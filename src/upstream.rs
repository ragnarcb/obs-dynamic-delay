//! RTMP(S) client that publishes the delayed stream to the real platform.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use bytes::Bytes;
use rml_rtmp::handshake::PeerType;
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
    PublishRequestType, StreamMetadata,
};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc::UnboundedReceiver;
use url::Url;

use crate::engine::OutPacket;
use crate::flv::{self, Kind};
use crate::rtmp_io::{self, Io};
use crate::status::{Status, UpstreamState};

pub enum UpMsg {
    Start { key: String },
    Metadata(StreamMetadata),
    Packet(OutPacket),
    Stop,
}

/// Remembers what a freshly (re)connected upstream needs before media.
#[derive(Default)]
struct Cache {
    metadata: Option<StreamMetadata>,
    video_header: Option<OutPacket>,
    audio_header: Option<OutPacket>,
}

impl Cache {
    fn observe(&mut self, p: &OutPacket) {
        match p.kind {
            Kind::Video if flv::video_is_sequence_header(&p.data) => {
                self.video_header = Some(p.clone())
            }
            Kind::Audio if flv::audio_is_sequence_header(&p.data) => {
                self.audio_header = Some(p.clone())
            }
            _ => {}
        }
    }
}

enum Ended {
    Stopped,
    ChannelClosed,
}

type Conn = (ClientSession, ReadHalf<Box<dyn Io>>, WriteHalf<Box<dyn Io>>);

pub async fn run(url: String, fixed_key: String, mut rx: UnboundedReceiver<UpMsg>, status: Arc<Mutex<Status>>) {
    let set_state = |s: UpstreamState, err: Option<String>| {
        let mut st = status.lock().unwrap();
        st.upstream = s;
        if err.is_some() {
            st.upstream_error = err;
        }
    };
    let mut cache = Cache::default();
    loop {
        set_state(UpstreamState::Idle, None);
        let key = loop {
            match rx.recv().await {
                Some(UpMsg::Start { key }) => break key,
                Some(UpMsg::Metadata(m)) => cache.metadata = Some(m),
                Some(UpMsg::Packet(p)) => cache.observe(&p),
                Some(UpMsg::Stop) => {}
                None => return,
            }
        };
        let key = if fixed_key.is_empty() { key } else { fixed_key.clone() };

        'session: loop {
            set_state(UpstreamState::Connecting, None);
            match connect(&url, &key).await {
                Ok(conn) => {
                    log::info!("[upstream] publishing to {url}");
                    set_state(UpstreamState::Connected, None);
                    status.lock().unwrap().upstream_error = None;
                    match pump(conn, &mut rx, &mut cache).await {
                        Ok(Ended::Stopped) => break 'session,
                        Ok(Ended::ChannelClosed) => return,
                        Err(e) => {
                            log::warn!("[upstream] connection lost: {e:#}");
                            set_state(UpstreamState::Reconnecting, Some(format!("{e:#}")));
                        }
                    }
                }
                Err(e) => {
                    log::warn!("[upstream] connect failed: {e:#}");
                    set_state(UpstreamState::Reconnecting, Some(format!("{e:#}")));
                }
            }
            // Keep consuming the output while waiting, so the delay buffer keeps moving.
            let wait = tokio::time::sleep(Duration::from_secs(3));
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    m = rx.recv() => match m {
                        Some(UpMsg::Stop) => break 'session,
                        Some(UpMsg::Packet(p)) => cache.observe(&p),
                        Some(UpMsg::Metadata(m)) => cache.metadata = Some(m),
                        Some(UpMsg::Start { .. }) => {}
                        None => return,
                    }
                }
            }
        }
        log::info!("[upstream] stream finished");
    }
}

async fn write_results<W: AsyncWriteExt + Unpin>(
    w: &mut W,
    results: Vec<ClientSessionResult>,
) -> Result<Vec<ClientSessionEvent>> {
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

async fn pump(conn: Conn, rx: &mut UnboundedReceiver<UpMsg>, cache: &mut Cache) -> Result<Ended> {
    let (mut session, mut reader, mut writer) = conn;

    if let Some(m) = &cache.metadata {
        let r = session.publish_metadata(m)?;
        write_results(&mut writer, vec![r]).await?;
    }
    for h in [cache.video_header.clone(), cache.audio_header.clone()].into_iter().flatten() {
        send_packet(&mut session, &mut writer, h).await?;
    }
    // After a reconnect video must restart at a keyframe.
    let mut need_key = true;

    let mut buf = vec![0u8; 16 * 1024];
    loop {
        tokio::select! {
            n = reader.read(&mut buf) => {
                let n = n?;
                if n == 0 {
                    bail!("server closed the connection");
                }
                let res = session.handle_input(&buf[..n])?;
                for ev in write_results(&mut writer, res).await? {
                    if let ClientSessionEvent::UnhandleableOnStatusCode { code } = ev {
                        log::warn!("[upstream] server status: {code}");
                    }
                }
            }
            msg = rx.recv() => match msg {
                None => return Ok(Ended::ChannelClosed),
                Some(UpMsg::Stop) => {
                    if let Ok(r) = session.stop_publishing() {
                        let _ = write_results(&mut writer, r).await;
                    }
                    let _ = writer.flush().await;
                    let _ = writer.shutdown().await;
                    return Ok(Ended::Stopped);
                }
                Some(UpMsg::Start { .. }) => {}
                Some(UpMsg::Metadata(m)) => {
                    let r = session.publish_metadata(&m)?;
                    write_results(&mut writer, vec![r]).await?;
                    cache.metadata = Some(m);
                }
                Some(UpMsg::Packet(p)) => {
                    cache.observe(&p);
                    if p.kind == Kind::Video && need_key {
                        if flv::video_is_keyframe(&p.data) {
                            need_key = false;
                        } else if !flv::video_is_sequence_header(&p.data) {
                            continue;
                        }
                    }
                    send_packet(&mut session, &mut writer, p).await?;
                }
            }
        }
    }
}

async fn send_packet(
    session: &mut ClientSession,
    writer: &mut WriteHalf<Box<dyn Io>>,
    p: OutPacket,
) -> Result<()> {
    let ts = RtmpTimestamp::new(p.ts);
    let data: Bytes = p.data;
    let r = match p.kind {
        Kind::Video => session.publish_video_data(data, ts, false)?,
        Kind::Audio => session.publish_audio_data(data, ts, false)?,
    };
    write_results(writer, vec![r]).await?;
    Ok(())
}
