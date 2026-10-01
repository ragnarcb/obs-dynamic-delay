# First real stream checklist / Checklist da primeira live real

## Automated regression tests

- `cargo test` and `cargo test --features pt`: relay and installer unit tests, including field-level recovery, absent values, legacy backups and manual edits.
- `python -m pip install lupa==2.6`, then `DD_TEST_BIN=target/release/obs-dynamic-delay python scripts/test_obs_bridge.py`: runs the actual bridge in LuaJIT with a file-backed OBS mock, configures several profiles through the panel's Lua handlers, and restores them with the actual Rust helper. On PowerShell, set `$env:DD_TEST_BIN = 'target/release/obs-dynamic-delay.exe'` first.
- Windows: `./scripts/test_setup.ps1` after `cargo build --release` (Inno Setup 6 required). Builds an isolated Setup, blocks uninstall with a simulated `obs64.exe`, a missing helper and a corrupt backup, then checks a successful retry. Test files/logs are kept in the printed temporary directory.
- `bash scripts/e2e-vod-track.sh`: real local RTMP with live and VOD audio, delay on/off, decode and timestamp checks. The live audio contains 440 Hz + 880 Hz; the VOD contains only 880 Hz. The test fails if 440 Hz appears in the VOD. Needs FFmpeg 8+ for Enhanced FLV; CI uses `scripts/build_test_ffmpeg.sh` to build a checksum-pinned version, while the system FFmpeg generates the fixture. `FFMPEG_BIN`, `FFPROBE_BIN` and `FFMPEG_FIXTURE` select those binaries.

CI runs these tests on pull requests. The mock does not replace a real OBS/Twitch test, and the VOD test does not yet cover every censor/replay/reconnect combination.

**English** · [Português](#português)

Use an unlisted or test stream (YouTube "unlisted", Twitch with a test title) and watch it on your phone next to the PC.

1. **Install** with OBS closed, open OBS, and check the **Dynamic Delay** panel shows *OBS ready* and no *stream key missing* warning.
2. **Start streaming** in OBS. In **Stream health**: *OBS streaming*, your platform *connected*, bitrate above 0.
3. **Watch on the phone** until the picture shows up.
4. **Turn the delay on** (30 s). The panel goes ADJUSTING and then DELAY 30.0 s. On the phone, with the default rewind mode, the stream jumps back and keeps playing.
5. **Turn it off.** The panel goes LIVE; on the phone the stream cuts to the present.
6. With the delay on, press **Delete before it airs**. The panel shows *Deleted the last ...s*; on the phone, about 30 s later, the stream holds a frame and skips that part.
7. **Instant replay** and **Save clip**: the clip appears in `Videos\Dynamic Delay` and plays in any player.
8. If you use them: **panic button** (scene + mute and back), a **scene rule**, a **chat command** from a mod, the **phone QR code**, the **Stream Deck**.
9. **Multistream:** add a second destination, start a new stream, check both show *connected*.
10. **Stop streaming** with the delay on: the platform ends the stream about 30 s later, after the delayed tail.

Something off? Open an issue with `%APPDATA%\obs-dynamic-delay\obs-dynamic-delay.log` and what the panel showed.

---

## Português

Use uma live de teste ou não listada (YouTube "não listado", Twitch com título de teste) e assista no celular ao lado do PC.

1. **Instale** com o OBS fechado, abra o OBS e confira se o painel **Delay dinâmico** mostra *OBS pronto* e nenhum aviso de *falta a chave*.
2. **Inicie a transmissão** no OBS. Em **Saúde da live**: *OBS transmitindo*, sua plataforma *conectada*, bitrate acima de 0.
3. **Assista no celular** até a imagem aparecer.
4. **Ligue o delay** (30 s). O painel passa por AJUSTANDO e depois DELAY 30,0 s. No celular, com o modo padrão (rebobinar), a live volta no tempo e continua.
5. **Desligue.** O painel volta para AO VIVO; no celular a live corta para o presente.
6. Com o delay ligado, aperte **Apagar antes de ir ao ar**. O painel mostra *Apagados os últimos ...s*; no celular, uns 30 s depois, a live segura um quadro e pula aquele trecho.
7. **Replay instantâneo** e **Salvar clipe**: o clipe aparece em `Vídeos\Dynamic Delay` e abre em qualquer player.
8. Se for usar: **botão de pânico** (cena + mudo e volta), uma **regra por cena**, um **comando no chat** vindo de um mod, o **QR code do celular**, o **Stream Deck**.
9. **Multistream:** adicione um segundo destino, comece uma live nova e confira os dois *conectados*.
10. **Pare a transmissão** com o delay ligado: a plataforma encerra uns 30 s depois, após o trecho atrasado.

Algo estranho? Abra uma issue com o `%APPDATA%\obs-dynamic-delay\obs-dynamic-delay.log` e o que o painel mostrou.

Review follow-up: Rust tests cover unreadable unrelated profiles, missing relay configuration, corrupt relay services and preservation of recovery files with explicit force. Lua tests cover legacy backup fallback and per-profile precedence. On Windows, the helper force path is checked with a corrupt backup; silent Inno uninstall still refuses removal without interactive consent. Manually verify both Yes and No in the new recovery-failure prompt, including a missing helper, and confirm Yes keeps setup.log and obs-service-backup.json; profile recovery files are retained when restoration fails and cleaned up normally when it succeeds.
Forced Inno removal also copies legacy recovery data to `obs-studio/dynamic-delay-recovery` before deletion, protecting it from unconditional deletion records left by earlier installers. If that copy fails, removal aborts rather than destroying the only backup.
Validation of this follow-up on Linux: 71 Rust tests passed in each default/pt configuration; Clippy passed in both configurations; all 10 LuaJIT tests passed with the debug helper binary. Native Windows/Inno execution remains pending. The optional `clippy --all-targets` check reports existing field_reassign_with_default lints in config.rs and control.rs tests; the CI Clippy commands pass.

Forced-success regression: install with Twitch, force-uninstall successfully, switch to YouTube and change Stream Delay, reinstall, then uninstall normally. The test checks removal of service/basic.ini backups and the journal after the first uninstall, and restoration of YouTube and the new Stream Delay value after the second. Existing forced-failure tests still require recovery data to be retained.
