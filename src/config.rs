use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const TWITCH_URL: &str = "rtmp://live.twitch.tv/app";
pub const YOUTUBE_URL: &str = "rtmp://a.rtmp.youtube.com/live2";

/// Panel modules, in their default order. `panel_modules` lists the visible ones.
pub const ALL_MODULES: &[&str] = &[
    "delay", "censor", "replay", "clips", "panic", "health", "multistream", "rules", "chat", "phone", "streamdeck",
];
pub const DEFAULT_MODULES: &[&str] = &["delay", "censor", "health"];

/// An extra multistream destination (the main one is `upstream_url` / `stream_key`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Destination {
    pub name: String,
    pub url: String,
    pub key: String,
    pub enabled: bool,
}

impl Default for Destination {
    fn default() -> Self {
        Destination { name: String::new(), url: String::new(), key: String::new(), enabled: true }
    }
}

/// What happens when an OBS scene goes on air.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SceneRule {
    pub scene: String,
    /// "on", "off" or "set:<seconds>" (turns the delay on with that length).
    pub action: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct TwitchChat {
    pub enabled: bool,
    pub channel: String,
    /// Who may use the commands: "broadcaster", "mods" or "vips" (mods and VIPs).
    pub allow: String,
    pub prefix: String,
}

impl Default for TwitchChat {
    fn default() -> Self {
        TwitchChat { enabled: false, channel: String::new(), allow: "mods".into(), prefix: "!delay".into() }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// "en" or "pt": language of the panel, the OBS script and messages.
    pub language: String,
    /// Address OBS streams to.
    pub listen: String,
    /// Main destination and key (empty key = use the key typed in OBS).
    pub upstream_url: String,
    pub stream_key: String,
    /// Extra destinations for multistreaming.
    pub destinations: Vec<Destination>,

    pub delay_seconds: u32,
    pub start_enabled: bool,
    /// How extra delay is built: "rewind", "scene" or "freeze".
    pub grow_mode: String,
    /// OBS scene shown while the delay builds up (grow_mode = "scene").
    pub delay_scene: String,
    pub max_delay_seconds: u32,
    pub filler_fps: u32,

    /// "Delete before it airs": seconds removed from the delay buffer.
    pub censor_seconds: u32,
    /// Instant replay length.
    pub replay_seconds: u32,
    /// Clip length and folder (empty = Videos\Dynamic Delay).
    pub clip_seconds: u32,
    pub clips_dir: String,

    /// Keep sending what was missed when the platform connection drops (0 = off).
    pub outage_buffer_seconds: u32,
    pub alert_sound: bool,

    pub scene_rules: Vec<SceneRule>,

    /// Panic button: scene to show, mute all audio, delete the unaired part.
    pub panic_scene: String,
    pub panic_mute: bool,
    pub panic_censor: bool,

    pub twitch_chat: TwitchChat,

    /// Serve the panel on the local network (for phones), protected by the token.
    pub lan_access: bool,
    pub http_listen: String,
    pub udp_listen: String,
    /// Secret required by the HTTP API. Generated on first start.
    pub api_token: String,

    /// Visible panel modules, in order.
    pub panel_modules: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: crate::i18n::DEFAULT.code().into(),
            listen: "127.0.0.1:1935".into(),
            upstream_url: TWITCH_URL.into(),
            stream_key: String::new(),
            destinations: Vec::new(),
            delay_seconds: 30,
            start_enabled: false,
            grow_mode: "rewind".into(),
            delay_scene: String::new(),
            max_delay_seconds: 600,
            filler_fps: 2,
            censor_seconds: 10,
            replay_seconds: 10,
            clip_seconds: 30,
            clips_dir: String::new(),
            outage_buffer_seconds: 60,
            alert_sound: true,
            scene_rules: Vec::new(),
            panic_scene: String::new(),
            panic_mute: true,
            panic_censor: true,
            twitch_chat: TwitchChat::default(),
            lan_access: false,
            http_listen: "127.0.0.1:8787".into(),
            udp_listen: "127.0.0.1:8788".into(),
            api_token: String::new(),
            panel_modules: DEFAULT_MODULES.iter().map(|s| s.to_string()).collect(),
        }
    }
}

