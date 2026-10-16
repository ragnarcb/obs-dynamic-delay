use std::hash::BuildHasher;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const TWITCH_URL: &str = "rtmp://live.twitch.tv/app";
pub const YOUTUBE_URL: &str = "rtmp://a.rtmp.youtube.com/live2";

/// Panel modules, in their default order. `panel_modules` lists the visible ones.
pub const ALL_MODULES: &[&str] = &[
    "delay", "censor", "replay", "clips", "panic", "health", "multistream", "rules", "chat", "phone", "streamdeck", "overlay",
];
pub const DEFAULT_MODULES: &[&str] = &["delay", "censor", "health"];
pub const DEFAULT_PRESETS: &[u32] = &[10, 30, 60, 120];

/// An extra multistream destination (the main one is `upstream_url` / `stream_key`).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Destination {
    /// Stable id, so a destination keeps its saved key when renamed (0 = new,
    /// given one by [`Config::normalize`]).
    pub id: u64,
    pub name: String,
    pub url: String,
    pub key: String,
    /// Listed for the stream (off = kept in the list, never used).
    pub enabled: bool,
    /// Goes live together with the stream; otherwise it is started from the panel.
    pub auto_start: bool,
    /// Video bitrate of its own in kbps, re-encoded by the relay (0 = what OBS sends).
    pub video_kbps: u32,
}

impl Default for Destination {
    fn default() -> Self {
        Destination { id: 0, name: String::new(), url: String::new(), key: String::new(), enabled: true, auto_start: true, video_kbps: 0 }
    }
}

/// Own bitrate of a destination: 0 (what OBS sends) or 500 to 50000 kbps.
fn video_kbps(v: u32) -> u32 {
    if v == 0 { 0 } else { v.clamp(500, 50_000) }
}

/// Actions a phone deck key can run. `arg` is seconds, minutes (`delay.timer`), a scene or an audio source.
pub const DECK_ACTIONS: &[&str] = &[
    "delay.toggle", "delay.on", "delay.off", "delay.set", "delay.add", "delay.timer", "censor", "replay", "clip", "panic", "catchup",
    "obs.scene", "obs.mute", "obs.stream", "obs.record", "dest.toggle",
];

/// On-screen widget (OBS Browser Source at /overlay) that tells viewers the delay is on.
/// Every field can also be overridden per source with a URL parameter of the same name.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Overlay {
    /// "pill", "card" or "minimal" (text only, no box).
    pub style: String,
    /// "dark", "light" or "outline" (pill and card only).
    pub theme: String,
    pub show_dot: bool,
    pub show_label: bool,
    pub show_seconds: bool,
    /// Progress line while the delay builds up or goes down.
    pub show_progress: bool,
    /// Hidden while the delay is off (else shows the "off" text).
    pub hide_when_off: bool,
    /// Time left on the auto-off timer, while it runs.
    pub show_timer: bool,
    /// Visible while not live too (shows the set delay), to place and style it in OBS.
    pub show_offline: bool,
    /// Custom texts (empty = default text in the panel language).
    pub label_on: String,
    pub label_off: String,
    pub label_adjusting: String,
    pub label_replay: String,
    /// "s" (30s) or "clock" (0:30).
    pub time_format: String,
    /// Color of the dot and the progress line (#rrggbb).
    pub accent: String,
    /// Size in percent (50 to 300).
    pub scale: u32,
    /// "left", "center" or "right" inside the source.
    pub align: String,
    /// "sans", "mono" or "condensed".
    pub font: String,
}

impl Default for Overlay {
    fn default() -> Self {
        Overlay {
            style: "pill".into(),
            theme: "dark".into(),
            show_dot: true,
            show_label: true,
            show_seconds: true,
            show_progress: true,
            hide_when_off: true,
            show_timer: false,
            show_offline: true,
            label_on: String::new(),
            label_off: String::new(),
            label_adjusting: String::new(),
            label_replay: String::new(),
            time_format: "s".into(),
            accent: "#e5484d".into(),
            scale: 100,
            align: "left".into(),
            font: "sans".into(),
        }
    }
}

