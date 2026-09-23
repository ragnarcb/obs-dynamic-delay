//! RTMP(S) clients that publish the delayed stream to the platforms. One
//! [`session`] runs per destination (multistream), each with its own buffer:
//! when a connection drops, what could not be sent is kept (up to
//! `outage_buffer_seconds`) and sent after reconnecting, in real time, so
//! viewers miss nothing; the destination then runs that much behind until
//! [`UpMsg::CatchUp`]. The same buffer bounds memory on a slow network.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rml_rtmp::handshake::PeerType;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType, StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use url::Url;

use crate::engine::OutPacket;
use crate::flv::{self, Kind};
use crate::rtmp_io::{self, Io};
use crate::status::{Shared, UpstreamState};

pub enum UpMsg {
    Metadata(StreamMetadata),
    Packet(OutPacket),
    /// Drop what is buffered and jump to the newest keyframe.
    CatchUp,
    Stop,
}

pub struct Dest {
    pub name: String,
    pub url: String,
    pub key: String,
}

/// Starts publishing to `dest`; the session ends on [`UpMsg::Stop`].
pub fn spawn(shared: Arc<Shared>, index: usize, dest: Dest, outage_buffer: Duration) -> UnboundedSender<UpMsg> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(session(shared, index, dest, rx, outage_buffer));
    tx
}

/// Media waiting to be sent to one destination.
struct Output {
    pending: VecDeque<OutPacket>,
    metadata: Option<StreamMetadata>,
    video_header: Option<OutPacket>,
    audio_header: Option<OutPacket>,
    /// Longest span kept while disconnected or while the network is too slow.
    limit_ms: u32,
    /// Added to every timestamp after a catch-up, so the timeline stays continuous.
    shift: i64,
    last_ts: Option<u32>,
    /// Real-time pacing anchor (wall clock, timestamp) while sending a backlog.
    pacing: Option<(Instant, u32)>,
    need_key: bool,
}

impl Output {
    fn new(limit: Duration) -> Self {
        Output {
            pending: VecDeque::new(),
            metadata: None,
            video_header: None,
            audio_header: None,
            limit_ms: limit.as_millis() as u32,
            shift: 0,
            last_ts: None,
            pacing: None,
            need_key: true,
        }
    }

    fn push(&mut self, p: OutPacket) {
        // decoder configs are sent in order and also kept for the next reconnect
        match p.kind {
            Kind::Video if flv::video_is_sequence_header(&p.data) => self.video_header = Some(p.clone()),
            Kind::Audio if flv::audio_is_sequence_header(&p.data) => self.audio_header = Some(p.clone()),
            _ => {}
        }
        self.pending.push_back(p);
    }

    fn span_ms(&self) -> u32 {
        match (self.pending.front(), self.pending.back()) {
            (Some(a), Some(b)) => b.ts.saturating_sub(a.ts),
            _ => 0,
        }
    }

    /// Keeps at most `limit_ms`, dropping whole GOPs from the front.
    fn trim(&mut self) {
        while self.span_ms() > self.limit_ms {
            self.pending.pop_front();
            while let Some(p) = self.pending.front() {
                if p.kind == Kind::Video && flv::video_is_keyframe(&p.data) {
                    break;
                }
                self.pending.pop_front();
            }
        }
    }

    /// Drops the backlog up to the newest keyframe and keeps the timeline continuous.
    fn catch_up(&mut self) -> bool {
        let Some(k) = self
            .pending
            .iter()
            .rposition(|p| p.kind == Kind::Video && flv::video_is_keyframe(&p.data))
        else {
            return false;
        };
        if k == 0 {
            return false;
        }
        self.pending.drain(..k);
        if let (Some(last), Some(head)) = (self.last_ts, self.pending.front()) {
            self.shift = (last as i64 + 50) - head.ts as i64;
        }
        self.pacing = self.pending.front().map(|h| (Instant::now(), h.ts));
        true
    }

    /// Next packet to send, if it is due. Also tells when the next one will be due.
    fn next_due(&mut self, now: Instant) -> (Option<OutPacket>, Option<Instant>) {
        let Some(head) = self.pending.front() else {
            self.pacing = None;
            return (None, None);
        };
        if let Some((wall, ts)) = self.pacing {
            let due = wall + Duration::from_millis(head.ts.saturating_sub(ts) as u64);
            if due > now {
                return (None, Some(due));
            }
        }
        (self.pending.pop_front(), None)
    }