const HEADER: &str = "# obs-dynamic-delay configuration.
# Normally edited from the \"Dynamic Delay\" panel inside OBS.
# grow_mode: \"rewind\" (replay the last seconds), \"scene\" (show delay_scene) or \"freeze\".
# scene_rules actions: \"on\", \"off\" or \"set:<seconds>\".
# twitch_chat.allow: \"broadcaster\", \"mods\" or \"vips\".

";

impl Config {
    /// Loads the config, writing the default one first if the file does not exist.
    /// Also fills in a random API token when there is none.
    pub fn load_or_create(path: &Path) -> Result<Config> {
        if !path.exists() {
            Config::default().save(path)?;
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        cfg.normalize();
        if cfg.api_token.is_empty() {
            cfg.api_token = new_token();
            cfg.save(path)?;
        }
        Ok(cfg)
    }

    /// Keeps values in their valid ranges.
    pub fn normalize(&mut self) {
        self.max_delay_seconds = self.max_delay_seconds.max(1);
        self.delay_seconds = self.delay_seconds.min(self.max_delay_seconds);
        self.clip_seconds = self.clip_seconds.clamp(5, 120);
        self.replay_seconds = self.replay_seconds.clamp(3, 60);
        self.censor_seconds = self.censor_seconds.clamp(1, 120);
        self.outage_buffer_seconds = self.outage_buffer_seconds.min(300);
        self.filler_fps = self.filler_fps.clamp(1, 30);
        if !["rewind", "scene", "freeze"].contains(&self.grow_mode.as_str()) {
            self.grow_mode = "rewind".into();
        }
        if !["broadcaster", "mods", "vips"].contains(&self.twitch_chat.allow.as_str()) {
            self.twitch_chat.allow = "mods".into();
        }
        self.twitch_chat.channel = crate::chat::channel_name(&self.twitch_chat.channel);
        let mut seen = Vec::new();
        self.panel_modules.retain(|m| ALL_MODULES.contains(&m.as_str()) && !seen.contains(m) && {
            seen.push(m.clone());
            true
        });
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self)?;
        std::fs::write(path, format!("{HEADER}{body}")).with_context(|| format!("writing {}", path.display()))
    }

    /// Port of an "ip:port" listen address.
    pub fn port_of(addr: &str) -> u16 {
        addr.rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or(0)
    }

    pub fn http_port(&self) -> u16 {
        Self::port_of(&self.http_listen)
    }

    pub fn clips_path(&self) -> PathBuf {
        if !self.clips_dir.trim().is_empty() {
            return PathBuf::from(self.clips_dir.trim());
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
        PathBuf::from(home).join("Videos").join("Dynamic Delay")
    }
}

fn new_token() -> String {
    let s = std::collections::hash_map::RandomState::new();
    let a = s.hash_one(std::process::id());
    let b = s.hash_one(std::time::SystemTime::now());
    format!("{a:016x}{b:016x}")
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
        c.destinations.push(Destination { name: "YT".into(), url: YOUTUBE_URL.into(), key: "k".into(), enabled: false });
        c.scene_rules.push(SceneRule { scene: "Ranked".into(), action: "set:60".into() });
        let text = format!("{HEADER}{}", toml::to_string_pretty(&c).unwrap());
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed, c);
    }

    #[test]
    fn old_configs_still_load() {
        let old = "language = \"pt\"\nupstream_url = \"rtmp://a/b\"\nstream_key = \"k\"\ndelay_seconds = 45\n";
        let c: Config = toml::from_str(old).unwrap();
        assert_eq!(c.delay_seconds, 45);
        assert_eq!(c.panel_modules, vec!["delay", "censor", "health"]);
        assert_eq!(c.twitch_chat.prefix, "!delay");
    }

    #[test]
    fn creates_token() {
        let dir = std::env::temp_dir().join(format!("dd-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        let _ = std::fs::remove_file(&p);
        let a = Config::load_or_create(&p).unwrap();
        assert_eq!(a.api_token.len(), 32);
        let b = Config::load_or_create(&p).unwrap();
        assert_eq!(a.api_token, b.api_token);
        let _ = std::fs::remove_dir_all(&dir);
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
