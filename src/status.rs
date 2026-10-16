use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::config::Config;
use crate::engine::{EngineStatus, Phase};
use crate::t;

/// State shared between the engine, the outputs, the HTTP API and the UDP port.
pub struct Shared {
    pub status: Mutex<Status>,
    pub config: Mutex<Config>,
    pub config_path: PathBuf,
    pub bridge: Mutex<Bridge>,
    /// The panel asked for an update check now.
    pub update_now: std::sync::atomic::AtomicBool,
    /// Push events for bots (`/api/events`).
    pub events: tokio::sync::broadcast::Sender<PushEvent>,
}

impl Shared {
    pub fn save_config(&self) {
        let cfg = self.config.lock().unwrap().clone();
        if let Err(e) = cfg.save(&self.config_path) {
            log::warn!("{e:#}");
        }
    }

    /// Shows a short message in the panel.
    pub fn event(&self, kind: &str, text: String) {
        let mut st = self.status.lock().unwrap();
        let id = st.event.as_ref().map_or(1, |e| e.id + 1);
        log::info!("{text}");
        st.event = Some(Event { id, kind: kind.to_string(), text });
    }

    /// Sends a push event (`{"type": ..., ...}`) to the `/api/events` listeners.
    pub fn push(&self, mut ev: Value) {
        let kind = ev["type"].as_str().unwrap_or("event").to_string();
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
        if let Some(o) = ev.as_object_mut() {
            o.insert("ts".into(), json!(ts));
        }
        // no listener is not an error
        let _ = self.events.send(PushEvent { kind, json: ev.to_string() });
    }
}

/// One push event: its type (the SSE event name) and the JSON body.
#[derive(Clone, Debug)]
pub struct PushEvent {
    pub kind: String,
    pub json: String,
}

/// Events for bots, found by comparing two status snapshots. Censor and the
/// auto-off firing are sent where they happen (they leave no trace in the status).
pub fn status_events(prev: &Status, cur: &Status) -> Vec<Value> {
    let mut v = Vec::new();
    if prev.enabled != cur.enabled {
        v.push(json!({ "type": if cur.enabled { "delay_on" } else { "delay_off" }, "delay_seconds": cur.delay_seconds }));
    }
    if prev.delay_seconds != cur.delay_seconds {
        v.push(json!({ "type": "delay_seconds", "seconds": cur.delay_seconds, "previous": prev.delay_seconds }));
    }
    let phase = |s: &Status| s.engine.as_ref().map_or(json!("offline"), |e| json!(e.phase));
    let (was, is) = (phase(prev), phase(cur));
    if was != is {
        v.push(json!({ "type": "phase", "phase": is, "previous": was }));
    }
    let replaying = |s: &Status| s.engine.as_ref().is_some_and(|e| e.replaying);
    if replaying(prev) != replaying(cur) {
        v.push(json!({ "type": if replaying(cur) { "replay_start" } else { "replay_end" } }));
    }
    if prev.panic != cur.panic {
        v.push(json!({ "type": if cur.panic { "panic_on" } else { "panic_off" } }));
    }
    if let Some(path) = cur.last_clip.as_ref().filter(|p| prev.last_clip.as_ref() != Some(*p)) {
        v.push(json!({ "type": "clip_saved", "path": path }));
    }
    for o in &cur.outputs {
        let old = prev.outputs.iter().find(|p| p.id == o.id);
        if old.is_none_or(|p| p.state != o.state || p.running != o.running) {
            v.push(json!({
                "type": "destination", "id": o.id, "name": o.name, "running": o.running, "state": o.state,
                "error": if o.state == UpstreamState::Connected { None } else { o.error.clone() },
            }));
        }
    }
    if prev.auto_off_s.is_some() != cur.auto_off_s.is_some() || prev.auto_off_minutes != cur.auto_off_minutes {
        v.push(json!({ "type": "auto_off_timer", "minutes": cur.auto_off_minutes, "seconds_left": cur.auto_off_s }));
    }
    v
}

