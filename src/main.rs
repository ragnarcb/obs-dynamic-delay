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
mod update;
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
use crate::engine::{Engine, GrowMode, OutPacket, SceneEvent, TsUnwrapper};
use crate::flv::Kind;
use crate::status::{Bridge, Cmd, ObsAction, OutputStatus, Shared, Status, UpstreamState};
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
    /// The destinations changed in the settings: apply them to the running stream.
    DestinationsChanged,
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--lang en|pt` picks the language of the installer texts (the Setup passes the one chosen there)
    if let Some(l) = args.iter().position(|a| a == "--lang").and_then(|i| args.get(i + 1)) {
        i18n::set(i18n::Lang::parse(l));
    }
    // `--quiet`: no questions, errors as exit code (used by Dynamic-Delay-Setup.exe)
    let quiet = args.iter().any(|a| a == "--quiet");
    let arg = args.first().cloned();
    match arg.as_deref() {
        Some("--install") if quiet => std::process::exit(installer::quiet(installer::Mode::Install)),
        Some("--uninstall") if quiet => std::process::exit(installer::quiet(installer::Mode::Uninstall)),
        Some("--launch-obs") => {
            installer::launch_obs();
            return Ok(());
        }
        Some("--install") => return installer::wizard(installer::Mode::Install),
        Some("--uninstall") => return installer::wizard(installer::Mode::Uninstall),
        // double-clicked in Explorer: show the installer
        None if std::io::stdin().is_terminal() => return installer::start(),
        Some("--console") => return installer::wizard(installer::Mode::Ask),
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
        update_now: Default::default(),
    });
    let (tx, rx) = mpsc::unbounded_channel();

    tokio::spawn(run_engine(cfg.clone(), rx, shared.clone()));
    tokio::spawn(control::serve_udp(udp, tx.clone(), shared.clone()));
    tokio::spawn(chat::run(shared.clone(), tx.clone()));
    tokio::spawn(update::run(shared.clone()));
    let http = tokio::spawn(control::serve_http(tx.clone(), shared));

    tokio::select! {
        r = ingest::serve(cfg.listen.clone(), tx) => r?,
        r = http => r??,
        _ = tokio::signal::ctrl_c() => log::info!("bye"),
    }
    Ok(())
}

/// Destinations of the stream: the main one plus the enabled extras, each with
/// whether it goes live together with the stream.
fn destinations(cfg: &Config, obs_key: &str) -> Vec<(Dest, bool)> {
    let main_key = if cfg.stream_key.is_empty() { obs_key.to_string() } else { cfg.stream_key.clone() };
    let mut v = vec![(Dest { name: t!("Main", "Principal"), url: cfg.upstream_url.clone(), key: main_key }, true)];
    let extras = if cfg.features.multistream { cfg.destinations.as_slice() } else { &[] };
    for d in extras.iter().filter(|d| d.enabled && !d.url.is_empty()) {
        let name = if d.name.trim().is_empty() { d.url.clone() } else { d.name.clone() };
        v.push((Dest { name, url: d.url.clone(), key: d.key.clone() }, d.auto_start));
    }
    v
}

/// The destinations of the running stream. Each one can be started and stopped
/// while live, and the list follows the settings (added, removed, changed).
struct Outputs {
    slots: Vec<Slot>,
    next_id: u64,
    outage: Duration,
    /// Key OBS used, for a main destination without its own key.
    obs_key: String,
}

struct Slot {
    id: u64,
    dest: Dest,
    auto: bool,
    tx: Option<UnboundedSender<UpMsg>>,
}

impl Outputs {
    fn new(cfg: &Config, obs_key: &str) -> Self {
        let mut o = Outputs { slots: Vec::new(), next_id: 0, outage: Duration::from_secs(cfg.outage_seconds()), obs_key: obs_key.to_string() };
        for (dest, auto) in destinations(cfg, obs_key) {
            let id = o.next_id;
            o.next_id += 1;
            o.slots.push(Slot { id, dest, auto, tx: None });
        }
        o
    }

    /// Sends to every started destination.
    fn send(&self, msg: impl Fn() -> UpMsg) {
        for s in &self.slots {
            if let Some(tx) = &s.tx {
                let _ = tx.send(msg());
            }
        }
    }

    /// Starts slot `i`; `primer` gives a destination started mid-stream what it needs first.
    fn start(&mut self, i: usize, shared: &Arc<Shared>, primer: &[UpMsg]) {
        let s = &mut self.slots[i];
        if s.tx.is_some() {
            return;
        }
        let tx = upstream::spawn(shared.clone(), s.id, Dest { name: s.dest.name.clone(), url: s.dest.url.clone(), key: s.dest.key.clone() }, self.outage);
        for m in primer {
            let _ = tx.send(clone_msg(m));
        }
        log::info!("[{}] started", s.dest.name);
        s.tx = Some(tx);
    }

    fn stop(&mut self, i: usize) {
        if let Some(tx) = self.slots[i].tx.take() {
            let _ = tx.send(UpMsg::Stop);
            log::info!("[{}] stopped", self.slots[i].dest.name);
        }
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.slots.iter().position(|s| s.id == id)
    }

    /// Follows the settings while live: new destinations are added (and started
    /// when they start with the stream), removed ones stop, changed ones reconnect.
    fn reconcile(&mut self, cfg: &Config, shared: &Arc<Shared>, primer: &[UpMsg]) {
        self.outage = Duration::from_secs(cfg.outage_seconds());
        let wanted = destinations(cfg, &self.obs_key);
        let mut kept = Vec::new();
        for (dest, auto) in wanted {
            match self.slots.iter().position(|s| s.dest.name == dest.name) {
                Some(i) => {
                    let mut s = self.slots.remove(i);
                    let changed = s.dest.url != dest.url || s.dest.key != dest.key;
                    s.dest = dest;
                    s.auto = auto;
                    kept.push((s, changed, false));
                }
                None => {
                    let id = self.next_id;
                    self.next_id += 1;
                    kept.push((Slot { id, dest, auto, tx: None }, false, true));
                }
            }
        }
        // what is left was removed from the settings
        for i in 0..self.slots.len() {
            self.stop(i);
        }
        self.slots.clear();
        let mut restart = Vec::new();
        for (s, changed, new) in kept {
            let was_running = s.tx.is_some();
            let is_new = new && s.auto;
            self.slots.push(s);
            let i = self.slots.len() - 1;
            if changed && was_running {
                self.stop(i);
                restart.push(i);
            } else if is_new {
                restart.push(i);
            }
        }
        for i in restart {
            self.start(i, shared, primer);
        }
        self.sync_status(shared);
    }

    /// Rewrites the status list in slot order, keeping each destination's counters.
    fn sync_status(&self, shared: &Shared) {
        let mut st = shared.status.lock().unwrap();
        let old = std::mem::take(&mut st.outputs);
        st.outputs = self
            .slots
            .iter()
            .map(|s| {
                let mut o = old.iter().find(|o| o.id == s.id).cloned().unwrap_or_else(|| OutputStatus::new(s.id, &s.dest.name, &s.dest.url));
                o.name = s.dest.name.clone();
                o.running = s.tx.is_some();
                o.auto_start = s.auto;
                if !o.running {
                    o.state = UpstreamState::Idle;
                    o.kbps = 0;
                    o.behind_ms = 0;
                }
                o
            })
            .collect();
    }
}

fn clone_msg(m: &UpMsg) -> UpMsg {
    match m {
        UpMsg::Metadata(x) => UpMsg::Metadata(x.clone()),
        UpMsg::Packet(p) => UpMsg::Packet(p.clone()),
        UpMsg::CatchUp => UpMsg::CatchUp,
        UpMsg::Stop => UpMsg::Stop,
    }
}

/// What a destination started in the middle of the stream needs before the media:
/// the stream metadata and the current codec headers.
fn primer(engine: &Engine, metadata: &Option<StreamMetadata>, ts: u32) -> Vec<UpMsg> {
    let mut v = Vec::new();
    if let Some(m) = metadata {
        v.push(UpMsg::Metadata(m.clone()));
    }
    if let Some(h) = engine.video_header() {
        v.push(UpMsg::Packet(OutPacket { kind: Kind::Video, ts, data: h.clone() }));
    }
    if let Some(h) = engine.audio_header() {
        v.push(UpMsg::Packet(OutPacket { kind: Kind::Audio, ts, data: h.clone() }));
    }
    v
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
    let mut in_frames = 0u32;
    let mut in_meter = Instant::now();
    // newest output timestamp, where a destination started mid-stream begins
    let mut last_out_ts = 0u32;

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
                            if kind == Kind::Video && flv::is_video_frame(&data) {
                                in_frames += 1;
                            }
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
                            let mut o = Outputs::new(&c, &key);
                            let first: Vec<UpMsg> = last_metadata.iter().map(|m| UpMsg::Metadata(m.clone())).collect();
                            for i in 0..o.slots.len() {
                                if o.slots[i].auto {
                                    o.start(i, &shared, &first);
                                }
                            }
                            o.sync_status(&shared);
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
                    EngineMsg::DestinationsChanged => {
                        if let Some(o) = &mut outputs {
                            let c = shared.config.lock().unwrap().clone();
                            let p = primer(&engine, &last_metadata, last_out_ts);
                            o.reconcile(&c, &shared, &p);
                        }
                        continue;
                    }
                }
                let Some(cmd) = cmd else { continue };
                let now = Instant::now();
                let c = shared.config.lock().unwrap().clone();
                if let Some(name) = disabled_feature(&c, cmd, panic) {
                    shared.event("warn", t!("\"{name}\" is turned off (Features and panel).", "\"{name}\" está desativado (Recursos e painel)."));
                    continue;
                }
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
                        let fps = last_metadata.as_ref().and_then(|m| m.video_frame_rate).map(f64::from);
                        let size = stream_size(&engine, video_size);
                        save_clip(&engine, &shared, now, secs, c.clip_aired_only, c.clips_path(), size, fps);
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
                    Cmd::OutputStart(id) | Cmd::OutputStop(id) | Cmd::OutputToggle(id) => {
                        let Some(o) = &mut outputs else {
                            shared.event("warn", t!("Start the stream in OBS first.", "Inicie a live no OBS primeiro."));
                            continue;
                        };
                        let Some(i) = o.index(id) else { continue };
                        let start = match cmd {
                            Cmd::OutputStart(_) => true,
                            Cmd::OutputStop(_) => false,
                            _ => o.slots[i].tx.is_none(),
                        };
                        let name = o.slots[i].dest.name.clone();
                        if start {
                            let p = primer(&engine, &last_metadata, last_out_ts);
                            o.start(i, &shared, &p);
                            shared.event("ok", t!("{name}: going live.", "{name}: entrando ao vivo."));
                        } else {
                            o.stop(i);
                            shared.event("ok", t!("{name}: stopped.", "{name}: parado."));
                        }
                        o.sync_status(&shared);
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
                    (GrowMode::parse(&c.grow_mode), c.delay_scene.clone(), Duration::from_secs(c.history_seconds(delay)))
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
                            last_out_ts = last_out_ts.max(p.ts);
                            o.send(|| UpMsg::Packet(p.clone()));
                        }
                    }
                    None => out.clear(),
                }
                if outputs.is_some() && publishing.is_none() && engine.is_idle() {
                    // OBS stopped and the delayed tail has been sent: end the stream.
                    if let Some(mut o) = outputs.take() {
                        for i in 0..o.slots.len() {
                            o.stop(i);
                        }
                    }
                    last_out_ts = 0;
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
                        st.health.in_fps = if publishing.is_some() {
                            snap_fps(in_frames as f64 * 1000.0 / el.as_millis().max(1) as f64) as f32
                        } else {
                            0.0
                        };
                        let (w, h) = stream_size(&engine, video_size);
                        (st.health.width, st.health.height) = if publishing.is_some() { (w as u32, h as u32) } else { (0, 0) };
                        in_bytes = 0;
                        in_frames = 0;
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

/// A frame count over about a second jitters (29.8, 60.2...): show the usual rate it is close to.
fn snap_fps(measured: f64) -> f64 {
    [24.0, 25.0, 30.0, 48.0, 50.0, 60.0, 100.0, 120.0, 144.0]
        .into_iter()
        .find(|r| (measured - r).abs() / r < 0.04)
        .unwrap_or((measured * 10.0).round() / 10.0)
}

/// Picture size: from the H.264 header when it can be read, else from OBS' metadata.
fn stream_size(engine: &Engine, metadata: (u16, u16)) -> (u16, u16) {
    engine.video_header().and_then(|h| clip::avc_size(h)).unwrap_or(metadata)
}

#[allow(clippy::too_many_arguments)]
fn save_clip(
    engine: &Engine,
    shared: &Arc<Shared>,
    now: Instant,
    secs: u32,
    aired_only: bool,
    dir: PathBuf,
    size: (u16, u16),
    fps: Option<f64>,
) {
    let Some(clip) = engine.snapshot(now, Duration::from_secs(secs as u64), aired_only) else {
        shared.event("warn", t!("Nothing to clip yet.", "Ainda não há nada para o clipe."));
        return;
    };
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || match clip::save(&clip, &dir, size, fps) {
        Ok(path) => {
            let p = path.display().to_string();
            shared.status.lock().unwrap().last_clip = Some(p.clone());
            shared.event("ok", t!("Clip saved: {p}", "Clipe salvo: {p}"));
        }
        Err(e) => shared.event("error", t!("Could not save the clip: {e:#}", "Não deu para salvar o clipe: {e:#}")),
    });
}

/// Name of the feature a command needs, when that feature is turned off.
/// Ending a running panic is always allowed.
fn disabled_feature(c: &Config, cmd: Cmd, panic_on: bool) -> Option<String> {
    let f = &c.features;
    let off = match cmd {
        Cmd::Censor(_) => !f.censor,
        Cmd::Replay(_) => !f.replay,
        Cmd::Clip(_) => !f.clips,
        Cmd::Panic => !f.panic && !panic_on,
        Cmd::CatchUp => !f.outage,
        _ => false,
    };
    off.then(|| match cmd {
        Cmd::Censor(_) => t!("Delete before it airs", "Apagar antes de ir ao ar"),
        Cmd::Replay(_) => t!("Instant replay", "Replay instantâneo"),
        Cmd::Clip(_) => t!("Clips", "Clipes"),
        Cmd::Panic => t!("Panic button", "Botão de pânico"),
        _ => t!("Connection drop protection", "Proteção contra queda"),
    })
}

/// Command for a scene going on air, from the scene rules. The flag means
/// "also turn the delay on" (for "set:N").
fn scene_rule(shared: &Shared, scene: &str) -> Option<(Cmd, bool)> {
    let c = shared.config.lock().unwrap();
    if !c.features.rules || scene == c.delay_scene || scene == c.panic_scene {
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