    fn out_ts(&mut self, ts: u32) -> u32 {
        let mut t = (ts as i64 + self.shift).max(0) as u32;
        if let Some(last) = self.last_ts {
            t = t.max(last);
        }
        self.last_ts = Some(t);
        t
    }
}

enum Ended {
    Stopped,
    ChannelClosed,
}

type Conn = (ClientSession, ReadHalf<Box<dyn Io>>, WriteHalf<Box<dyn Io>>);

async fn session(shared: Arc<Shared>, index: usize, dest: Dest, mut rx: UnboundedReceiver<UpMsg>, outage: Duration) {
    let set = |f: &dyn Fn(&mut crate::status::OutputStatus)| {
        let mut st = shared.status.lock().unwrap();
        if let Some(o) = st.outputs.get_mut(index) {
            f(o);
        }
        // the main destination also drives the legacy single-output fields
        if index == 0 {
            let (state, err) = st.outputs.first().map(|o| (o.state, o.error.clone())).unwrap_or((UpstreamState::Idle, None));
            st.upstream = state;
            st.upstream_error = err;
        }
    };
    // while disconnected keep the configured backlog; while connected allow a bit of slack
    let mut out = Output::new(outage);
    let connected_limit = outage.max(Duration::from_secs(20));
    loop {
        set(&|o| o.state = UpstreamState::Connecting);
        match connect(&dest.url, &dest.key).await {
            Ok(conn) => {
                log::info!("[{}] publishing to {}", dest.name, dest.url);
                set(&|o| {
                    o.state = UpstreamState::Connected;
                    o.error = None;
                });
                out.limit_ms = connected_limit.as_millis() as u32;
                let r = pump(conn, &mut rx, &mut out, &shared, index, &dest.name).await;
                out.limit_ms = outage.as_millis() as u32;
                match r {
                    Ok(Ended::Stopped) => break,
                    Ok(Ended::ChannelClosed) => return,
                    Err(e) => {
                        log::warn!("[{}] connection lost: {e:#}", dest.name);
                        let msg = format!("{e:#}");
                        set(&|o| {
                            o.state = UpstreamState::Reconnecting;
                            o.error = Some(msg.clone());
                            o.reconnects += 1;
                            o.kbps = 0;
                        });
                    }
                }
            }
            Err(e) => {
                log::warn!("[{}] connect failed: {e:#}", dest.name);
                let msg = format!("{e:#}");
                set(&|o| {
                    o.state = UpstreamState::Reconnecting;
                    o.error = Some(msg.clone());
                });
            }
        }
        // keep buffering while waiting to reconnect
        let wait = tokio::time::sleep(Duration::from_secs(3));
        tokio::pin!(wait);
        loop {
            tokio::select! {
                _ = &mut wait => break,
                m = rx.recv() => match m {
                    Some(UpMsg::Stop) => {
                        set(&|o| o.state = UpstreamState::Idle);
                        log::info!("[{}] stream finished", dest.name);
                        return;
                    }
                    Some(UpMsg::Packet(p)) => { out.push(p); out.trim(); }
                    Some(UpMsg::Metadata(m)) => out.metadata = Some(m),
                    Some(UpMsg::CatchUp) => { out.catch_up(); }
                    None => return,
                }
            }
        }
        let behind = out.span_ms();
        set(&|o| o.behind_ms = behind);
        if !out.pending.is_empty() {
            // resume the backlog in real time after reconnecting
            out.pacing = out.pending.front().map(|h| (Instant::now() + Duration::from_secs(1), h.ts));
        }
        out.need_key = true;
    }
    set(&|o| o.state = UpstreamState::Idle);
    log::info!("[{}] stream finished", dest.name);
}

async fn write_results<W: AsyncWriteExt + Unpin>(w: &mut W, results: Vec<ClientSessionResult>) -> Result<Vec<ClientSessionEvent>> {
    let mut events = Vec::new();
    for r in results {
        match r {
            ClientSessionResult::OutboundResponse(p) => w.write_all(&p.bytes).await?,
            ClientSessionResult::RaisedEvent(e) => events.push(e),
            ClientSessionResult::UnhandleableMessageReceived(_) => {}
        }
    }
    Ok(events)
}

