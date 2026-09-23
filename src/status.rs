use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::Config;
use crate::engine::{EngineStatus, Phase};
use crate::t;

/// State shared between the engine, the outputs, the HTTP API and the UDP port.
pub struct Shared {
    pub status: Mutex<Status>,
    pub config: Mutex<Config>,
    pub config_path: PathBuf,
    pub bridge: Mutex<Bridge>,
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
    pub scenes: Vec<String>,
    pub program_scene: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObsInfo {
    /// The OBS script is running and talking to the relay.
    pub script: bool,
    /// OBS streams to this relay.
    pub configured: bool,
    pub message: Option<String>,
    /// Scene names reported by the OBS script.
    pub scenes: Vec<String>,
    pub program_scene: String,
}

impl Bridge {
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
            scenes: self.scenes.clone(),
            program_scene: self.program_scene.clone(),
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

/// One destination (the main one is index 0).
#[derive(Clone, Debug, Serialize)]
pub struct OutputStatus {
    pub name: String,
    pub host: String,
    pub state: UpstreamState,
    pub error: Option<String>,
    pub kbps: u32,
    /// How far behind the delayed stream this destination runs after an outage.
    pub behind_ms: u32,
    pub reconnects: u32,
}

impl OutputStatus {
    pub fn new(name: &str, url: &str) -> Self {
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
        OutputStatus {
            name: name.to_string(),
            host,
            state: UpstreamState::Idle,
            error: None,
            kbps: 0,
            behind_ms: 0,
            reconnects: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Health {
    /// Bitrate received from OBS.
    pub in_kbps: u32,
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
        }
    }

    /// Human readable status, shown inside OBS by the script.
    pub fn summary(&self) -> String {
        let d = self.delay_seconds;
        let mut lines = vec![if self.enabled {
            t!("Delay ON ({d}s)", "Delay LIGADO ({d}s)")
        } else {
            t!("Delay OFF (set to {d}s)", "Delay DESLIGADO (configurado: {d}s)")
        }];
        lines.push(match &self.engine {
            None => t!("Not live right now", "Sem live no momento"),
            Some(e) => {
                let phase = match e.phase {
                    Phase::Live => t!("LIVE", "AO VIVO"),
                    Phase::Delayed => t!("DELAYED", "COM DELAY"),
                    Phase::Growing => t!("waiting for a keyframe", "aguardando keyframe"),
                    Phase::Filling => t!("holding a frame, applying delay", "congelado, aplicando delay"),
                    Phase::Shrinking => t!("cutting back to live", "cortando para o vivo"),
                };
                let secs = e.current_ms as f64 / 1000.0;
                t!("{phase} | current delay {secs:.1}s", "{phase} | atraso atual {secs:.1}s")
            }
        });
        let obs = if self.obs_connected { t!("OBS streaming", "OBS transmitindo") } else { t!("OBS idle", "OBS parado") };
        let up = match self.upstream {
            UpstreamState::Idle => t!("platform idle", "plataforma parada"),
            UpstreamState::Connecting => t!("connecting to the platform", "conectando na plataforma"),
            UpstreamState::Connected => t!("platform connected", "plataforma conectada"),
            UpstreamState::Reconnecting => t!("platform RECONNECTING", "plataforma RECONECTANDO"),
        };
        lines.push(format!("{obs} | {up}"));
        if let (true, Some(e)) = (self.upstream != UpstreamState::Connected, &self.upstream_error) {
            lines.push(t!("Error: {e}", "Erro: {e}"));
        }
        if self.panic {
            lines.push(t!("PANIC MODE ON", "MODO PÂNICO LIGADO"));
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
    /// Exit once no stream is active (a delayed tail is still sent first).
    Quit,
    /// Cancel a pending Quit.
    Stay,
}

impl Cmd {
    /// Parses text commands: `on`, `off`, `toggle`, `set 30`, `add -5`, `censor [s]`,
    /// `replay [s]`, `clip [s]`, `panic`, `catchup`, `quit`, `stay`.
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
    }

    #[test]
    fn encodes_actions() {
        assert_eq!(ObsAction::Panic { scene: "BRB".into(), mute: true }.encode(), "panic\t1\tBRB");
        assert_eq!(ObsAction::ShowScene("X".into()).encode(), "scene_show\tX");
    }
}