impl Overlay {
    fn normalize(&mut self) {
        let pick = |v: &mut String, ok: &[&str]| {
            if !ok.contains(&v.as_str()) {
                *v = ok[0].into();
            }
        };
        pick(&mut self.style, &["pill", "card", "minimal"]);
        pick(&mut self.theme, &["dark", "light", "outline"]);
        pick(&mut self.time_format, &["s", "clock"]);
        pick(&mut self.align, &["left", "center", "right"]);
        pick(&mut self.font, &["sans", "mono", "condensed"]);
        let hex = self.accent.len() == 7 && self.accent.starts_with('#') && self.accent[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !hex {
            self.accent = "#e5484d".into();
        }
        self.scale = self.scale.clamp(50, 300);
        for l in [&mut self.label_on, &mut self.label_off, &mut self.label_adjusting, &mut self.label_replay] {
            *l = l.chars().take(24).collect();
        }
    }
}

/// One key of the phone deck.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct DeckKey {
    pub action: String,
    pub arg: String,
    /// Empty = the default label of the action.
    pub label: String,
    /// "", "blue", "green", "orange", "red" or "grey".
    pub color: String,
}

impl Default for DeckKey {
    fn default() -> Self {
        DeckKey { action: "delay.toggle".into(), arg: String::new(), label: String::new(), color: String::new() }
    }
}

/// The phone deck: a grid of keys, like a Stream Deck.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Deck {
    pub columns: u8,
    pub keys: Vec<DeckKey>,
}

impl Default for Deck {
    fn default() -> Self {
        let key = |action: &str, arg: &str| DeckKey { action: action.into(), arg: arg.into(), ..DeckKey::default() };
        Deck {
            columns: 3,
            keys: vec![
                key("delay.toggle", ""),
                key("delay.add", "-5"),
                key("delay.add", "5"),
                key("censor", ""),
                key("replay", ""),
                key("clip", ""),
                key("panic", ""),
                key("delay.off", ""),
                key("obs.stream", ""),
            ],
        }
    }
}

/// What happens when an OBS scene goes on air.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SceneRule {
    pub scene: String,
    /// "on", "off" or "set:<seconds>" (turns the delay on with that length).
    pub action: String,
}

/// Optional features. A feature that is off does no work at all: its commands are
/// refused, its background tasks stop and its panel block is hidden.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Features {
    pub censor: bool,
    pub replay: bool,
    pub clips: bool,
    pub panic: bool,
    pub multistream: bool,
    /// Keep and resend what was missed during a connection drop.
    pub outage: bool,
    /// Delay rules per OBS scene.
    pub rules: bool,
    /// Twitch chat commands (connects to Twitch chat).
    pub chat: bool,
    /// Panel on the local network for phones (opens the port to the LAN).
    pub phone: bool,
    /// The panel checks GitHub for new versions.
    pub update_check: bool,
    /// On-screen widget page (/overlay) for an OBS Browser Source.
    pub overlay: bool,
}

