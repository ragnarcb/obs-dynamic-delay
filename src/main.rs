mod config;
mod control;
mod engine;
mod flv;
mod ingest;
mod rtmp_io;
mod status;
mod upstream;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use rml_rtmp::sessions::StreamMetadata;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::Config;
use crate::engine::{Engine, TsUnwrapper};
use crate::flv::Kind;
use crate::status::{Cmd, Status, UpstreamState};
use crate::upstream::UpMsg;

pub enum EngineMsg {
    Media { session: u64, kind: Kind, ts: u32, data: Bytes, arrival: Instant },
    Metadata(StreamMetadata),
    PublishStart { session: u64, key: String },
    PublishEnd { session: u64 },
    Cmd(Cmd),
}

fn config_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.toml")))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let path = config_path();
    let cfg = Config::load_or_create(&path)?;
    log::info!("config {} | upstream {} | delay {}s", path.display(), cfg.upstream_url, cfg.delay_seconds);

    let status = Arc::new(Mutex::new(Status {
        enabled: cfg.start_enabled,
        delay_seconds: cfg.delay_seconds,
        max_delay_seconds: cfg.max_delay_seconds,
        obs_connected: false,
        upstream: UpstreamState::Idle,
        upstream_error: None,
        engine: None,
    }));
    let (tx, rx) = mpsc::unbounded_channel();
    let (up_tx, up_rx) = mpsc::unbounded_channel();

    tokio::spawn(upstream::run(cfg.upstream_url.clone(), cfg.stream_key.clone(), up_rx, status.clone()));
    tokio::spawn(run_engine(cfg.clone(), rx, up_tx, status.clone()));
    tokio::spawn(control::serve_udp(cfg.udp_listen.clone(), tx.clone()));
    let http = tokio::spawn(control::serve_http(cfg.http_listen.clone(), tx.clone(), status));

    tokio::select! {
        r = ingest::serve(cfg.listen.clone(), tx) => r?,
        r = http => r??,
        _ = tokio::signal::ctrl_c() => log::info!("bye"),
    }
    Ok(())
}

async fn run_engine(
    cfg: Config,
    mut rx: UnboundedReceiver<EngineMsg>,
    up_tx: UnboundedSender<UpMsg>,
    status: Arc<Mutex<Status>>,
) {
    let mut engine = Engine::new(cfg.filler_fps);
    let mut unwrapper = TsUnwrapper::new();
    let mut enabled = cfg.start_enabled;
    let mut delay = cfg.delay_seconds;
    let mut publishing: Option<u64> = None;
    let mut upstream_active = false;
    let mut out = Vec::new();
    let mut ticks = 0u64;

    let target = |enabled: bool, delay: u32| Duration::from_secs(if enabled { delay as u64 } else { 0 });
    engine.set_target(target(enabled, delay));

    let mut tick = tokio::time::interval(Duration::from_millis(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { return };
                match msg {
                    EngineMsg::Media { session, kind, ts, data, arrival } => {
                        if publishing == Some(session) {
                            engine.push(kind, unwrapper.unwrap(session, ts), data, arrival);
                        }
                    }
                    EngineMsg::Metadata(m) => {
                        let _ = up_tx.send(UpMsg::Metadata(m));
                    }
                    EngineMsg::PublishStart { session, key } => {
                        publishing = Some(session);
                        if !upstream_active {
                            upstream_active = true;
                            let _ = up_tx.send(UpMsg::Start { key });
                        }
                    }
                    EngineMsg::PublishEnd { session } => {
                        if publishing == Some(session) {
                            publishing = None;
                        }
                    }
                    EngineMsg::Cmd(cmd) => {
                        match cmd {
                            Cmd::On => enabled = true,
                            Cmd::Off => enabled = false,
                            Cmd::Toggle => enabled = !enabled,
                            Cmd::Set(s) => delay = s.min(cfg.max_delay_seconds),
                            Cmd::Add(d) => {
                                delay = (delay as i64 + d).clamp(0, cfg.max_delay_seconds as i64) as u32
                            }
                        }
                        log::info!("delay {} ({}s)", if enabled { "ON" } else { "OFF" }, delay);
                        engine.set_target(target(enabled, delay));
                    }
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                engine.poll(now, &mut out);
                for p in out.drain(..) {
                    let _ = up_tx.send(UpMsg::Packet(p));
                }
                if upstream_active && publishing.is_none() && engine.is_idle() {
                    // OBS stopped and the delayed tail has been sent: end the stream.
                    upstream_active = false;
                    let _ = up_tx.send(UpMsg::Stop);
                    engine = Engine::new(cfg.filler_fps);
                    engine.set_target(target(enabled, delay));
                    unwrapper = TsUnwrapper::new();
                }
                ticks += 1;
                if ticks.is_multiple_of(20) {
                    let mut st = status.lock().unwrap();
                    st.enabled = enabled;
                    st.delay_seconds = delay;
                    st.obs_connected = publishing.is_some();
                    st.engine = upstream_active.then(|| engine.status(now));
                }
            }
        }
    }
}
