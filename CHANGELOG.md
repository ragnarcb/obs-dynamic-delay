# Changelog

Format based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Fixed

- Windows uninstall now stops before deleting files if OBS is open, the recovery helper cannot start, or restoration fails. Recovery files and the error log remain available for a retry.
- Configuring OBS from the panel saves each profile's own original service. A failed backup prevents reconfiguration; restoring a profile cannot load another profile's stream key.
- The installer and Lua bridge journal each changed setting's previous and applied values. Uninstall restores only those fields that still match the applied values, preserves later manual edits and missing defaults, and writes configuration files atomically. Ambiguous encoder backups from older versions remain available for manual recovery.

### Tests

- CI runs the Lua bridge with multiple OBS profiles and verifies its recovery files with the Rust uninstaller; Windows also exercises the real Inno uninstaller with OBS open, a missing helper, a failed restoration and a successful retry.
- CI runs the VOD end-to-end test with a pinned FFmpeg 8 build. It checks decoding, timestamps and that a live-only audio tone never reaches the VOD track.

## [0.13.0] - 2026-09-28

### Added

- **Twitch VOD track works with Dynamic Delay.** The Setup turns on OBS' own switch that shows and sends the VOD track on a custom server (`EnableCustomServerVodTrack`, OBS 30 or newer), and the relay forwards the second audio track (Enhanced RTMP multitrack audio, OBS 30.2 or newer) with the same delay: delete before it airs, replay, rewind, freeze (with silence of its own) and multistream destinations started mid-stream included. Clips keep only the stream's own audio. Uninstalling puts the switch back as it was.
- `scripts/e2e-vod-track.sh`: end-to-end test with a VOD track (ffmpeg plays OBS and the platform).

### Fixed

- A second audio track sent by OBS was taken for a normal audio frame: its codec header was dropped at stream start and never sent to a destination that started mid-stream, and it could be written into clips.
- Uninstalling with no copy of the original stream settings left (an install whose copy was removed or consumed) left OBS streaming to the relay that was gone: OBS now goes straight to the destination (the Twitch service itself for Twitch).
- Uninstalling removes the script from every folder it was loaded from (copies left by an old install or a moved folder), not only the current one.
- Settings files keep their decimal numbers exactly (JSON floats are read and written back bit for bit).

## [0.12.1] - 2026-09-28

### Fixed

- **Uninstalling puts OBS back exactly as it was.** Before, it only restored the stream service of the profile in use at uninstall time. Now every profile that streams to the relay gets its own original settings back, and so do OBS' own Stream Delay and the bitrate and keyframe interval the OBS script capped for the platform (a value changed by hand later is kept). The original stream settings are also kept inside the OBS profile, so they survive the app folder being deleted, and no backup files are left behind.

### Documentation

- **Uninstall the right way:** only from Windows Settings > Apps, never with Geek Uninstaller, Revo or similar tools (they delete the files without giving OBS its settings back). Steps to fix OBS by hand after such a removal, and the `Error opening file: (null)` script error, in the READMEs and on the Discord server.
- Known limitation: while Dynamic Delay is installed, OBS streams to a custom server and hides the **Twitch VOD track** and the connected account options.

## [0.12.0] - 2026-09-27

### Added

- **On-screen widget while not live:** it shows the set delay before you go live, so it can be placed and styled in OBS offline ("Show it while not live too", on by default; URL parameter `show_offline=0` hides it offline).
- **Spanish:** a third language in the panel, phone deck, on-screen widget, OBS script, relay messages, Setup and a `README.es.md`. Chat command words follow the chosen language (in Spanish `apagar` turns the delay off; in Portuguese it still deletes), English words work in every language.
- **Auto-off timer:** "Turn off by itself" in the Delay block (15 to 120 min), `!delay 60 20m` / `!delay timer 20` in chat, `autooff` / `onfor` commands, a phone deck key, and an optional countdown on the on-screen widget. The countdown runs while the delay is on; turning it off by hand cancels it.
- **Your own delay buttons:** up to 6 presets (Edit the delay buttons), with the memory the longest one needs at the current bitrate.
- **Push events for bots** at `/api/events` (Server-Sent Events): delay, phase, replay, censor, clips, panic, destinations and auto-off.
- Discord community link in the panel footer and the READMEs.
- Facebook in the Settings platform list.

### Fixed

- Saving Settings could replace an account-specific Kick ingest address with the default one, and Twitch regional ingests could be taken for Kick (wrong bitrate cap). Kick and Twitch hosts are now told apart precisely, in the panel and in the OBS script.
- **Network too slow:** the catch-up protection for a destination that cannot keep up never kicked in, so memory grew by about 1 MB/s at 8 Mbps until the link recovered. The backlog is now limited as designed.
- A stalled platform connection now reconnects after 15 s without progress (it could hang for minutes), and a half-open OBS connection no longer blocks OBS from reconnecting.
- Settings saved at the same time no longer overwrite each other, and partial updates (for example one feature switch) keep the other values.
- Renaming a multistream destination kept its key only by name: destinations now have a stable id.
- A corrupt video header could crash the clip size parser.
- The connection-drop beep stopped working after a few drops; a second panic could leave sources muted after the panic ended; bare `!delay` in chat toggled the delay (now it does nothing, use `!delay toggle`); phone deck keys for the main destination named "principal" did not light.
- The panel shows errors from OBS in the error style, gives up on a hung request after 5 s, and opens links in the browser when the relay cannot.

