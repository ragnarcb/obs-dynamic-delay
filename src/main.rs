mod chat;
mod clip;
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
use std::io::IsTerminal;
use std::io::Write;
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
use crate::status::{Bridge, Cmd, ObsAction, OutputStatus, Shared, Status};
use crate::upstream::{Dest, UpMsg};

pub enum EngineMsg {
    Media { session: u64, kind: Kind, ts: u32, data: Bytes, arrival: Instant },
    Metadata(StreamMetadata),
    PublishStart { session: u64, key: String },
    PublishEnd { session: u64 },
    Cmd(Cmd),
    /// The OBS script switched to the delay scene.
    SceneShown(Instant),
    SceneFailed,
    /// A scene went on air in OBS (for scene rules).
    ProgramScene(String),
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
    log::info!("v{} | config {} | delay {}s", env!("CARGO_PKG_VERSION"), path.display(), cfg.delay_seconds);
    // the OBS dock loads this file; keep it in sync with this version and token
    if let Err(e) = std::fs::write(path.with_file_name("dock.html"), control::dock_html(&cfg)) {
        log::warn!("could not write dock.html: {e}");
    }

    let shared = Arc::new(Shared {
        status: Mutex::new(Status::new(&cfg)),
        config: Mutex::new(cfg.clone()),
        config_path: path.clone(),
        bridge: Mutex::new(Bridge::default()),
    });
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(run_engine(cfg.clone(), rx, shared.clone()));
    tokio::spawn(control::serve_udp(udp, tx.clone(), shared.clone()));
    tokio::spawn(chat::run(shared.clone(), tx.clone()));
    let http = tokio::spawn(control::serve_http(tx.clone(), shared));

    tokio::select! {
        r = ingest::serve(cfg.listen.clone(), tx) => r?,
        r = http => r??,
        _ = tokio::signal::ctrl_c() => log::info!("bye"),
    }
    Ok(())
}

/// Destinations of a new stream: the main one plus the enabled extras.
fn destinations(cfg: &Config, obs_key: &str) -> Vec<Dest> {
    let main_key = if cfg.stream_key.is_empty() { obs_key.to_string() } else { cfg.stream_key.clone() };
    let mut v = vec![Dest { name: t!("Main", "Principal"), url: cfg.upstream_url.clone(), key: main_key }];
    for d in cfg.destinations.iter().filter(|d| d.enabled && !d.url.is_empty()) {
        v.push(Dest { name: d.name.clone(), url: d.url.clone(), key: d.key.clone() });
    }
    v
}

/// One sender per destination of the running stream.
struct Outputs(Vec<UnboundedSender<UpMsg>>);

impl Outputs {
    fn send(&self, msg: impl Fn() -> UpMsg) {
        for s in &self.0 {
            let _ = s.send(msg());
        }
    }
}