async fn connect(raw_url: &str, key: &str) -> Result<Conn> {
    let url = Url::parse(raw_url).with_context(|| format!("invalid upstream_url {raw_url}"))?;
    let tls = match url.scheme() {
        "rtmp" => false,
        "rtmps" => true,
        s => bail!("unsupported scheme {s} (use rtmp:// or rtmps://)"),
    };
    let host = url.host_str().context("upstream_url has no host")?.to_string();
    let port = url.port().unwrap_or(if tls { 443 } else { 1935 });
    let mut app = url.path().trim_matches('/').to_string();
    if let Some(q) = url.query() {
        app = format!("{app}?{q}");
    }
    let tc_url = raw_url.trim_end_matches('/').to_string();

    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host.as_str(), port)))
        .await
        .context("timeout connecting")??;
    tcp.set_nodelay(true)?;
    let mut stream: Box<dyn Io> = if tls {
        let connector = tokio_native_tls::TlsConnector::from(tokio_native_tls::native_tls::TlsConnector::new()?);
        Box::new(connector.connect(&host, tcp).await.context("TLS handshake")?)
    } else {
        Box::new(tcp)
    };

    tokio::time::timeout(Duration::from_secs(15), async move {
        let leftover = rtmp_io::handshake(&mut stream, PeerType::Client).await?;
        let mut config = ClientSessionConfig::new();
        config.tc_url = Some(tc_url);
        let (mut session, initial) = ClientSession::new(config)?;
        let mut events = write_results(&mut stream, initial).await?;
        let req = session.request_connection(app)?;
        events.extend(write_results(&mut stream, vec![req]).await?);
        let res = session.handle_input(&leftover)?;
        events.extend(write_results(&mut stream, res).await?);

        let mut buf = vec![0u8; 16 * 1024];
        let mut connected = false;
        loop {
            for ev in events.drain(..) {
                match ev {
                    ClientSessionEvent::ConnectionRequestAccepted if !connected => {
                        connected = true;
                        let req = session.request_publishing(key.to_string(), PublishRequestType::Live)?;
                        write_results(&mut stream, vec![req]).await?;
                    }
                    ClientSessionEvent::ConnectionRequestRejected { description } => {
                        bail!("server rejected connection: {description}")
                    }
                    ClientSessionEvent::PublishRequestAccepted => {
                        let (r, w) = tokio::io::split(stream);
                        return Ok((session, r, w));
                    }
                    ClientSessionEvent::UnhandleableOnStatusCode { code } => {
                        bail!("server refused to publish: {code} (check the stream key)")
                    }
                    _ => {}
                }
            }
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                bail!("server closed the connection (check the stream key)");
            }
            let res = session.handle_input(&buf[..n])?;
            events = write_results(&mut stream, res).await?;
        }
    })
    .await
    .context("timeout during RTMP negotiation")?
}

async fn pump(
    conn: Conn,
    rx: &mut UnboundedReceiver<UpMsg>,
    out: &mut Output,
    shared: &Shared,
    index: usize,
    name: &str,
) -> Result<Ended> {
    let (mut session, mut reader, mut writer) = conn;

    if let Some(m) = &out.metadata {
        let r = session.publish_metadata(m)?;
        write_results(&mut writer, vec![r]).await?;
    }
    for h in [out.video_header.clone(), out.audio_header.clone()].into_iter().flatten() {
        let ts = out.last_ts.unwrap_or(h.ts);
        send_packet(&mut session, &mut writer, &h, ts).await?;
    }

    let mut buf = vec![0u8; 16 * 1024];
    let mut sent_bytes = 0usize;
    let mut meter = Instant::now();
    loop {
        // send everything that is due
        let wake = loop {
            let (p, wake) = out.next_due(Instant::now());
            let Some(p) = p else { break wake };
            if p.kind == Kind::Video && out.need_key && !flv::video_is_sequence_header(&p.data) {
                if !flv::video_is_keyframe(&p.data) {
                    continue; // after a (re)connect video restarts at a keyframe
                }
                out.need_key = false;
            }
            let ts = out.out_ts(p.ts);
            sent_bytes += p.data.len();
            send_packet(&mut session, &mut writer, &p, ts).await?;
        };
        if meter.elapsed() >= Duration::from_secs(1) {
            let kbps = (sent_bytes * 8 / 1000) as u32 * 1000 / meter.elapsed().as_millis().max(1) as u32;
            let behind = out.span_ms();
            if let Some(o) = shared.status.lock().unwrap().outputs.get_mut(index) {
                o.kbps = kbps;
                o.behind_ms = if out.pacing.is_some() { behind } else { 0 };
            }
            sent_bytes = 0;
            meter = Instant::now();
        }
        let sleep = tokio::time::sleep_until(wake.unwrap_or_else(|| Instant::now() + Duration::from_millis(500)).into());
        tokio::select! {
            n = reader.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    bail!("server closed the connection");
                }
                let res = session.handle_input(&buf[..n])?;
                for ev in write_results(&mut writer, res).await? {
                    if let ClientSessionEvent::UnhandleableOnStatusCode { code } = ev {
                        log::warn!("[{name}] server status: {code}");
                    }
                }
            }
            msg = rx.recv() => match msg {
                None => return Ok(Ended::ChannelClosed),
                Some(UpMsg::Stop) => {
                    // flush what is still buffered, then end the stream
                    out.pacing = None;
                    while let (Some(p), _) = out.next_due(Instant::now()) {
                        let ts = out.out_ts(p.ts);
                        send_packet(&mut session, &mut writer, &p, ts).await?;
                    }
                    if let Ok(r) = session.stop_publishing() {
                        let _ = write_results(&mut writer, r).await;
                    }
                    let _ = writer.flush().await;
                    let _ = writer.shutdown().await;
                    return Ok(Ended::Stopped);
                }
                Some(UpMsg::Metadata(m)) => {
                    let r = session.publish_metadata(&m)?;
                    write_results(&mut writer, vec![r]).await?;
                    out.metadata = Some(m);
                }
                Some(UpMsg::CatchUp) => {
                    if out.catch_up() {
                        log::info!("[{name}] caught up with the live delay");
                    }
                }
                Some(UpMsg::Packet(p)) => {
                    out.push(p);
                    if out.span_ms() > out.limit_ms {
                        // network too slow for the bitrate: skip ahead instead of growing without bound
                        log::warn!("[{name}] network too slow, skipping ahead");
                        out.catch_up();
                        out.trim();
                    }
                }
            },
            _ = sleep => {}
        }
    }
}