impl Default for Features {
    fn default() -> Self {
        Features {
            censor: true,
            replay: true,
            clips: true,
            panic: true,
            multistream: true,
            outage: true,
            rules: true,
            chat: false,
            phone: false,
            update_check: true,
            overlay: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct TwitchChat {
    /// Replaced by `features.chat`; still read from older configs.
    #[serde(skip_serializing)]
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
    /// "en", "pt" or "es": language of the panel, the OBS script and messages.
    pub language: String,
    /// Address OBS streams to.
    pub listen: String,
    /// Main destination and key (empty key = use the key typed in OBS).
    pub upstream_url: String,
    pub stream_key: String,
    /// The main destination goes live together with the stream (off = only the extras do).
    pub main_auto_start: bool,
    /// Video bitrate of the main destination, re-encoded by the relay (0 = what OBS sends).
    pub main_video_kbps: u32,
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
    /// Delay buttons of the panel, in seconds (1 to 6 of them).
    pub presets: Vec<u32>,

    /// "Delete before it airs": seconds removed from the delay buffer.
    pub censor_seconds: u32,
    /// Instant replay length.
    pub replay_seconds: u32,
    /// Clip length and folder (empty = Videos\Dynamic Delay).
    pub clip_seconds: u32,
    pub clips_dir: String,
    /// Clips hold only what viewers already saw (not the part still in the delay).
    pub clip_aired_only: bool,

    /// Keep sending what was missed when the platform connection drops (0 = off).
    pub outage_buffer_seconds: u32,
    pub alert_sound: bool,

    pub scene_rules: Vec<SceneRule>,

    /// Panic button: scene to show, mute all audio, delete the unaired part.
    pub panic_scene: String,
    pub panic_mute: bool,
    pub panic_censor: bool,

    pub twitch_chat: TwitchChat,

    /// Replaced by `features.phone`; still read from older configs.
    #[serde(skip_serializing)]
    pub lan_access: bool,
    pub http_listen: String,
    pub udp_listen: String,
    /// Secret required by the HTTP API. Generated on first start.
    pub api_token: String,

    /// Visible panel modules, in order.
    pub panel_modules: Vec<String>,
    /// Panel modules shown minimized (only their title bar).
    pub panel_collapsed: Vec<String>,

    /// Optional features on/off.
    pub features: Features,

    /// Keys of the phone deck.
    pub deck: Deck,

    /// On-screen widget.
    pub overlay: Overlay,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: crate::i18n::DEFAULT.code().into(),
            listen: "127.0.0.1:1935".into(),
            upstream_url: TWITCH_URL.into(),
            stream_key: String::new(),
            main_auto_start: true,
            main_video_kbps: 0,
            destinations: Vec::new(),
            delay_seconds: 30,
            start_enabled: false,
            grow_mode: "rewind".into(),
            delay_scene: String::new(),
            max_delay_seconds: 600,
            filler_fps: 2,
            presets: DEFAULT_PRESETS.to_vec(),
            censor_seconds: 10,
            replay_seconds: 10,
            clip_seconds: 30,
            clip_aired_only: false,
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
            panel_collapsed: Vec::new(),
            features: Features::default(),
            deck: Deck::default(),
            overlay: Overlay::default(),
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
    /// Also fills in a random API token and destination ids when missing.
    pub fn load_or_create(path: &Path) -> Result<Config> {
        if !path.exists() {
            Config::default().save(path)?;
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        let ids: Vec<u64> = cfg.destinations.iter().map(|d| d.id).collect();
        cfg.normalize();
        // saved when destinations got their ids, so the panel keeps seeing the same ones
        let mut changed = cfg.destinations.iter().map(|d| d.id).ne(ids);
        if cfg.api_token.is_empty() {
            cfg.api_token = new_token();
            changed = true;
        }
        if changed {
            cfg.save(path)?;
        }
        Ok(cfg)
    }

    /// Keeps values in their valid ranges.
    pub fn normalize(&mut self) {
        self.language = crate::i18n::Lang::parse(&self.language).code().into();
        self.max_delay_seconds = self.max_delay_seconds.max(1);
        self.delay_seconds = self.delay_seconds.min(self.max_delay_seconds);
        self.clip_seconds = self.clip_seconds.clamp(5, 120);
        self.replay_seconds = self.replay_seconds.clamp(3, 60);
        self.censor_seconds = self.censor_seconds.clamp(1, 120);
        self.outage_buffer_seconds = self.outage_buffer_seconds.min(300);
        self.filler_fps = self.filler_fps.clamp(1, 30);
        self.presets = normalize_presets(&self.presets, self.max_delay_seconds);
        if !["rewind", "scene", "freeze"].contains(&self.grow_mode.as_str()) {
            self.grow_mode = "rewind".into();
        }
        if !["broadcaster", "mods", "vips"].contains(&self.twitch_chat.allow.as_str()) {
            self.twitch_chat.allow = "mods".into();
        }
        self.twitch_chat.channel = crate::chat::channel_name(&self.twitch_chat.channel);
        // older configs had these switches elsewhere
        if std::mem::take(&mut self.twitch_chat.enabled) {
            self.features.chat = true;
        }
        if std::mem::take(&mut self.lan_access) {
            self.features.phone = true;
        }
        self.deck.columns = self.deck.columns.clamp(2, 6);
        self.deck.keys.retain(|k| DECK_ACTIONS.contains(&k.action.as_str()));
        self.deck.keys.truncate(48);
        self.overlay.normalize();
        self.number_destinations();
        self.main_video_kbps = video_kbps(self.main_video_kbps);
        for d in &mut self.destinations {
            d.video_kbps = video_kbps(d.video_kbps);
        }
        self.panel_collapsed.retain(|m| ALL_MODULES.contains(&m.as_str()));
        self.panel_collapsed.dedup();
        let mut seen = Vec::new();
        self.panel_modules.retain(|m| ALL_MODULES.contains(&m.as_str()) && !seen.contains(m) && {
            seen.push(m.clone());
            true
        });
    }

    /// Gives every destination a unique id (new ones, copies and ids that do not
    /// fit a JavaScript number or TOML integer get the next free one).
    fn number_destinations(&mut self) {
        const MAX_ID: u64 = (1 << 53) - 1;
        let mut next = self.destinations.iter().map(|d| d.id).filter(|&id| id <= MAX_ID).max().unwrap_or(0) + 1;
        let mut seen = std::collections::HashSet::new();
        for d in &mut self.destinations {
            if d.id == 0 || d.id > MAX_ID || !seen.insert(d.id) {
                d.id = next;
                next += 1;
                seen.insert(d.id);
            }
        }
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

    /// Seconds of already sent media the engine must keep (rewind, replay, clips).
    pub fn history_seconds(&self, delay: u32) -> u64 {
        let replay = if self.features.replay { self.replay_seconds } else { 0 };
        let clips = if self.features.clips { self.clip_seconds } else { 0 };
        let extra = replay.max(clips) as u64;
        if self.grow_mode == "rewind" || extra > 0 { delay as u64 + extra + 5 } else { 0 }
    }

    /// Outage buffer, zero when the feature is off.
    pub fn outage_seconds(&self) -> u64 {
        if self.features.outage { self.outage_buffer_seconds as u64 } else { 0 }
    }

    pub fn clips_path(&self) -> PathBuf {
        if !self.clips_dir.trim().is_empty() {
            return PathBuf::from(self.clips_dir.trim());
        }
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).unwrap_or_default();
        PathBuf::from(home).join("Videos").join("Dynamic Delay")
    }
}

/// Delay presets: the first 6 distinct values within 1..=max, sorted.
pub fn normalize_presets(presets: &[u32], max: u32) -> Vec<u32> {
    let mut v: Vec<u32> = Vec::new();
    for &s in presets.iter().filter(|&&s| (1..=max).contains(&s)) {
        if !v.contains(&s) && v.len() < 6 {
            v.push(s);
        }
    }
    v.sort_unstable();
    if v.is_empty() {
        v = DEFAULT_PRESETS.iter().copied().filter(|&s| s <= max).collect();
    }
    if v.is_empty() {
        v.push(max);
    }
    v
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
        c.destinations.push(Destination { id: 3, name: "YT".into(), url: YOUTUBE_URL.into(), key: "k".into(), enabled: false, auto_start: true, video_kbps: 0 });
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
    fn old_switches_move_to_features() {
        let old = "lan_access = true\n[twitch_chat]\nenabled = true\nchannel = \"twitch.tv/Someone\"\n";
        let mut c: Config = toml::from_str(old).unwrap();
        c.normalize();
        assert!(c.features.chat && c.features.phone);
        assert_eq!(c.twitch_chat.channel, "someone");
        let text = toml::to_string_pretty(&c).unwrap();
        assert!(!text.contains("lan_access") && !text.contains("enabled = true\nchannel"), "{text}");
    }

    #[test]
    fn deck_is_cleaned() {
        let mut c = Config::default();
        assert_eq!(c.deck.keys.len(), 9);
        c.deck.columns = 12;
        c.deck.keys.push(DeckKey { action: "rm -rf".into(), ..DeckKey::default() });
        c.normalize();
        assert_eq!(c.deck.columns, 6);
        assert_eq!(c.deck.keys.len(), 9, "unknown actions are dropped");
    }

    #[test]
    fn history_follows_features() {
        let mut c = Config::default();
        c.grow_mode = "freeze".into();
        assert_eq!(c.history_seconds(30), 30 + 30 + 5); // clips (30 s) is the longest
        c.features.clips = false;
        assert_eq!(c.history_seconds(30), 30 + 10 + 5); // replay
        c.features.replay = false;
        assert_eq!(c.history_seconds(30), 0); // nothing needs history
        c.grow_mode = "rewind".into();
        assert_eq!(c.history_seconds(30), 35);
        c.features.outage = false;
        assert_eq!(c.outage_seconds(), 0);
    }

    #[test]
    fn presets_are_kept_valid() {
        let mut c = Config::default();
        assert_eq!(c.presets, [10, 30, 60, 120]);
        c.presets = vec![300, 0, 60, 60, 5, 900, 120, 180, 240, 30, 15];
        c.normalize();
        assert_eq!(c.presets, [5, 60, 120, 180, 240, 300], "first 6 distinct in range, sorted");
        c.presets.clear();
        c.normalize();
        assert_eq!(c.presets, [10, 30, 60, 120]);
        assert_eq!(normalize_presets(&[], 45), [10, 30]);
        assert_eq!(normalize_presets(&[], 5), [5]);
        // older configs get the default presets
        let old: Config = toml::from_str("delay_seconds = 45").unwrap();
        assert_eq!(old.presets, [10, 30, 60, 120]);
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

    #[test]
    fn own_bitrate_is_off_or_in_range() {
        let mut c = Config {
            main_video_kbps: 100,
            destinations: vec![
                Destination { video_kbps: 0, ..Destination::default() },
                Destination { video_kbps: 6000, ..Destination::default() },
                Destination { video_kbps: 900_000, ..Destination::default() },
            ],
            ..Config::default()
        };
        c.normalize();
        assert_eq!(c.main_video_kbps, 500);
        assert_eq!(c.destinations.iter().map(|d| d.video_kbps).collect::<Vec<_>>(), [0, 6000, 50_000]);
        // older configs have neither field: the main destination still starts with the stream
        let old: Config = toml::from_str("upstream_url = \"rtmp://a/b\"\n").unwrap();
        assert!(old.main_auto_start);
        assert_eq!(old.main_video_kbps, 0);
    }

    #[test]
    fn destinations_get_unique_ids() {
        let mut c = Config::default();
        let d = |id: u64| Destination { id, url: YOUTUBE_URL.into(), ..Destination::default() };
        c.destinations = vec![d(0), d(5), d(5), d(0), d(u64::MAX)];
        c.normalize();
        let ids: Vec<u64> = c.destinations.iter().map(|d| d.id).collect();
        assert_eq!(ids, vec![6, 5, 7, 8, 9]);
        c.normalize();
        assert_eq!(c.destinations.iter().map(|d| d.id).collect::<Vec<_>>(), ids, "stable once given");
        // an old config without ids gets them too
        let mut old: Config = toml::from_str("[[destinations]]\nname = \"YT\"\nurl = \"rtmp://a/b\"\n").unwrap();
        old.normalize();
        assert_eq!(old.destinations[0].id, 1);
    }

    #[test]
    fn overlay_values_are_kept_valid() {
        let mut c = Config::default();
        c.overlay.style = "neon".into();
        c.overlay.accent = "red".into();
        c.overlay.scale = 5;
        c.overlay.label_on = "x".repeat(40);
        c.normalize();
        assert_eq!(c.overlay.style, "pill");
        assert_eq!(c.overlay.accent, "#e5484d");
        assert_eq!(c.overlay.scale, 50);
        assert_eq!(c.overlay.label_on.chars().count(), 24);
        // an old config without the [overlay] table gets the defaults
        let old: Config = toml::from_str("language = \"pt\"").unwrap();
        assert_eq!(old.overlay, Overlay::default());
        assert!(old.features.overlay);
        assert!(!old.overlay.show_timer);
        assert!(old.overlay.show_offline);
    }

    #[test]
    fn language_is_kept_valid() {
        for (typed, kept) in [("es", "es"), ("es-MX", "es"), ("pt-BR", "pt"), ("en", "en"), ("xx", crate::i18n::DEFAULT.code())] {
            let mut c = Config { language: typed.into(), ..Config::default() };
            c.normalize();
            assert_eq!(c.language, kept, "{typed}");
        }
    }
}
