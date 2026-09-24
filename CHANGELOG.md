# Changelog

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [0.7.0] - 2026-09-24

### Changed

- **New panel design:** a professional dark interface for the OBS dock. App bar with the OBS connection status, a delay hero with a large readout and a progress meter, cards with icons, grouped settings, switches instead of checkboxes, icon buttons with labels for screen readers, visible keyboard focus, toasts that do not steal focus, and reduced motion support. Works in older OBS browser docks (no newer CSS features).
- **Phone deck:** vector icons instead of emoji, the same colors as the panel, key states announced to screen readers.
- Banners are rebuilt only when they change, so their buttons are always clickable.

### Fixed

- **Streams dropping to 720p on Twitch:** streaming to the relay (a custom server) made OBS stop applying the platform's encoder rules, so it could send 10000 kbps with 4 s keyframes. The OBS script now applies them: bitrate cap (Twitch 6000, Kick 8000 kbps) and a keyframe every 2 s, keeping the resolution. It respects "Apply service settings" / "Ignore streaming service setting recommendations". The panel warns while live if OBS is above the limit.

## [0.6.0] - 2026-09-24

### Added

- **Phone deck:** phone control became a Stream Deck on the phone (or a tablet or second monitor). A full-screen grid of keys you design in the panel: delay (toggle, on, off, on with N s, ±N s), delete, replay, clip, panic, catch up, OBS scene switch, mute/unmute an audio source, start/stop streaming and recording (hold to confirm). Keys light up with the live state (delay on, scene on air, source muted, streaming, recording, panic), vibrate when pressed and shake on errors.
- Deck editor in the panel: actions, targets, text, colors, order, columns, default layout; "Open the deck on this PC".
- The OBS script reports audio sources with their mute state and whether OBS streams or records, and runs the deck's OBS actions.

## [0.5.0] - 2026-09-23

### Added

- **Every optional feature can be switched off for real** (`features` in `config.toml`, and "Features and panel" in the panel): delete before it airs, replay, clips, panic, multistream, connection drop protection, delay by scene, Twitch chat commands, phone control and the update notice. An off feature refuses its commands from every source, stops its background work (no Twitch chat connection, no LAN port, no replay/clip buffer, no extra destinations, no outage buffer, no GitHub check) and hides its block.
- Switching a feature on also adds its block to the panel.

### Changed

- "Customize panel" became "Features and panel", with an On switch and a Panel switch per feature.
- The chat and phone on/off moved from their blocks to the feature switches; older configs (`twitch_chat.enabled`, `lan_access`) are migrated.

## [0.4.1] - 2026-09-23

### Fixed

- Twitch chat commands did nothing when the channel was typed as `twitch.tv/name` or as a link: the relay joined a channel that does not exist. Any form (name, `@name`, `twitch.tv/name`, full link) now works and is saved as the plain name.
- `!delay 60` now also turns the delay on (it only changed the length).

### Added

- The log shows `[chat] joined #channel` once the chat is really joined, and Twitch notices.

## [0.4.0] - 2026-09-23

### Added

- **Modular panel:** every feature is a block; "Customize panel" picks which ones show and their order.
- **Delete before it airs:** removes the newest unaired seconds; the stream holds its last frame over the gap and keeps the delay.
- **Instant replay** of the last seconds on air, then back to the normal delay.
- **Clips** of the last seconds as MP4 (H.264 + AAC) or FLV, including what has not aired yet.
- **Multistream** to extra destinations, each on its own connection.
- **Connection drop protection:** what could not be sent is kept and sent after the reconnect, with a catch-up button.
- **Delay by scene** rules and a **panic button** (cover scene, mute all audio, delete the unaired part).
- **Twitch chat commands** (`!delay on/off/60/censor/replay/clip/panic`) for the streamer, mods or VIPs.
- **Phone control** on the local network with a QR code.
- **Stream health:** input bitrate, time live, per destination status and bitrate, alert beep.
- **Stream Deck plugin** (experimental) with live state on the keys; ready-made API links.
- Update notice when a new release is out.
- Windowed installer (native dialogs); the text installer stays available with `--console`.
- Hotkeys for delete, replay, clip and panic.

### Security

- Every HTTP API call now needs an access token, generated on first start; websites open in the browser can no longer control the relay or change the destination. The API never returns stream keys or the token.

### Changed

- The relay writes `dock.html` (with the token) at every start, so the dock always matches the installed version.
- Per destination buffers bound memory on a slow network.

## [0.3.1] - 2026-09-23

### Added

- "Developed by ragnarcb" credit with a link to the author's GitHub in the panel, the installer, the OBS script description and the READMEs. In the OBS dock, the link opens in the system browser.

## [0.3.0] - 2026-09-23

### Added

- English and Portuguese (Brazil) for everything the user sees: panel, installer, OBS script (hotkeys, buttons, messages) and relay status.
- Language picker in the panel settings; the `language` setting is stored in `config.toml`.
- Two installers per release: `Dynamic-Delay-Installer.exe` (English) and `Instalar-Delay-Dinamico.exe` (Portuguese).

### Changed

- Repository documentation in English (`README.md`), with a Portuguese version (`README.pt-BR.md`).
- The OBS dock is named "Dynamic Delay" in English and "Delay dinâmico" in Portuguese; reinstalling in another language replaces it.

## [0.2.0] - 2026-09-23

### Added

- Choice of what viewers see when the delay is switched on or increased (panel > Settings):
  - **Rewind** (new default): the stream jumps back instantly and replays the last seconds, with no freeze.
  - **Show an OBS scene**: the script switches to the chosen scene, its frame stays on screen while the delay builds up, then OBS switches back to the previous scene (studio mode included).
  - **Freeze the picture**: the 0.1.0 behaviour.
- The panel lists the OBS scenes.

## [0.1.0] - 2026-09-23

First release.

### Added

- Local RTMP relay between OBS and the platform, with a delay that turns on, off and changes length while live.
  - Growing the delay freezes on the next keyframe, with muted AAC audio, until the buffer is full.
  - Shrinking cuts at the most recent keyframe.
  - Continuous timestamps, B-frames included; H.264, HEVC/AV1 (Enhanced RTMP) and AAC.
- RTMP and RTMPS output (Twitch, YouTube, Kick and others) with automatic reconnection.
- "Dynamic Delay" panel as an OBS dock: state, real delay, presets, destination and key settings.
- Lua script for OBS: hotkeys, starts and closes the relay with OBS, configures and restores the stream settings.
- Two-click installer: imports destination and key, configures OBS, turns off the built-in delay, adds script and dock, with backups; uninstall option.
- HTTP API (Stream Deck, bots) and UDP commands.