/// "Turn the delay off after N minutes". The countdown runs while the delay is on:
/// armed while it is off, it starts when the delay is turned on. Turning the delay
/// off (by hand or by the timer) clears it.
#[derive(Debug, Default)]
pub struct AutoOff {
    minutes: u32,
    deadline: Option<Instant>,
}

impl AutoOff {
    /// Longest timer: one day.
    pub const MAX_MINUTES: u32 = 24 * 60;

    /// Arms the timer (0 = cancel).
    pub fn set(&mut self, minutes: u32, enabled: bool, now: Instant) {
        self.minutes = minutes.min(Self::MAX_MINUTES);
        self.deadline = (self.minutes > 0 && enabled).then(|| now + Duration::from_secs(self.minutes as u64 * 60));
    }

    /// The delay was switched: the countdown starts with it and ends when it goes off.
    pub fn delay_changed(&mut self, was: bool, is: bool, now: Instant) {
        if was && !is {
            self.set(0, false, now);
        } else if !was && is && self.minutes > 0 && self.deadline.is_none() {
            self.set(self.minutes, true, now);
        }
    }

    /// True once when the time is up (the timer is then cleared).
    pub fn due(&mut self, now: Instant) -> bool {
        let due = self.deadline.is_some_and(|d| now >= d);
        if due {
            self.set(0, false, now);
        }
        due
    }

    pub fn minutes(&self) -> u32 {
        self.minutes
    }

    /// Seconds left while counting down.
    pub fn seconds_left(&self, now: Instant) -> Option<u64> {
        self.deadline.map(|d| d.saturating_duration_since(now).as_secs_f64().ceil() as u64)
    }
}

/// Actions the relay asks the OBS script to perform inside OBS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObsAction {
    Configure,
    Restore,
    ShowScene(String),
    SceneBack,
    Panic { scene: String, mute: bool },
    Unpanic,
    /// Phone deck keys.
    Scene(String),
    ToggleMute(String),
    ToggleStream,
    ToggleRecord,
    /// Sets the OBS frame rate (Settings > Video > Common FPS values).
    SetFps(u32),
    /// Adds the on-screen widget (Browser Source with this URL) to the current scene.
    AddOverlay(String),
}

impl ObsAction {
    /// Wire format of the poll reply.
    pub fn encode(&self) -> String {
        match self {
            ObsAction::Configure => "configure".into(),
            ObsAction::Restore => "restore".into(),
            ObsAction::ShowScene(name) => format!("scene_show\t{name}"),
            ObsAction::SceneBack => "scene_back".into(),
            ObsAction::Panic { scene, mute } => format!("panic\t{}\t{scene}", if *mute { 1 } else { 0 }),
            ObsAction::Unpanic => "unpanic".into(),
            ObsAction::Scene(name) => format!("scene\t{name}"),
            ObsAction::ToggleMute(name) => format!("mute\t{name}"),
            ObsAction::ToggleStream => "stream_toggle".into(),
            ObsAction::ToggleRecord => "record_toggle".into(),
            ObsAction::SetFps(n) => format!("fps\t{n}"),
            ObsAction::AddOverlay(url) => format!("overlay\t{url}"),
        }
    }
}

/// Link with the OBS script, which polls the relay over UDP.
#[derive(Default)]
pub struct Bridge {
    pub pending: VecDeque<ObsAction>,
    pub last_poll: Option<Instant>,
    pub obs_configured: bool,
    pub message: Option<(Instant, String)>,
    /// The last message reports a failure.
    pub message_error: bool,
    /// Counts the messages, so the panel shows the same text again when it repeats.
    pub message_id: u64,
    pub scenes: Vec<String>,
    pub program_scene: String,
    pub audio: Vec<AudioSource>,
    pub streaming: bool,
    pub recording: bool,
    pub video: Option<ObsVideo>,
}

