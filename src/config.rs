use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const TWITCH_URL: &str = "rtmp://live.twitch.tv/app";
pub const YOUTUBE_URL: &str = "rtmp://a.rtmp.youtube.com/live2";

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub listen: String,
    pub upstream_url: String,
    pub stream_key: String,
    pub delay_seconds: u32,
    pub start_enabled: bool,
    /// How extra delay is built: "rewind", "scene" or "freeze".
    pub grow_mode: String,
    /// OBS scene shown while the delay builds up (grow_mode = "scene").
    pub delay_scene: String,
    pub max_delay_seconds: u32,
    pub filler_fps: u32,
    pub http_listen: String,
    pub udp_listen: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: "127.0.0.1:1935".into(),
            upstream_url: TWITCH_URL.into(),
            stream_key: String::new(),
            delay_seconds: 30,
            start_enabled: false,
            grow_mode: "rewind".into(),
            delay_scene: String::new(),
            max_delay_seconds: 600,
            filler_fps: 2,
            http_listen: "127.0.0.1:8787".into(),
            udp_listen: "127.0.0.1:8788".into(),
        }
    }
}

fn q(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

impl Config {
    /// Loads the config, writing the default one first if the file does not exist.
    pub fn load_or_create(path: &Path) -> Result<Config> {
        if !path.exists() {
            Config::default().save(path)?;
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.max_delay_seconds = cfg.max_delay_seconds.max(1);
        cfg.delay_seconds = cfg.delay_seconds.min(cfg.max_delay_seconds);
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_toml())
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn to_toml(&self) -> String {
        format!(
            r#"# obs-dynamic-delay configuration
# Normally edited from the "Delay dinamico" panel inside OBS.

# Address OBS streams to (OBS: Settings > Stream > Custom, rtmp://127.0.0.1:1935/live).
listen = {listen}

# Real destination. Twitch: {TWITCH_URL}  YouTube: {YOUTUBE_URL}
# Kick and others: the rtmp:// or rtmps:// URL shown in the platform dashboard.
upstream_url = {url}

# Leave empty to use the stream key typed in OBS.
stream_key = {key}

# Delay applied when the delay is switched on (seconds).
delay_seconds = {delay}

# Start every stream with the delay already on.
start_enabled = {start}

# What viewers see when the delay is switched on or increased:
#   "rewind" = replay the last seconds (instant, no freeze)
#   "scene"  = show the OBS scene below, frozen, while the delay builds up
#   "freeze" = freeze the live picture while the delay builds up
grow_mode = {grow}
delay_scene = {scene}

# Upper limit for the delay. Memory use is roughly bitrate x delay.
max_delay_seconds = {max}

# Frame rate of the frozen image shown while the delay builds up.
filler_fps = {fps}

# Control panel / API.
http_listen = {http}

# UDP control port used by the OBS script.
udp_listen = {udp}
"#,
            listen = q(&self.listen),
            url = q(&self.upstream_url),
            key = q(&self.stream_key),
            delay = self.delay_seconds,
            start = self.start_enabled,
            grow = q(&self.grow_mode),
            scene = q(&self.delay_scene),
            max = self.max_delay_seconds,
            fps = self.filler_fps,
            http = q(&self.http_listen),
            udp = q(&self.udp_listen),
        )
    }
}

/// Maps the stream settings found in OBS to a destination URL.
pub fn destination_from_obs(server: &str, service: &str) -> Option<String> {
    if server.starts_with("rtmp://") || server.starts_with("rtmps://") {
        if server.contains("127.0.0.1") || server.contains("localhost") {
            return None; // already pointing to a local relay
        }
        return Some(server.to_string());
    }
    let service = service.to_ascii_lowercase();
    if service.contains("twitch") {
        Some(TWITCH_URL.into())
    } else if service.contains("youtube") {
        Some(YOUTUBE_URL.into())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_toml() {
        let mut c = Config::default();
        c.stream_key = r#"a"b\c"#.into();
        c.start_enabled = true;
        let parsed: Config = toml::from_str(&c.to_toml()).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn maps_obs_destinations() {
        assert_eq!(destination_from_obs("auto", "Twitch").as_deref(), Some(TWITCH_URL));
        assert_eq!(
            destination_from_obs("rtmps://a.rtmps.youtube.com:443/live2", "YouTube - RTMPS").as_deref(),
            Some("rtmps://a.rtmps.youtube.com:443/live2")
        );
        assert_eq!(destination_from_obs("rtmp://127.0.0.1:1935/live", ""), None);
        assert_eq!(destination_from_obs("", "Some service"), None);
    }
}
