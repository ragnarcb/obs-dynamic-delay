use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::Config;
use crate::engine::{EngineStatus, Phase};

/// State shared between the engine, the upstream, the HTTP API and the UDP port.
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
}

/// Actions the panel asks the OBS script to perform inside OBS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObsAction {
    Configure,
    Restore,
}

/// Link with the OBS script, which polls the relay over UDP.
#[derive(Default)]
pub struct Bridge {
    pub pending: VecDeque<ObsAction>,
    pub last_poll: Option<Instant>,
    pub obs_configured: bool,
    pub message: Option<(Instant, String)>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObsInfo {
    /// The OBS script is running and talking to the relay.
    pub script: bool,
    /// OBS streams to this relay.
    pub configured: bool,
    pub message: Option<String>,
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

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub enabled: bool,
    pub delay_seconds: u32,
    pub max_delay_seconds: u32,
    pub obs_connected: bool,
    pub upstream: UpstreamState,
    pub upstream_error: Option<String>,
    pub engine: Option<EngineStatus>,
}

impl Status {
    /// Human readable status, shown inside OBS by the script.
    pub fn summary(&self) -> String {
        let mut lines = vec![if self.enabled {
            format!("Delay LIGADO ({}s)", self.delay_seconds)
        } else {
            format!("Delay DESLIGADO (configurado: {}s)", self.delay_seconds)
        }];
        lines.push(match &self.engine {
            None => "Sem live no momento".to_string(),
            Some(e) => {
                let phase = match e.phase {
                    Phase::Live => "AO VIVO",
                    Phase::Delayed => "COM DELAY",
                    Phase::Growing => "aguardando keyframe",
                    Phase::Filling => "congelado, aplicando delay",
                    Phase::Shrinking => "cortando para o vivo",
                };
                format!("{phase} | atraso atual {:.1}s", e.current_ms as f64 / 1000.0)
            }
        });
        let obs = if self.obs_connected { "OBS transmitindo" } else { "OBS parado" };
        let up = match self.upstream {
            UpstreamState::Idle => "plataforma parada",
            UpstreamState::Connecting => "conectando na plataforma",
            UpstreamState::Connected => "plataforma conectada",
            UpstreamState::Reconnecting => "plataforma RECONECTANDO",
        };
        lines.push(format!("{obs} | {up}"));
        if let (true, Some(e)) = (self.upstream != UpstreamState::Connected, &self.upstream_error) {
            lines.push(format!("Erro: {e}"));
        }
        lines.join("\n")
    }
}

/// A control command, from the HTTP API, the UDP port or the OBS script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    On,
    Off,
    Toggle,
    Set(u32),
    Add(i64),
    /// Exit once no stream is active (a delayed tail is still sent first).
    Quit,
    /// Cancel a pending Quit.
    Stay,
}

impl Cmd {
    /// Parses text commands: `on`, `off`, `toggle`, `set 30`, `add 5`, `add -5`, `quit`, `stay`.
    pub fn parse(s: &str) -> Option<Cmd> {
        let mut it = s.split_whitespace();
        let cmd = it.next()?.to_ascii_lowercase();
        let arg = it.next();
        match (cmd.as_str(), arg) {
            ("on", None) => Some(Cmd::On),
            ("off", None) => Some(Cmd::Off),
            ("toggle", None) => Some(Cmd::Toggle),
            ("set", Some(n)) => n.parse().ok().map(Cmd::Set),
            ("add", Some(n)) => n.parse().ok().map(Cmd::Add),
            ("quit", None) => Some(Cmd::Quit),
            ("stay", None) => Some(Cmd::Stay),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cmd;

    #[test]
    fn parses_commands() {
        assert_eq!(Cmd::parse("toggle\n"), Some(Cmd::Toggle));
        assert_eq!(Cmd::parse("SET 45"), Some(Cmd::Set(45)));
        assert_eq!(Cmd::parse("add -5"), Some(Cmd::Add(-5)));
        assert_eq!(Cmd::parse("quit"), Some(Cmd::Quit));
        assert_eq!(Cmd::parse("set"), None);
        assert_eq!(Cmd::parse("nope"), None);
    }
}
