use serde::Serialize;

use crate::engine::EngineStatus;

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

/// A control command, from the HTTP API, the UDP port or the OBS script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    On,
    Off,
    Toggle,
    Set(u32),
    Add(i64),
}

impl Cmd {
    /// Parses text commands: `on`, `off`, `toggle`, `set 30`, `add 5`, `add -5`.
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
        assert_eq!(Cmd::parse("set"), None);
        assert_eq!(Cmd::parse("nope"), None);
    }
}
