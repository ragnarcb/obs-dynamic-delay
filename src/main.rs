mod config;
mod control;
mod engine;
mod flv;
mod i18n;
mod ingest;
mod installer;
mod rtmp_io;
mod status;
mod upstream;

use std::fs::File;
use std::io::Write;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use rml_rtmp::sessions::StreamMetadata;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::config::Config;
use crate::engine::{Engine, GrowMode, SceneEvent, TsUnwrapper};
use crate::flv::Kind;
use crate::status::{Bridge, Cmd, ObsAction, Shared, Status, UpstreamState};
use crate::upstream::UpMsg;

pub enum EngineMsg {
    Media { session: u64, kind: Kind, ts: u32, data: Bytes, arrival: Instant },
    Metadata(StreamMetadata),
    PublishStart { session: u64, key: String },
    PublishEnd { session: u64 },
    Cmd(Cmd),
    /// The OBS script switched to the delay scene.
    SceneShown(Instant),
    SceneFailed,
}

fn config_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1).filter(|a| !a.starts_with("--")) {
        return PathBuf::from(arg);
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.toml")))
        .unwrap_or_else(|| PathBuf::from("config.toml"))
}

/// Writes log lines to stderr and to a log file next to the config.
struct Tee(File);

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let _ = std::io::stderr().write_all(buf);
        self.0.write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

fn init_logging(config: &Path) {
    let mut b = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"));
    let log_path = config.with_file_name("obs-dynamic-delay.log");
    if let Ok(f) = File::create(&log_path) {
        b.target(env_logger::Target::Pipe(Box::new(Tee(f))));
    }
    b.init();
}

fn main() -> Result<()> {
    let arg = std::env::args().nth(1);
    match arg.as_deref() {
        Some("--install") => return installer::wizard(installer::Mode::Install),
        Some("--uninstall") => return installer::wizard(installer::Mode::Uninstall),
        // double-clicked in Explorer: show the installer
        None if std::io::stdin().is_terminal() => return installer::wizard(installer::Mode::Ask),
        _ => {}
    }
    tokio::runtime::Runtime::new()?.block_on(relay())
}

async fn relay() -> Result<()> {
    let path = config_path();
    let cfg = Config::load_or_create(&path)?;
    i18n::set(i18n::Lang::parse(&cfg.language));
    // The UDP port doubles as a single-instance lock: bind it before touching the log file.
    let udp = match std::net::UdpSocket::bind(&cfg.udp_listen) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "{}",
                t!(
                    "obs-dynamic-delay is already running (port {} in use): {e}",
                    "obs-dynamic-delay ja esta rodando (porta {} ocupada): {e}",
                    cfg.udp_listen
                )
            );
            std::process::exit(2);
        }
    };
    udp.set_nonblocking(true)?;
    let udp = tokio::net::UdpSocket::from_std(udp)?;
    init_logging(&path);
    log::info!("config {} | upstream {} | delay {}s", path.display(), cfg.upstream_url, cfg.delay_seconds);

    let shared = Arc::new(Shared {
        status: Mutex::new(Status {
            enabled: cfg.start_enabled,
            delay_seconds: cfg.delay_seconds,
            max_delay_seconds: cfg.max_delay_seconds,
            obs_connected: false,
            upstream: UpstreamState::Idle,
            upstream_error: None,
            engine: None,
        }),
        config: Mutex::new(cfg.clone()),
        config_path: path.clone(),
        bridge: Mutex::new(Bridge::default()),
    });
    let (tx, rx) = mpsc::unbounded_channel();
    let (up_tx, up_rx) = mpsc::unbounded_channel();

    tokio::spawn(upstream::run(shared.clone(), up_rx));
    tokio::spawn(run_engine(cfg.clone(), rx, up_tx, shared.clone()));
    tokio::spawn(control::serve_udp(udp, tx.clone(), shared.clone()));
    let http = tokio::spawn(control::serve_http(cfg.http_listen.clone(), tx.clone(), shared));

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
    shared: Arc<Shared>,
) {
    let mut engine = Engine::new(cfg.filler_fps);
    let mut unwrapper = TsUnwrapper::new();
    let mut enabled = cfg.start_enabled;
    let mut delay = cfg.delay_seconds;
    let mut publishing: Option<u64> = None;
    let mut upstream_active = false;
    let mut out = Vec::new();
    let mut ticks = 0u64;
    let mut quit = false;

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
                    EngineMsg::SceneShown(at) => engine.scene_shown(at),
                    EngineMsg::SceneFailed => engine.scene_unavailable(),
                    EngineMsg::Metadata(m) => {
                        let _ = up_tx.send(UpMsg::Metadata(m));
                    }
                    EngineMsg::PublishStart { session, key } => {
                        publishing = Some(session);
                        if !upstream_active {
                            upstream_active = true;
                            if shared.config.lock().unwrap().start_enabled {
                                enabled = true;
                                engine.set_target(target(enabled, delay));
                            }
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
                            Cmd::Quit => {
                                log::info!("quit requested, exiting when no stream is active");
                                quit = true;
                                continue;
                            }
                            Cmd::Stay => {
                                quit = false;
                                continue;
                            }
                        }
                        log::info!("delay {} ({}s)", if enabled { "ON" } else { "OFF" }, delay);
                        if matches!(cmd, Cmd::Set(_) | Cmd::Add(_)) {
                            let changed = {
                                let mut c = shared.config.lock().unwrap();
                                std::mem::replace(&mut c.delay_seconds, delay) != delay
                            };
                            if changed {
                                shared.save_config();
                            }
                        }
                        engine.set_target(target(enabled, delay));
                    }
                }
            }
            _ = tick.tick() => {
                let now = Instant::now();
                let (mode, scene) = {
                    let c = shared.config.lock().unwrap();
                    (GrowMode::parse(&c.grow_mode), c.delay_scene.clone())
                };
                // keep enough history to rewind the configured delay
                engine.set_grow_mode(mode, Duration::from_secs(delay as u64 + 3));
                engine.poll(now, &mut out);
                for ev in engine.take_scene_events() {
                    let mut b = shared.bridge.lock().unwrap();
                    match ev {
                        SceneEvent::Show if b.info().script && !scene.is_empty() => {
                            b.pending.push_back(ObsAction::ShowScene(scene.clone()))
                        }
                        SceneEvent::Show => {
                            drop(b);
                            engine.scene_unavailable();
                        }
                        SceneEvent::Back => b.pending.push_back(ObsAction::SceneBack),
                    }
                }
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
                if quit && !upstream_active {
                    log::info!("bye");
                    std::process::exit(0);
                }
                ticks += 1;
                if ticks.is_multiple_of(20) {
                    let mut st = shared.status.lock().unwrap();
                    st.enabled = enabled;
                    st.delay_seconds = delay;
                    st.obs_connected = publishing.is_some();
                    st.engine = upstream_active.then(|| engine.status(now));
                }
            }
        }
    }
}