async fn run_engine(cfg: Config, mut rx: UnboundedReceiver<EngineMsg>, shared: Arc<Shared>) {
    let mut engine = Engine::new(cfg.filler_fps);
    let mut unwrapper = TsUnwrapper::new();
    let mut enabled = cfg.start_enabled;
    let mut delay = cfg.delay_seconds;
    let mut publishing: Option<u64> = None;
    let mut outputs: Option<Outputs> = None;
    let mut out = Vec::new();
    let mut ticks = 0u64;
    let mut quit = false;
    let mut panic = false;
    let mut last_program = String::new();
    let mut video_size = (1280u16, 720u16);
    let mut last_metadata: Option<StreamMetadata> = None;
    let mut stream_started: Option<Instant> = None;
    let mut in_bytes = 0usize;
    let mut in_meter = Instant::now();

    let target = |enabled: bool, delay: u32| Duration::from_secs(if enabled { delay as u64 } else { 0 });
    engine.set_target(target(enabled, delay));

    let mut tick = tokio::time::interval(Duration::from_millis(5));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            msg = rx.recv() => {
                let Some(msg) = msg else { return };
                // commands come from the panel, hotkeys, chat and scene rules
                let mut cmd = None;
                let mut force_on = false;
                match msg {
                    EngineMsg::Media { session, kind, ts, data, arrival } => {
                        if publishing == Some(session) {
                            in_bytes += data.len();
                            engine.push(kind, unwrapper.unwrap(session, ts), data, arrival);
                        }
                    }
                    EngineMsg::SceneShown(at) => engine.scene_shown(at),
                    EngineMsg::SceneFailed => engine.scene_unavailable(),
                    EngineMsg::Metadata(m) => {
                        if let (Some(w), Some(h)) = (m.video_width, m.video_height) {
                            video_size = (w as u16, h as u16);
                        }
                        if let Some(o) = &outputs {
                            o.send(|| UpMsg::Metadata(m.clone()));
                        }
                        last_metadata = Some(m);
                    }
                    EngineMsg::PublishStart { session, key } => {
                        publishing = Some(session);
                        engine.set_input_ended(false);
                        if outputs.is_none() {
                            let c = shared.config.lock().unwrap().clone();
                            if c.start_enabled {
                                enabled = true;
                                engine.set_target(target(enabled, delay));
                            }
                            let dests = destinations(&c, &key);
                            shared.status.lock().unwrap().outputs =
                                dests.iter().map(|d| OutputStatus::new(&d.name, &d.url)).collect();
                            let outage = Duration::from_secs(c.outage_buffer_seconds as u64);
                            let o = Outputs(
                                dests.into_iter().enumerate().map(|(i, d)| upstream::spawn(shared.clone(), i, d, outage)).collect(),
                            );
                            if let Some(m) = &last_metadata {
                                o.send(|| UpMsg::Metadata(m.clone()));
                            }
                            outputs = Some(o);
                            stream_started = Some(Instant::now());
                        }
                    }
                    EngineMsg::PublishEnd { session } => {
                        if publishing == Some(session) {
                            publishing = None;
                            engine.set_input_ended(true);
                        }
                    }
                    EngineMsg::ProgramScene(name) => {
                        if name != last_program {
                            last_program = name.clone();
                            if let Some((c, on)) = scene_rule(&shared, &name) {
                                cmd = Some(c);
                                force_on = on;
                            }
                        }
                    }
                    EngineMsg::Cmd(c) => cmd = Some(c),
                }
                let Some(cmd) = cmd else { continue };
                let now = Instant::now();
                let c = shared.config.lock().unwrap().clone();
                match cmd {
                    Cmd::On => enabled = true,
                    Cmd::Off => enabled = false,
                    Cmd::Toggle => enabled = !enabled,
                    Cmd::Set(s) => delay = s.min(c.max_delay_seconds),
                    Cmd::Add(d) => delay = (delay as i64 + d).clamp(0, c.max_delay_seconds as i64) as u32,
                    Cmd::Censor(secs) => {
                        censor(&mut engine, &shared, now, secs.unwrap_or(c.censor_seconds));
                        continue;
                    }
                    Cmd::Replay(secs) => {
                        let secs = secs.unwrap_or(c.replay_seconds).clamp(3, 60);
                        let busy = engine.is_adjusting();
                        let got = engine.replay(now, Duration::from_secs(secs as u64));
                        if busy {
                            shared.event("warn", t!("Wait: the delay is still being adjusted.", "Aguarde: o delay ainda está sendo ajustado."));
                        } else if got.is_zero() {
                            shared.event("warn", t!("Nothing to replay yet.", "Ainda não há nada para o replay."));
                        } else {
                            let s = got.as_secs_f64();
                            shared.event("ok", t!("Replaying the last {s:.0}s on air.", "Replay dos últimos {s:.0}s no ar."));
                        }
                        continue;
                    }
                    Cmd::Clip(secs) => {
                        let secs = secs.unwrap_or(c.clip_seconds).clamp(5, 120);
                        save_clip(&engine, &shared, now, secs, c.clips_path(), video_size);
                        continue;
                    }
                    Cmd::Panic => {
                        panic = !panic;
                        shared.status.lock().unwrap().panic = panic;
                        if panic {
                            shared.bridge.lock().unwrap().pending.push_back(ObsAction::Panic {
                                scene: c.panic_scene.clone(),
                                mute: c.panic_mute,
                            });
                            if c.panic_censor {
                                censor(&mut engine, &shared, now, c.censor_seconds);
                            }
                            shared.event("error", t!("PANIC: stream covered.", "PÂNICO: live protegida."));
                        } else {
                            shared.bridge.lock().unwrap().pending.push_back(ObsAction::Unpanic);
                            shared.event("ok", t!("Panic mode off.", "Modo pânico desligado."));
                        }
                        continue;
                    }
                    Cmd::CatchUp => {
                        if let Some(o) = &outputs {
                            o.send(|| UpMsg::CatchUp);
                        }
                        continue;
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
                if force_on {
                    enabled = true;
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
            _ = tick.tick() => {
                let now = Instant::now();
                let (mode, scene, history) = {
                    let c = shared.config.lock().unwrap();
                    let extra = c.replay_seconds.max(c.clip_seconds) as u64;
                    (GrowMode::parse(&c.grow_mode), c.delay_scene.clone(), Duration::from_secs(delay as u64 + extra + 5))
                };
                engine.set_grow_mode(mode, history);
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
                match &outputs {
                    Some(o) => {
                        for p in out.drain(..) {
                            o.send(|| UpMsg::Packet(p.clone()));
                        }
                    }
                    None => out.clear(),
                }
                if outputs.is_some() && publishing.is_none() && engine.is_idle() {
                    // OBS stopped and the delayed tail has been sent: end the stream.
                    if let Some(o) = outputs.take() {
                        o.send(|| UpMsg::Stop);
                    }
                    engine = Engine::new(cfg.filler_fps);
                    engine.set_target(target(enabled, delay));
                    unwrapper = TsUnwrapper::new();
                    stream_started = None;
                }
                if quit && outputs.is_none() {
                    log::info!("bye");
                    std::process::exit(0);
                }
                ticks += 1;
                if ticks.is_multiple_of(20) {
                    let mut st = shared.status.lock().unwrap();
                    st.enabled = enabled;
                    st.delay_seconds = delay;
                    st.obs_connected = publishing.is_some();
                    st.engine = outputs.is_some().then(|| engine.status(now));
                    let el = in_meter.elapsed();
                    if el >= Duration::from_secs(1) {
                        st.health.in_kbps = (in_bytes as u128 * 8 / el.as_millis().max(1)) as u32;
                        in_bytes = 0;
                        in_meter = Instant::now();
                    }
                    st.health.uptime_s = stream_started.map_or(0, |t| t.elapsed().as_secs());
                }
            }
        }
    }
}

fn censor(engine: &mut Engine, shared: &Shared, now: Instant, secs: u32) {
    let removed = engine.censor(now, Duration::from_secs(secs as u64));
    if removed.is_zero() {
        shared.event("warn", t!("Nothing to delete: turn the delay on first.", "Nada para apagar: ligue o delay antes."));
    } else {
        let s = removed.as_secs_f64();
        shared.event("ok", t!("Deleted the last {s:.1}s before they aired.", "Apagados os últimos {s:.1}s antes de irem ao ar."));
    }
}

fn save_clip(engine: &Engine, shared: &Arc<Shared>, now: Instant, secs: u32, dir: PathBuf, size: (u16, u16)) {
    let Some(clip) = engine.snapshot(now, Duration::from_secs(secs as u64)) else {
        shared.event("warn", t!("Nothing to clip yet.", "Ainda não há nada para o clipe."));
        return;
    };
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || match clip::save(&clip, &dir, size) {
        Ok(path) => {
            let p = path.display().to_string();
            shared.status.lock().unwrap().last_clip = Some(p.clone());
            shared.event("ok", t!("Clip saved: {p}", "Clipe salvo: {p}"));
        }
        Err(e) => shared.event("error", t!("Could not save the clip: {e:#}", "Não deu para salvar o clipe: {e:#}")),
    });
}

/// Command for a scene going on air, from the scene rules. The flag means
/// "also turn the delay on" (for "set:N").
fn scene_rule(shared: &Shared, scene: &str) -> Option<(Cmd, bool)> {
    let c = shared.config.lock().unwrap();
    if scene == c.delay_scene || scene == c.panic_scene {
        return None; // switched by us
    }
    let rule = c.scene_rules.iter().find(|r| r.scene == scene)?;
    let out = match rule.action.as_str() {
        "on" => (Cmd::On, false),
        "off" => (Cmd::Off, false),
        a => (Cmd::Set(a.strip_prefix("set:")?.parse().ok()?), true),
    };
    drop(c);
    shared.event("ok", t!("Scene \"{scene}\": delay rule applied.", "Cena \"{scene}\": regra de delay aplicada."));
    Some(out)
}