async fn send_packet(
    session: &mut ClientSession,
    writer: &mut WriteHalf<Box<dyn Io>>,
    p: &OutPacket,
    ts: u32,
) -> Result<()> {
    let ts = RtmpTimestamp::new(ts);
    let r = match p.kind {
        Kind::Video => session.publish_video_data(p.data.clone(), ts, false)?,
        Kind::Audio => session.publish_audio_data(p.data.clone(), ts, false)?,
    };
    write_results(writer, vec![r]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn pkt(kind: Kind, ts: u32, key: bool) -> OutPacket {
        let data = match (kind, key) {
            (Kind::Video, true) => vec![0x17, 1, 0, 0, 0],
            (Kind::Video, false) => vec![0x27, 1, 0, 0, 0],
            (Kind::Audio, _) => vec![0xAF, 1, 0x21],
        };
        OutPacket { kind, ts, data: Bytes::from(data) }
    }

    fn feed(o: &mut Output, from: u32, to: u32) {
        let mut t = from;
        while t < to {
            o.push(pkt(Kind::Video, t, t % 2000 == 0));
            o.push(pkt(Kind::Audio, t + 5, false));
            o.trim();
            t += 40;
        }
    }

    #[test]
    fn backlog_is_bounded_and_starts_at_a_keyframe() {
        let mut o = Output::new(Duration::from_secs(10));
        feed(&mut o, 0, 30_000);
        assert!(o.span_ms() <= 10_000, "{}", o.span_ms());
        let head = o.pending.front().unwrap();
        assert!(flv::video_is_keyframe(&head.data));
    }

    #[test]
    fn paced_backlog_waits_for_its_time() {
        let mut o = Output::new(Duration::from_secs(10));
        feed(&mut o, 0, 4_000);
        let t0 = Instant::now();
        o.pacing = Some((t0, 0));
        let (first, _) = o.next_due(t0);
        assert!(first.is_some());
        // a packet 1 s later in the timeline is not due yet
        while let (Some(_), _) = o.next_due(t0) {}
        let (none, wake) = o.next_due(t0);
        assert!(none.is_none());
        assert!(wake.unwrap() > t0);
    }

    #[test]
    fn catch_up_keeps_timeline_continuous() {
        let mut o = Output::new(Duration::from_secs(30));
        feed(&mut o, 0, 1_000);
        o.pacing = None;
        while let (Some(p), _) = o.next_due(Instant::now()) {
            o.out_ts(p.ts);
        }
        let last = o.last_ts.unwrap();
        feed(&mut o, 1_000, 9_000);
        assert!(o.catch_up());
        let head = o.pending.front().unwrap().clone();
        assert_eq!(head.ts, 8_000);
        let t = o.out_ts(head.ts);
        assert!(t > last && t <= last + 100, "{t} vs {last}");
    }
}
