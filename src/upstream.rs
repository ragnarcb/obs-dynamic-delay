//! RTMP(S) clients that publish the delayed stream to the platforms. One
//! [`session`] runs per destination (multistream), each with its own buffer:
//! when a connection drops, what could not be sent is kept (up to
//! `outage_buffer_seconds`) and sent after reconnecting, in real time, so
//! viewers miss nothing; the destination then runs that much behind until
//! [`UpMsg::CatchUp`]. The same buffer bounds memory on a slow network.

use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rml_rtmp::handshake::PeerType;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType, StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt, BufWriter, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc::error::TryRecvError;
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

/// Starts publishing to `dest`; the session ends on [`UpMsg::Stop`]. `id` is the
/// destination's entry in the status (0 is the main one).
pub fn spawn(shared: Arc<Shared>, id: u64, dest: Dest, outage_buffer: Duration) -> UnboundedSender<UpMsg> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(session(shared, id, dest, rx, outage_buffer));
    tx
}

/// Media waiting to be sent to one destination.
struct Output {
    pending: VecDeque<OutPacket>,
    metadata: Option<StreamMetadata>,
    video_header: Option<OutPacket>,
    audio_header: Option<OutPacket>,
    /// Headers of the extra audio tracks (the Twitch VOD track), by track id.
    extra_headers: Vec<(u8, OutPacket)>,
    /// Whether this destination gets the extra audio tracks. Only Twitch takes Enhanced
    /// RTMP multitrack audio: Kick and the others drop the connection on it.
    vod_track: bool,
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
            extra_headers: Vec::new(),
            vod_track: true,
            limit_ms: limit.as_millis() as u32,
            shift: 0,
            last_ts: None,
            pacing: None,
            need_key: true,
        }
    }

    fn push(&mut self, p: OutPacket) {
        if !self.vod_track && p.kind == Kind::Audio && flv::audio_track(&p.data) > 0 {
            return;
        }
        // decoder configs are sent in order and also kept for the next reconnect
        match p.kind {
            Kind::Video if flv::video_is_sequence_header(&p.data) => self.video_header = Some(p.clone()),
            Kind::Audio if flv::audio_is_sequence_header(&p.data) => match flv::audio_track(&p.data) {
                0 => self.audio_header = Some(p.clone()),
                t => match self.extra_headers.iter_mut().find(|(k, _)| *k == t) {
                    Some(h) => h.1 = p.clone(),
                    None => self.extra_headers.push((t, p.clone())),
                },
            },
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
        if self.last_ts.is_none() {
            // a destination started in the middle of the stream still starts at 0
            self.shift = -(ts as i64);
        }
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

async fn session(shared: Arc<Shared>, id: u64, dest: Dest, mut rx: UnboundedReceiver<UpMsg>, outage: Duration) {
    let set = |f: &dyn Fn(&mut crate::status::OutputStatus)| {
        let mut st = shared.status.lock().unwrap();
        if let Some(o) = st.outputs.iter_mut().find(|o| o.id == id) {
            f(o);
        }
        // the main destination also drives the legacy single-output fields
        if id == 0 {
            let (state, err) =
                st.outputs.iter().find(|o| o.id == 0).map(|o| (o.state, o.error.clone())).unwrap_or((UpstreamState::Idle, None));
            st.upstream = state;
            st.upstream_error = err;
        }
    };
    // while disconnected keep the configured backlog; while connected allow a bit of slack
    let mut out = Output::new(outage);
    out.vod_track = takes_vod_track(&dest.url);
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
                let r = pump(conn, &mut rx, &mut out, &shared, id, &dest.name).await;
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

/// The Twitch VOD track (Enhanced RTMP multitrack audio) only goes to Twitch ingests. Kick
/// also runs on `*.contribute.live-video.net` (IVS), under a 12 hex digit host, and rejects it.
fn takes_vod_track(url: &str) -> bool {
    let Some(host) = Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_ascii_lowercase)) else { return false };
    let first = host.split('.').next().unwrap_or("");
    let kick = (host.ends_with(".global-contribute.live-video.net") && first.len() == 12 && first.bytes().all(|b| b.is_ascii_hexdigit()))
        || host == "kick.com"
        || host.ends_with(".kick.com");
    !kick && (host == "twitch.tv" || host.ends_with(".twitch.tv") || host.ends_with("contribute.live-video.net"))
}

/// A write that makes no progress for this long means the connection is dead even
/// if the OS has not noticed yet (which can take minutes): reconnect instead.
const WRITE_STALL: Duration = Duration::from_secs(15);

async fn stalled<T>(write: impl Future<Output = std::io::Result<T>>) -> Result<T> {
    Ok(tokio::time::timeout(WRITE_STALL, write).await.context("network stalled (nothing sent for 15 s)")??)
}

async fn write_results<W: AsyncWrite + Unpin>(
    w: &mut W,
    results: impl IntoIterator<Item = ClientSessionResult>,
) -> Result<Vec<ClientSessionEvent>> {
    let mut events = Vec::new();
    for r in results {
        match r {
            ClientSessionResult::OutboundResponse(p) => stalled(w.write_all(&p.bytes)).await?,
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
    let stream: Box<dyn Io> = if tls {
        let connector = tokio_native_tls::TlsConnector::from(tokio_native_tls::native_tls::TlsConnector::new()?);
        Box::new(connector.connect(&host, tcp).await.context("TLS handshake")?)
    } else {
        Box::new(tcp)
    };
    negotiate(stream, app, tc_url, key).await
}

/// RTMP handshake, connect and publish request on an open stream.
async fn negotiate(mut stream: Box<dyn Io>, app: String, tc_url: String, key: &str) -> Result<Conn> {
    tokio::time::timeout(Duration::from_secs(15), async move {
        let leftover = rtmp_io::handshake(&mut stream, PeerType::Client).await?;
        let mut config = ClientSessionConfig::new();
        config.tc_url = Some(tc_url);
        let (mut session, initial) = ClientSession::new(config)?;
        let mut events = write_results(&mut stream, initial).await?;
        let req = session.request_connection(app)?;
        events.extend(write_results(&mut stream, [req]).await?);
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
                        write_results(&mut stream, [req]).await?;
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
    id: u64,
    name: &str,
) -> Result<Ended> {
    let (mut session, mut reader, writer) = conn;
    // packets sent back to back (a backlog, audio + video of one tick) go out in
    // fewer TCP segments / TLS records; flushed before every wait
    let mut writer = BufWriter::with_capacity(64 * 1024, writer);

    if let Some(m) = &out.metadata {
        let r = session.publish_metadata(m)?;
        write_results(&mut writer, [r]).await?;
    }

    let mut buf = vec![0u8; 16 * 1024];
    let mut sent_bytes = 0usize;
    let mut meter = Instant::now();
    loop {
        // send everything that is due
        let wake = loop {
            // take in what the engine sent meanwhile first: while the network is slower
            // than the stream, the backlog must build up in `out`, which bounds it, not
            // in the channel
            if let Some(ended) = drain(rx, &mut session, &mut writer, out, name).await? {
                return Ok(ended);
            }
            let (p, wake) = out.next_due(Instant::now());
            let Some(p) = p else { break wake };
            if out.need_key {
                // after a (re)connect, or when started mid-stream, everything starts together at a
                // keyframe: codec headers, then the keyframe, then audio (no audio-only lead)
                if p.kind != Kind::Video || !flv::video_is_keyframe(&p.data) || flv::video_is_sequence_header(&p.data) {
                    continue; // headers are kept in `out` and sent with the keyframe
                }
                out.need_key = false;
                let ts = out.out_ts(p.ts);
                let extra = out.extra_headers.iter().map(|(_, h)| h.clone());
                for h in [out.video_header.clone(), out.audio_header.clone()].into_iter().flatten().chain(extra) {
                    send_packet(&mut session, &mut writer, &h, ts).await?;
                }
                sent_bytes += p.data.len();
                trace(name, &p, ts);
                send_packet(&mut session, &mut writer, &p, ts).await?;
                continue;
            }
            let ts = out.out_ts(p.ts);
            sent_bytes += p.data.len();
            trace(name, &p, ts);
            send_packet(&mut session, &mut writer, &p, ts).await?;
        };
        stalled(writer.flush()).await?;
        if meter.elapsed() >= Duration::from_secs(1) {
            let kbps = (sent_bytes as u64 * 8 / meter.elapsed().as_millis().max(1) as u64) as u32;
            let behind = out.span_ms();
            if let Some(o) = shared.status.lock().unwrap().outputs.iter_mut().find(|o| o.id == id) {
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
            msg = rx.recv() => {
                if let Some(ended) = on_msg(msg, &mut session, &mut writer, out, name).await? {
                    return Ok(ended);
                }
            }
            _ = sleep => {}
        }
    }
}

/// Handles every message already waiting in `rx`, without blocking.
async fn drain<W: AsyncWrite + Unpin>(
    rx: &mut UnboundedReceiver<UpMsg>,
    session: &mut ClientSession,
    writer: &mut W,
    out: &mut Output,
    name: &str,
) -> Result<Option<Ended>> {
    loop {
        let msg = match rx.try_recv() {
            Ok(m) => Some(m),
            Err(TryRecvError::Empty) => return Ok(None),
            Err(TryRecvError::Disconnected) => None,
        };
        if let Some(ended) = on_msg(msg, session, writer, out, name).await? {
            return Ok(Some(ended));
        }
    }
}

/// Handles one message from the engine. Returns how the session ended, if it did.
async fn on_msg<W: AsyncWrite + Unpin>(
    msg: Option<UpMsg>,
    session: &mut ClientSession,
    writer: &mut W,
    out: &mut Output,
    name: &str,
) -> Result<Option<Ended>> {
    match msg {
        None => return Ok(Some(Ended::ChannelClosed)),
        Some(UpMsg::Stop) => {
            // flush what is still buffered, then end the stream
            out.pacing = None;
            while let (Some(p), _) = out.next_due(Instant::now()) {
                let ts = out.out_ts(p.ts);
                send_packet(session, writer, &p, ts).await?;
            }
            if let Ok(r) = session.stop_publishing() {
                let _ = write_results(writer, r).await;
            }
            let _ = stalled(writer.flush()).await;
            let _ = stalled(writer.shutdown()).await;
            return Ok(Some(Ended::Stopped));
        }
        Some(UpMsg::Metadata(m)) => {
            let r = session.publish_metadata(&m)?;
            write_results(writer, [r]).await?;
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
    }
    Ok(None)
}

/// `DD_TRACE=<file>`: one line per packet sent (destination, wall ms, kind, timestamp,
/// bytes, keyframe), to study the pacing seen by the platforms.
fn trace(name: &str, p: &OutPacket, ts: u32) {
    static T: OnceLock<Option<(Instant, Mutex<std::fs::File>)>> = OnceLock::new();
    let t = T.get_or_init(|| {
        let path = std::env::var_os("DD_TRACE")?;
        Some((Instant::now(), Mutex::new(std::fs::File::create(path).ok()?)))
    });
    if let Some((t0, f)) = t {
        let key = p.kind == Kind::Video && flv::video_is_keyframe(&p.data);
        let _ = writeln!(f.lock().unwrap(), "{name}\t{}\t{:?}\t{ts}\t{}\t{}", t0.elapsed().as_millis(), p.kind, p.data.len(), key as u8);
    }
}

async fn send_packet<W: AsyncWrite + Unpin>(
    session: &mut ClientSession,
    writer: &mut W,
    p: &OutPacket,
    ts: u32,
) -> Result<()> {
    let ts = RtmpTimestamp::new(ts);
    let r = match p.kind {
        Kind::Video => session.publish_video_data(p.data.clone(), ts, false)?,
        Kind::Audio => session.publish_audio_data(p.data.clone(), ts, false)?,
    };
    write_results(writer, [r]).await?;
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
            o.push(pkt(Kind::Video, t, t.is_multiple_of(2000)));
            o.push(pkt(Kind::Audio, t + 5, false));
            o.trim();
            t += 40;
        }
    }

    #[test]
    fn vod_track_only_to_twitch() {
        for u in ["rtmp://live.twitch.tv/app", "rtmps://ingest.global-contribute.live-video.net/app", "rtmp://sao03.contribute.live-video.net/app"] {
            assert!(takes_vod_track(u), "{u}");
        }
        for u in [
            "rtmps://fa723fc1b171.global-contribute.live-video.net:443/app",
            "rtmps://FA723FC1B171.global-contribute.live-video.net/app",
            "rtmp://a.rtmp.youtube.com/live2",
            "rtmps://live-api-s.facebook.com:443/rtmp/",
            "not a url",
        ] {
            assert!(!takes_vod_track(u), "{u}");
        }
        // Kick never sees a multitrack packet, not even its header
        let mut o = Output::new(Duration::from_secs(10));
        o.vod_track = false;
        o.push(OutPacket { kind: Kind::Audio, ts: 0, data: Bytes::from_static(&[0x95, 0x00, b'm', b'p', b'4', b'a', 1, 0x11, 0x90]) });
        o.push(OutPacket { kind: Kind::Audio, ts: 0, data: Bytes::from_static(&[0x95, 0x01, b'm', b'p', b'4', b'a', 1, 0x21]) });
        o.push(OutPacket { kind: Kind::Audio, ts: 0, data: Bytes::from_static(&[0xAF, 0x00, 0x11, 0x90]) });
        assert!(o.extra_headers.is_empty());
        assert_eq!(o.pending.len(), 1);
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

    /// Accepts one publisher on `sock` and reads at most `rate` bytes/s once it
    /// publishes (a slow uplink). Returns how many video frames arrived.
    async fn slow_server(mut sock: tokio::io::DuplexStream, rate: u64) -> usize {
        use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult};
        let leftover = rtmp_io::handshake(&mut sock, PeerType::Server).await.unwrap();
        let (mut session, initial) = ServerSession::new(ServerSessionConfig::new()).unwrap();
        let mut results: VecDeque<ServerSessionResult> = initial.into();
        results.extend(session.handle_input(&leftover).unwrap());
        let (mut buf, mut frames, mut read) = (vec![0u8; 16 * 1024], 0, None::<(Instant, u64)>);
        loop {
            while let Some(r) = results.pop_front() {
                match r {
                    ServerSessionResult::OutboundResponse(p) => sock.write_all(&p.bytes).await.unwrap(),
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::ConnectionRequested { request_id, .. }) => {
                        results.extend(session.accept_request(request_id).unwrap())
                    }
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::PublishStreamRequested { request_id, .. }) => {
                        read = Some((Instant::now(), 0));
                        results.extend(session.accept_request(request_id).unwrap())
                    }
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::VideoDataReceived { .. }) => frames += 1,
                    _ => {}
                }
            }
            // token bucket: independent of the timer resolution of the OS
            let mut max = buf.len();
            if let Some((t0, total)) = read {
                let allowed = (rate * t0.elapsed().as_millis() as u64 / 1000).saturating_sub(total) as usize;
                if allowed < 4096 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    continue;
                }
                max = max.min(allowed);
            }
            match sock.read(&mut buf[..max]).await {
                Ok(0) | Err(_) => return frames,
                Ok(n) => {
                    if let Some((_, total)) = &mut read {
                        *total += n as u64;
                    }
                    results.extend(session.handle_input(&buf[..n]).unwrap())
                }
            }
        }
    }

    /// Records the first bytes of every media message a platform receives.
    async fn recording_server(mut sock: tokio::io::DuplexStream) -> Vec<(Kind, Vec<u8>)> {
        use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult};
        let leftover = rtmp_io::handshake(&mut sock, PeerType::Server).await.unwrap();
        let (mut session, initial) = ServerSession::new(ServerSessionConfig::new()).unwrap();
        let mut results: VecDeque<ServerSessionResult> = initial.into();
        results.extend(session.handle_input(&leftover).unwrap());
        let (mut buf, mut got) = (vec![0u8; 16 * 1024], Vec::new());
        loop {
            while let Some(r) = results.pop_front() {
                match r {
                    ServerSessionResult::OutboundResponse(p) => sock.write_all(&p.bytes).await.unwrap(),
                    ServerSessionResult::RaisedEvent(
                        ServerSessionEvent::ConnectionRequested { request_id, .. } | ServerSessionEvent::PublishStreamRequested { request_id, .. },
                    ) => results.extend(session.accept_request(request_id).unwrap()),
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::VideoDataReceived { data, .. }) => got.push((Kind::Video, data.to_vec())),
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::AudioDataReceived { data, .. }) => got.push((Kind::Audio, data.to_vec())),
                    _ => {}
                }
            }
            match sock.read(&mut buf).await {
                Ok(0) | Err(_) => return got,
                Ok(n) => results.extend(session.handle_input(&buf[..n]).unwrap()),
            }
        }
    }

    /// A destination that starts at a keyframe gets the VOD track's header with the others,
    /// and the VOD audio from before that keyframe is left out like the main audio.
    #[tokio::test]
    async fn vod_track_reaches_the_platform() {
        let (client, server) = tokio::io::duplex(256 * 1024);
        let server = tokio::spawn(recording_server(server));
        let conn = negotiate(Box::new(client), "app".into(), "rtmp://test/app".into(), "key").await.unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cfg = crate::config::Config::default();
        let shared = Shared {
            status: Mutex::new(crate::status::Status::new(&cfg)),
            config: Mutex::new(cfg),
            config_path: Default::default(),
            bridge: Default::default(),
            update_now: Default::default(),
            events: tokio::sync::broadcast::channel(16).0,
        };
        let mt_start = vec![0x95, 0x00, b'm', b'p', b'4', b'a', 1, 0x11, 0x90];
        let mt = |n: u8| vec![0x95, 0x01, b'm', b'p', b'4', b'a', 1, 0x21, n];
        let send = |kind, ts, d: Vec<u8>| tx.send(UpMsg::Packet(OutPacket { kind, ts, data: Bytes::from(d) })).unwrap();
        send(Kind::Video, 0, vec![0x17, 0, 0, 0, 0]);
        send(Kind::Audio, 0, vec![0xAF, 0, 0x11, 0x90]);
        send(Kind::Audio, 0, mt_start.clone());
        send(Kind::Audio, 5, mt(1)); // before the keyframe: dropped
        send(Kind::Video, 10, vec![0x17, 1, 0, 0, 0]);
        send(Kind::Audio, 21, vec![0xAF, 1, 0x21]);
        send(Kind::Audio, 20, mt(2)); // 1 ms behind the main audio
        send(Kind::Video, 43, vec![0x27, 1, 0, 0, 0]);
        // the stream goes on for a moment (a Stop right away flushes whatever is left)
        let stop = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            tx.send(UpMsg::Stop).unwrap();
        });
        let mut out = Output::new(Duration::from_secs(5));
        pump(conn, &mut rx, &mut out, &shared, 0, "test").await.unwrap();
        stop.await.unwrap();
        let got = server.await.unwrap();
        let heads: Vec<(Kind, Vec<u8>)> = got.iter().map(|(k, d)| (*k, d.clone())).collect();
        let want = vec![
            (Kind::Video, vec![0x17, 0, 0, 0, 0]),
            (Kind::Audio, vec![0xAF, 0, 0x11, 0x90]),
            (Kind::Audio, mt_start),
            (Kind::Video, vec![0x17, 1, 0, 0, 0]),
            (Kind::Audio, vec![0xAF, 1, 0x21]),
            (Kind::Audio, mt(2)),
            (Kind::Video, vec![0x27, 1, 0, 0, 0]),
        ];
        assert_eq!(heads, want);
    }

    /// The network is slower than the stream: the backlog must be dropped (skip
    /// ahead) instead of piling up without bound in the channel from the engine.
    #[tokio::test]
    async fn slow_network_backlog_is_bounded() {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(slow_server(server, 200_000));
        let conn = negotiate(Box::new(client), "app".into(), "rtmp://test/app".into(), "key").await.unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cfg = crate::config::Config::default();
        let shared = Shared {
            status: Mutex::new(crate::status::Status::new(&cfg)),
            config: Mutex::new(cfg),
            config_path: Default::default(),
            bridge: Default::default(),
            update_now: Default::default(),
            events: tokio::sync::broadcast::channel(16).0,
        };
        let mut out = Output::new(Duration::from_secs(1));
        // 5 s of a 5 Mbps stream (20 KB frames at 30 fps), produced 2-4x faster than real time
        let sent = 150u32;
        let producer = tokio::spawn(async move {
            for i in 0..sent {
                let mut data = vec![if i.is_multiple_of(30) { 0x17 } else { 0x27 }, 1, 0, 0, 0];
                data.resize(20_000, 0);
                let _ = tx.send(UpMsg::Packet(OutPacket { kind: Kind::Video, ts: i * 33, data: Bytes::from(data) }));
                tokio::time::sleep(Duration::from_millis(8)).await;
            }
            let _ = tx.send(UpMsg::Stop);
        });
        let t0 = Instant::now();
        let ended = pump(conn, &mut rx, &mut out, &shared, 0, "test").await.unwrap();
        let took = t0.elapsed();
        producer.await.unwrap();
        let got = server.await.unwrap();
        assert!(matches!(ended, Ended::Stopped));
        eprintln!("slow network: {got}/{sent} frames delivered in {took:?}");
        // 3 MB cannot go through 200 KB/s in the few seconds the input lasts: most of it is skipped
        assert!(got < sent as usize / 2, "{got} of {sent} frames sent, backlog was not bounded");
        assert!(took < Duration::from_secs(8), "took {took:?}");
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