/// OBS video settings (output size and frame rate), reported by the script.
#[derive(Clone, Copy, Debug, Serialize, PartialEq)]
pub struct ObsVideo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

/// An OBS audio source and whether it is muted.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AudioSource {
    pub name: String,
    pub muted: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObsInfo {
    /// The OBS script is running and talking to the relay.
    pub script: bool,
    /// OBS streams to this relay.
    pub configured: bool,
    pub message: Option<String>,
    pub message_error: bool,
    pub message_id: u64,
    /// Scene names reported by the OBS script.
    pub scenes: Vec<String>,
    pub program_scene: String,
    /// Audio sources with their mute state.
    pub audio: Vec<AudioSource>,
    /// OBS is streaming / recording (OBS itself, not the relay).
    pub streaming: bool,
    pub recording: bool,
    pub video: Option<ObsVideo>,
}

impl Bridge {
    /// Outcome of an OBS action, shown once in the panel.
    pub fn set_message(&mut self, text: &str, error: bool) {
        self.message = Some((Instant::now(), text.to_string()));
        self.message_error = error;
        self.message_id += 1;
    }

    pub fn info(&self) -> ObsInfo {
        let script = self.last_poll.is_some_and(|t| t.elapsed() < Duration::from_secs(4));
        ObsInfo {
            script,
            configured: script && self.obs_configured,
            message: self
                .message
                .as_ref()
                .filter(|(t, _)| t.elapsed() < Duration::from_secs(60))
                .map(|(_, m)| m.clone()),
            message_error: self.message_error,
            message_id: self.message_id,
            scenes: self.scenes.clone(),
            program_scene: self.program_scene.clone(),
            audio: self.audio.clone(),
            streaming: self.streaming,
            recording: self.recording,
            video: self.video,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamState {
    Idle,
    Connecting,
    Connected,
    Reconnecting,
}

/// One destination (the main one has id 0).
#[derive(Clone, Debug, Serialize)]
pub struct OutputStatus {
    /// Stable id for start/stop, also across list changes while live.
    pub id: u64,
    pub name: String,
    /// Sending (started), as opposed to listed but stopped.
    pub running: bool,
    pub auto_start: bool,
    pub host: String,
    pub state: UpstreamState,
    pub error: Option<String>,
    pub kbps: u32,
    /// How far behind the delayed stream this destination runs after an outage.
    pub behind_ms: u32,
    pub reconnects: u32,
    /// Own video bitrate, re-encoded by the relay (0 = what OBS sends).
    pub video_kbps: u32,
    /// Encoder doing that ("NVENC", "x264"...), empty while not re-encoding.
    pub encoder: String,
    /// Why the video is sent as OBS sends it although a bitrate of its own is set.
    pub transcode_error: Option<String>,
}

impl OutputStatus {
    pub fn new(id: u64, name: &str, url: &str) -> Self {
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
        OutputStatus {
            id,
            name: name.to_string(),
            running: false,
            auto_start: true,
            host,
            state: UpstreamState::Idle,
            error: None,
            kbps: 0,
            behind_ms: 0,
            reconnects: 0,
            video_kbps: 0,
            encoder: String::new(),
            transcode_error: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Health {
    /// Bitrate received from OBS.
    pub in_kbps: u32,
    /// Frames per second received from OBS (0 = not streaming).
    pub in_fps: f32,
    /// Picture size of the stream (0 = unknown).
    pub width: u32,
    pub height: u32,
    pub uptime_s: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Event {
    pub id: u64,
    /// "ok", "warn" or "error".
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub version: &'static str,
    pub enabled: bool,
    pub delay_seconds: u32,
    pub max_delay_seconds: u32,
    pub obs_connected: bool,
    /// Main destination (same as `outputs[0]`).
    pub upstream: UpstreamState,
    pub upstream_error: Option<String>,
    pub engine: Option<EngineStatus>,
    pub outputs: Vec<OutputStatus>,
    pub health: Health,
    pub panic: bool,
    pub last_clip: Option<String>,
    pub event: Option<Event>,
    /// Update notice (None when the feature is off).
    pub update: Option<crate::update::UpdateStatus>,
    /// Seconds until the auto-off timer turns the delay off (None = not counting).
    pub auto_off_s: Option<u64>,
    /// Length of the armed auto-off timer (0 = none); it counts while the delay is on.
    pub auto_off_minutes: u32,
}

impl Status {
    pub fn new(cfg: &Config) -> Self {
        Status {
            version: env!("CARGO_PKG_VERSION"),
            enabled: cfg.start_enabled,
            delay_seconds: cfg.delay_seconds,
            max_delay_seconds: cfg.max_delay_seconds,
            obs_connected: false,
            upstream: UpstreamState::Idle,
            upstream_error: None,
            engine: None,
            outputs: Vec::new(),
            health: Health::default(),
            panic: false,
            last_clip: None,
            event: None,
            update: None,
            auto_off_s: None,
            auto_off_minutes: 0,
        }
    }

    /// Human readable status, shown inside OBS by the script.
    pub fn summary(&self) -> String {
        let d = self.delay_seconds;
        let mut lines = vec![if self.enabled {
            t!("Delay ON ({d}s)", "Delay LIGADO ({d}s)", "Delay ACTIVADO ({d}s)")
        } else {
            t!("Delay OFF (set to {d}s)", "Delay DESLIGADO (configurado: {d}s)", "Delay DESACTIVADO (configurado: {d}s)")
        }];
        lines.push(match &self.engine {
            None => t!("Not live right now", "Sem live no momento", "Sin transmisión en este momento"),
            Some(e) => {
                let phase = match e.phase {
                    Phase::Live => t!("LIVE", "AO VIVO", "EN DIRECTO"),
                    Phase::Delayed => t!("DELAYED", "COM DELAY", "CON DELAY"),
                    Phase::Growing => t!("waiting for a keyframe", "aguardando keyframe", "esperando un keyframe"),
                    Phase::Filling => t!("holding a frame, applying delay", "congelado, aplicando delay", "congelado, aplicando delay"),
                    Phase::Shrinking => t!("cutting back to live", "cortando para o vivo", "volviendo al directo"),
                };
                let secs = e.current_ms as f64 / 1000.0;
                t!("{phase} | current delay {secs:.1}s", "{phase} | atraso atual {secs:.1}s", "{phase} | retraso actual {secs:.1}s")
            }
        });
        let obs = if self.obs_connected { t!("OBS streaming", "OBS transmitindo", "OBS transmitiendo") } else { t!("OBS idle", "OBS parado", "OBS inactivo") };
        let up = match self.upstream {
            UpstreamState::Idle => t!("platform idle", "plataforma parada", "plataforma inactiva"),
            UpstreamState::Connecting => t!("connecting to the platform", "conectando na plataforma", "conectando con la plataforma"),
            UpstreamState::Connected => t!("platform connected", "plataforma conectada", "plataforma conectada"),
            UpstreamState::Reconnecting => t!("platform RECONNECTING", "plataforma RECONECTANDO", "plataforma RECONECTANDO"),
        };
        lines.push(format!("{obs} | {up}"));
        if let (true, Some(e)) = (self.upstream != UpstreamState::Connected, &self.upstream_error) {
            lines.push(t!("Error: {e}", "Erro: {e}", "Error: {e}"));
        }
        if let Some(left) = self.auto_off_s {
            let (m, s) = (left / 60, left % 60);
            lines.push(t!("Auto-off in {m}:{s:02}", "Desliga sozinho em {m}:{s:02}", "Apagado automático en {m}:{s:02}"));
        }
        if self.panic {
            lines.push(t!("PANIC MODE ON", "MODO PÂNICO LIGADO", "MODO PÁNICO ACTIVADO"));
        }
        if let Some(v) = self.update.as_ref().filter(|u| u.available).and_then(|u| u.latest.as_deref()) {
            lines.push(t!(
                "UPDATE: version {v} is available (github.com/ragnarcb/obs-dynamic-delay/releases)",
                "ATUALIZAÇÃO: a versão {v} está disponível (github.com/ragnarcb/obs-dynamic-delay/releases)",
                "ACTUALIZACIÓN: la versión {v} está disponible (github.com/ragnarcb/obs-dynamic-delay/releases)"
            ));
        }
        lines.join("\n")
    }
}

/// A control command, from the HTTP API, the UDP port, hotkeys or chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    On,
    Off,
    Toggle,
    Set(u32),
    Add(i64),
    /// Delete the newest unaired seconds (None = configured length).
    Censor(Option<u32>),
    /// Instant replay on air (None = configured length).
    Replay(Option<u32>),
    /// Save a clip of the last seconds (None = configured length).
    Clip(Option<u32>),
    /// Toggle the panic mode.
    Panic,
    /// Make every destination drop its outage backlog.
    CatchUp,
    /// Start / stop / toggle one destination of the running stream (by id).
    OutputStart(u64),
    OutputStop(u64),
    OutputToggle(u64),
    /// Turn the delay off after this many minutes (0 = cancel the timer).
    AutoOff(u32),
    /// Turn the delay on (with these seconds, if given) and off again after the minutes.
    OnFor(u32, Option<u32>),
    /// Exit once no stream is active (a delayed tail is still sent first).
    Quit,
    /// Cancel a pending Quit.
    Stay,
}

impl Cmd {
    /// Parses text commands: `on`, `off`, `toggle`, `set 30`, `add -5`, `censor [s]`,
    /// `replay [s]`, `clip [s]`, `panic`, `catchup`, `autooff <min>` (0 = cancel),
    /// `onfor <min> [s]` (on, then off after the minutes), `quit`, `stay`.
    pub fn parse(s: &str) -> Option<Cmd> {
        let mut it = s.split_whitespace();
        let cmd = it.next()?.to_ascii_lowercase();
        let arg = it.next();
        let num = |a: Option<&str>| a.and_then(|n| n.parse::<u32>().ok());
        match (cmd.as_str(), arg) {
            ("on", None) => Some(Cmd::On),
            ("off", None) => Some(Cmd::Off),
            ("toggle", None) => Some(Cmd::Toggle),
            ("set", Some(n)) => n.parse().ok().map(Cmd::Set),
            ("add", Some(n)) => n.parse().ok().map(Cmd::Add),
            ("censor", a) => Some(Cmd::Censor(num(a))),
            ("replay", a) => Some(Cmd::Replay(num(a))),
            ("clip", a) => Some(Cmd::Clip(num(a))),
            ("panic", None) => Some(Cmd::Panic),
            ("catchup", None) => Some(Cmd::CatchUp),
            ("quit", None) => Some(Cmd::Quit),
            ("stay", None) => Some(Cmd::Stay),
            ("autooff" | "timer", Some(n)) => n.parse().ok().map(Cmd::AutoOff),
            ("onfor", Some(n)) => {
                let secs = it.next().map(|s| s.parse::<u32>().ok());
                match (n.parse().ok(), secs) {
                    (Some(m), None) => Some(Cmd::OnFor(m, None)),
                    (Some(m), Some(Some(s))) => Some(Cmd::OnFor(m, Some(s))),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands() {
        assert_eq!(Cmd::parse("toggle\n"), Some(Cmd::Toggle));
        assert_eq!(Cmd::parse("SET 45"), Some(Cmd::Set(45)));
        assert_eq!(Cmd::parse("add -5"), Some(Cmd::Add(-5)));
        assert_eq!(Cmd::parse("censor"), Some(Cmd::Censor(None)));
        assert_eq!(Cmd::parse("censor 7"), Some(Cmd::Censor(Some(7))));
        assert_eq!(Cmd::parse("clip 20"), Some(Cmd::Clip(Some(20))));
        assert_eq!(Cmd::parse("panic"), Some(Cmd::Panic));
        assert_eq!(Cmd::parse("quit"), Some(Cmd::Quit));
        assert_eq!(Cmd::parse("set"), None);
        assert_eq!(Cmd::parse("nope"), None);
        assert_eq!(Cmd::parse("autooff 20"), Some(Cmd::AutoOff(20)));
        assert_eq!(Cmd::parse("autooff 0"), Some(Cmd::AutoOff(0)));
        assert_eq!(Cmd::parse("autooff"), None);
        assert_eq!(Cmd::parse("onfor 20"), Some(Cmd::OnFor(20, None)));
        assert_eq!(Cmd::parse("onfor 20 60"), Some(Cmd::OnFor(20, Some(60))));
        assert_eq!(Cmd::parse("onfor 20 x"), None);
    }

    #[test]
    fn auto_off_counts_while_on() {
        let t0 = Instant::now();
        let min = |m: u64| Duration::from_secs(m * 60);
        let mut a = AutoOff::default();
        assert_eq!(a.seconds_left(t0), None);
        // armed with the delay on: counts from now
        a.set(20, true, t0);
        assert_eq!(a.seconds_left(t0), Some(1200));
        assert!(!a.due(t0 + min(19)));
        assert!(a.due(t0 + min(20)));
        assert!(!a.due(t0 + min(21)), "fires once");
        assert_eq!(a.minutes(), 0);

        // armed while off: starts when the delay goes on
        a.set(15, false, t0);
        assert_eq!((a.minutes(), a.seconds_left(t0)), (15, None));
        assert!(!a.due(t0 + min(60)));
        a.delay_changed(false, true, t0 + min(60));
        assert_eq!(a.seconds_left(t0 + min(61)), Some(14 * 60));

        // turning the delay off by hand cancels it
        a.delay_changed(true, false, t0 + min(62));
        assert_eq!((a.minutes(), a.seconds_left(t0)), (0, None));
        a.set(0, true, t0);
        assert_eq!(a.seconds_left(t0), None);
        a.set(100_000, true, t0);
        assert_eq!(a.minutes(), AutoOff::MAX_MINUTES);
    }

    #[test]
    fn events_from_status_changes() {
        use crate::engine::{EngineStatus, Phase};
        let cfg = Config::default();
        let a = Status::new(&cfg);
        assert!(status_events(&a, &a.clone()).is_empty());
        let mut b = a.clone();
        b.enabled = true;
        b.delay_seconds = 60;
        b.panic = true;
        b.last_clip = Some("c.mp4".into());
        b.engine = Some(EngineStatus { phase: Phase::Growing, target_ms: 60_000, current_ms: 0, buffered_ms: 0, buffered_bytes: 0, replaying: true });
        let mut out = OutputStatus::new(0, "Main", "rtmp://live.twitch.tv/app");
        out.state = UpstreamState::Connecting;
        b.outputs.push(out);
        b.auto_off_minutes = 20;
        b.auto_off_s = Some(1200);
        let ev = status_events(&a, &b);
        let types: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
        assert_eq!(
            types,
            ["delay_on", "delay_seconds", "phase", "replay_start", "panic_on", "clip_saved", "destination", "auto_off_timer"]
        );
        assert_eq!(ev[2]["phase"], "growing");
        assert_eq!(ev[2]["previous"], "offline");
        assert_eq!(ev[6]["state"], "connecting");
        // the countdown ticking is not an event
        let mut c = b.clone();
        c.auto_off_s = Some(1100);
        assert!(status_events(&b, &c).is_empty());
    }

    #[test]
    fn encodes_actions() {
        assert_eq!(ObsAction::Panic { scene: "BRB".into(), mute: true }.encode(), "panic\t1\tBRB");
        assert_eq!(ObsAction::ShowScene("X".into()).encode(), "scene_show\tX");
        assert_eq!(ObsAction::ToggleMute("Mic/Aux".into()).encode(), "mute\tMic/Aux");
    }
}
