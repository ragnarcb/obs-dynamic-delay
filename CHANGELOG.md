# Changelog

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

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