### Security

- The access token is compared in constant time, and the OBS script's stream import over UDP is only accepted from the script itself.

### Accessibility

- Phone deck keys work with the keyboard and the page can be zoomed; the connection chip is not re-announced every second; Go live / Stop buttons name their destination.

## [0.11.1] - 2026-09-27

### Changed

- **Every panel block can be minimized**, like Settings and Features and panel: click its title bar. Minimized blocks stay minimized after OBS restarts (`panel_collapsed` in the config). A minimized Delay block still shows the state and the seconds in its title bar.

## [0.11.0] - 2026-09-26

### Added

- **On-screen widget:** a minimal badge for viewers (OBS Browser Source at `/overlay`) showing that the delay is on and how many seconds, with states for building up / going back to live (progress line) and instant replay. Customizable in a new panel block with a live preview: style (pill, card, text only), theme (dark, light, outline), which parts show (dot, text, seconds, progress), own texts, color, size, alignment, font, time format, hide while off. "Add to the current OBS scene" creates the Browser Source; URL parameters override any setting per source. The page needs no token and only exposes the delay state; it can be switched off under Features and panel.

## [0.10.0] - 2026-09-25

### Added

- **Multistream, like the dedicated add-ons:** every destination starts and stops on its own while live (Go live / Stop in the panel, a phone deck key, `/api/output/{id}/{start,stop,toggle}`), can start together with the stream or not, and settings changes apply to the running stream (new destinations go live, removed ones stop, changed keys reconnect). Platform presets fill the server address (Twitch, YouTube, Kick, Facebook).
- `DD_TRACE=<file>` writes every packet sent to the platforms (wall clock, timestamp), to study pacing.

### Fixed

- **Stalls and "loading" on Kick when changing the delay:** turning the delay back on after it was cut back to live (rewind mode) replayed across the cut part, so the timeline jumped several seconds and keyframes were 10 s apart. Players of low latency platforms such as Kick (Amazon IVS) wait for the missing part. A rewind now never crosses a cut; what the history cannot cover is held on the last frame.
- RTMPS connections (Kick, YouTube, Facebook) are flushed after every batch of packets.
- A destination started mid-stream, or reconnecting, starts cleanly: codec headers, keyframe and audio together at timestamp 0, instead of audio running ahead of the picture.

## [0.9.1] - 2026-09-25

### Added

- **Support the project:** crypto donation addresses (BTC, ETH, SOL) with QR codes in the READMEs, and a "Support the project" link in the panel footer.

## [0.9.0] - 2026-09-25

### Added

- **Professional Windows Setup:** one `Dynamic-Delay-Setup.exe` for English and Portuguese (language picker), built with Inno Setup: welcome, license and progress pages with the app artwork, per-user install without administrator rights, a check that OBS is closed, the list of what was done in OBS and "Open OBS now" on the last page, an entry in Windows Settings > Apps, clean uninstall (restores the stream settings, asks whether to delete the settings) and updates over the old version.
- The program has an icon, version information (publisher, product, copyright) and a Windows manifest.
- Releases are built and published by a GitHub workflow on every tag, with `SHA256SUMS.txt`. Code signing is ready: it turns on as soon as a certificate is configured.
- `--install --quiet --lang en|pt`, `--uninstall --quiet` and `--launch-obs`, used by the Setup.

### Changed

- The Portuguese installer texts have their accents.
- **New logo and icons:** a flat mark (a play with its delayed echo and the red on-air light) drawn in vector, replacing the gradient stopwatch. The panel, the phone deck and the Stream Deck plugin use Phosphor Icons (MIT) instead of Lucide and Unicode symbols; primary buttons are monochrome, color only marks a state (live, delay, error). All images are rendered from the vector sources by `installer/make_art.py`.

## [0.8.1] - 2026-09-25

### Fixed

- **Update notice:** it checked only once, when the OBS dock opened (plus a 1 hour cache), so a release published while OBS stayed open was never shown, and nothing told you the check was working. The relay now checks GitHub itself when it starts, every 6 hours (15 minutes after a failure) and on **Check now**. Settings > General > Updates shows "up to date" with the time of the last check, or the new version with a download button; a new version also raises a notice and a line in the script status inside OBS. Off means no connection at all. New API: `/api/update/check`.

## [0.8.0] - 2026-09-25

### Added

- **Clip options:** the block shows the size and frame rate clips come out at (they copy the stream), offers **Switch OBS to 60 FPS** when OBS runs below 50 FPS, quick lengths (15/30/60/90/120 s), **Only what viewers already saw** and an editable clips folder.
- The OBS script reports the OBS output size and frame rate, and can set the frame rate (only with the stream and the recording stopped). New API: `/api/obs/fps/{n}`.
- Stream health reports the frames per second and picture size received from OBS.

### Changed

- Clips are written with a constant frame rate (exact 60/1, 30/1, 59.94 grid instead of 16/17 ms steps) and the audio at its own sample rate, for video editors and phone apps.
- The clip size comes from the H.264 header, so it is right even when OBS sends no metadata (it used to fall back to 1280x720).

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
