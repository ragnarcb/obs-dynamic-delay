use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const DEFAULT_CONFIG: &str = r#"# obs-dynamic-delay configuration

# Address OBS streams to. In OBS: Settings > Stream > Service "Custom...",
# Server "rtmp://127.0.0.1:1935/live". The stream key can be your real key.
listen = "127.0.0.1:1935"

# Real destination. Examples:
#   Twitch:  rtmp://live.twitch.tv/app
#   YouTube: rtmp://a.rtmp.youtube.com/live2
#   Kick:    the rtmps://... "Stream URL" shown in the Kick dashboard
upstream_url = "rtmp://live.twitch.tv/app"

# Leave empty to use the stream key typed in OBS.
stream_key = ""

# Delay applied when the delay is switched on (seconds).
delay_seconds = 30

# Start every stream with the delay already on.
start_enabled = false

# Upper limit for the delay. Memory use is roughly bitrate x delay.
max_delay_seconds = 600

# Frame rate of the frozen image shown while the delay builds up.
filler_fps = 2

# Control panel / API (add it to OBS as a Custom Browser Dock).
http_listen = "127.0.0.1:8787"

# UDP control port used by the OBS hotkey script.
udp_listen = "127.0.0.1:8788"
"#;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub listen: String,
    pub upstream_url: String,
    pub stream_key: String,
    pub delay_seconds: u32,
    pub start_enabled: bool,
    pub max_delay_seconds: u32,
    pub filler_fps: u32,
    pub http_listen: String,
    pub udp_listen: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "127.0.0.1:1935".into(),
            upstream_url: "rtmp://live.twitch.tv/app".into(),
            stream_key: String::new(),
            delay_seconds: 30,
            start_enabled: false,
            max_delay_seconds: 600,
            filler_fps: 2,
            http_listen: "127.0.0.1:8787".into(),
            udp_listen: "127.0.0.1:8788".into(),
        }
    }
}

impl Config {
    /// Loads the config, writing the default one first if the file does not exist.
    pub fn load_or_create(path: &Path) -> Result<Config> {
        if !path.exists() {
            std::fs::write(path, DEFAULT_CONFIG)
                .with_context(|| format!("writing default config to {}", path.display()))?;
            log::info!("created default config at {}", path.display());
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.max_delay_seconds = cfg.max_delay_seconds.max(1);
        cfg.delay_seconds = cfg.delay_seconds.min(cfg.max_delay_seconds);
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_file_matches_defaults() {
        let parsed: super::Config = toml::from_str(super::DEFAULT_CONFIG).unwrap();
        let d = super::Config::default();
        assert_eq!(format!("{parsed:?}"), format!("{d:?}"));
    }
}
