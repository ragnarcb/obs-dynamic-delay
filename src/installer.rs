//! Installer shown when the exe is double-clicked (native dialogs on Windows,
//! console with --console or --install): copies the relay and
//! the OBS script to a fixed folder and wires them into OBS (script, dock,
//! stream settings). OBS rewrites its config on exit, so it must be closed.

use std::hash::BuildHasher;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::config::{Config, TWITCH_URL, YOUTUBE_URL, destination_from_obs};
use crate::i18n::{self, Lang};
use crate::t;

const LUA: &str = include_str!("../obs/obs-dynamic-delay.lua");

fn dock_title() -> &'static str {
    i18n::dock_title(i18n::get())
}

/// True for our dock in any language.
fn is_our_dock(d: &Value) -> bool {
    [Lang::En, Lang::Pt, Lang::Es].iter().any(|l| d["title"] == i18n::dock_title(*l))
}
const EXE_NAME: &str = if cfg!(windows) { "obs-dynamic-delay.exe" } else { "obs-dynamic-delay" };

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ask,
    Install,
    Uninstall,
}

/// Steps done so far, shown in the final message of the windowed installer.
static STEPS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Double-click entry point: native dialogs on Windows, the console elsewhere.
pub fn start() -> Result<()> {
    #[cfg(windows)]
    {
        if !std::env::args().any(|a| a == "--console") {
            gui::run();
            return Ok(());
        }
    }
    wizard(Mode::Ask)
}

/// Install or uninstall without any question, for the Setup program. Returns
/// the process exit code; the steps and any error also go to `setup.log`.
pub fn quiet(mode: Mode) -> i32 {
    let result = (|| -> Result<PathBuf> {
        let p = Paths::detect()?;
        if obs_running() {
            bail!("{}", t!("Close OBS and try again.", "Feche o OBS e tente de novo.", "Cierra OBS e inténtalo de nuevo."));
        }
        match mode {
            Mode::Uninstall => uninstall_with_force(&p, std::env::args().any(|a| a == "--force"))?,
            _ => {
                install(&p, false)?;
            }
        }
        Ok(p.install_dir)
    })();
    let (code, text) = match &result {
        Ok(_) => (0, STEPS.lock().unwrap().join("\n")),
        Err(e) => (1, format!("{}\n{} {e:#}", STEPS.lock().unwrap().join("\n"), t!("ERROR:", "ERRO:", "ERROR:"))),
    };
    println!("{text}");
    let dir = match &result {
        Ok(d) => Some(d.clone()),
        Err(_) => Paths::detect().ok().map(|p| p.install_dir),
    };
    if let Some(d) = dir {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("setup.log"), &text);
    }
    code
}

pub fn wizard(mode: Mode) -> Result<()> {
    println!("==============================================");
    println!("{}", t!("  Dynamic Delay for OBS", "  Delay dinâmico para OBS", "  Delay dinámico para OBS"));
    println!("{}", t!("  Developed by ragnarcb", "  Desenvolvido por ragnarcb", "  Desarrollado por ragnarcb"));
    println!("  {}", crate::control::AUTHOR_URL);
    println!("==============================================\n");
    let r = run(mode);
    if let Err(e) = &r {
        println!("\n{} {e:#}", t!("ERROR:", "ERRO:", "ERROR:"));
    }
    if mode == Mode::Ask {
        ask(&t!("\nPress Enter to close.", "\nPressione Enter para fechar.", "\nPresiona Enter para cerrar."));
    }
    r
}

fn run(mode: Mode) -> Result<()> {
    let p = Paths::detect()?;
    let installed = p.is_installed();
    let mode = match mode {
        Mode::Ask if installed => {
            println!("{}", t!("Already installed in {}.", "Já está instalado em {}.", "Ya está instalado en {}.", p.install_dir.display()));
            match ask(&t!(
                "Type 1 to update/reinstall, 2 to uninstall, or Enter to quit: ",
                "Digite 1 para atualizar/reinstalar, 2 para desinstalar, ou Enter para sair: ",
                "Escribe 1 para actualizar/reinstalar, 2 para desinstalar, o Enter para salir: "
            ))
            .as_str()
            {
                "1" => Mode::Install,
                "2" => Mode::Uninstall,
                _ => return Ok(()),
            }
        }
        Mode::Ask => {
            let dock = dock_title();
            println!("{}", t!("This installs the dynamic delay into your OBS:", "Isto vai instalar o delay dinâmico no seu OBS:", "Esto instala el delay dinámico en tu OBS:"));
            println!("{}", t!(
                "  - adds the script and a \"{dock}\" panel to the OBS window",
                "  - adiciona o script e um painel \"{dock}\" na tela do OBS",
                "  - agrega el script y un panel \"{dock}\" a la ventana de OBS"
            ));
            println!("{}", t!(
                "  - makes OBS stream through the relay (your current settings are backed up)",
                "  - faz o OBS transmitir pelo relay (a configuração atual fica salva)",
                "  - hace que OBS transmita a través del relay (tu configuración actual queda respaldada)"
            ));
            println!("{}", t!("  - turns off OBS' built-in Stream Delay\n", "  - desliga o Stream Delay nativo do OBS\n", "  - desactiva el Retraso de transmisión nativo de OBS\n"));
            if ask(&t!("Install now? [Y/n] ", "Instalar agora? [S/n] ", "¿Instalar ahora? [S/n] ")).to_lowercase().starts_with('n') {
                return Ok(());
            }
            Mode::Install
        }
        m => m,
    };
    wait_obs_closed();
    match mode {
        Mode::Uninstall => uninstall_with_force(&p, std::env::args().any(|a| a == "--force")),
        _ => {
            install(&p, true)?;
            if ask(&t!("Open OBS now? [Y/n] ", "Abrir o OBS agora? [S/n] ", "¿Abrir OBS ahora? [S/n] ")).to_lowercase().starts_with('n') {
                return Ok(());
            }
            launch_obs();
            Ok(())
        }
    }
}

struct Paths {
    obs_dir: PathBuf,
    install_dir: PathBuf,
}

impl Paths {
    fn detect() -> Result<Paths> {
        let obs_dir = match std::env::var_os("DD_OBS_CONFIG_DIR") {
            Some(d) => PathBuf::from(d),
            None => default_obs_dir()?,
        };
        if !obs_dir.join("basic").exists() {
            bail!(
                "{}",
                t!(
                    "OBS settings not found in {} (open OBS at least once)",
                    "não achei a configuração do OBS em {} (abra o OBS pelo menos uma vez)",
                    "no se encontró la configuración de OBS en {} (abre OBS al menos una vez)",
                    obs_dir.display()
                )
            );
        }
        let install_dir = match std::env::var_os("DD_INSTALL_DIR") {
            Some(d) => PathBuf::from(d),
            None => obs_dir.parent().context("OBS config dir has no parent")?.join("obs-dynamic-delay"),
        };
        Ok(Paths { obs_dir, install_dir })
    }

    fn lua(&self) -> PathBuf {
        self.install_dir.join("obs-dynamic-delay.lua")
    }
    fn config(&self) -> PathBuf {
        self.install_dir.join("config.toml")
    }
    fn service_backup(&self) -> PathBuf {
        self.install_dir.join("obs-service-backup.json")
    }
    fn dock_html(&self) -> PathBuf {
        self.install_dir.join("dock.html")
    }
    fn user_ini(&self) -> PathBuf {
        let u = self.obs_dir.join("user.ini");
        if u.exists() { u } else { self.obs_dir.join("global.ini") }
    }
    fn profile_dir(&self) -> Result<PathBuf> {
        let profiles = self.obs_dir.join("basic").join("profiles");
        let ini = std::fs::read_to_string(self.user_ini()).unwrap_or_default();
        if let Some(dir) = ini_get(&ini, "Basic", "ProfileDir") {
            let p = profiles.join(dir);
            if p.exists() {
                return Ok(p);
            }
        }
        std::fs::read_dir(&profiles)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.is_dir())
            .context(t!("no OBS profile found", "nenhum perfil do OBS encontrado", "no se encontró ningún perfil de OBS"))
    }
    fn profiles(&self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(self.obs_dir.join("basic").join("profiles"))? {
            let entry = entry?;
            if entry.metadata()?.is_dir() { paths.push(entry.path()); }
        }
        Ok(paths)
    }
    fn scene_collections(&self) -> Result<Vec<PathBuf>> {
        let dir = self.obs_dir.join("basic").join("scenes");
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let mut paths = Vec::new();
        for entry in rd {
            let path = entry?.path();
            if path.extension().is_some_and(|x| x == "json") { paths.push(path); }
        }
        Ok(paths)
    }
    fn is_installed(&self) -> bool {
        self.lua().exists()
            && self.scene_collections().unwrap_or_default().iter().any(|c| {
                read_json(c).is_ok_and(|v| script_index(&v, &self.lua()).is_some())
            })
    }
}

fn default_obs_dir() -> Result<PathBuf> {
    if cfg!(windows) {
        Ok(PathBuf::from(std::env::var_os("APPDATA").context("APPDATA not set")?).join("obs-studio"))
    } else {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME not set")?);
        Ok(if cfg!(target_os = "macos") {
            home.join("Library/Application Support/obs-studio")
        } else {
            home.join(".config/obs-studio")
        })
    }
}

/// Installs or updates. `interactive` asks for the destination on the console when
/// none could be imported (the windowed installer leaves that to the panel).
fn install(p: &Paths, interactive: bool) -> Result<Config> {
    std::fs::create_dir_all(&p.install_dir)?;

    // 1. files
    let me = std::env::current_exe()?;
    let target_exe = p.install_dir.join(EXE_NAME);
    if !same_file(&me, &target_exe) {
        copy_with_retry(&me, &target_exe)?;
    }
    std::fs::write(p.lua(), LUA)?;
    let mut cfg = Config::load_or_create(&p.config())?;
    // the installer's language (English or Portuguese build, or --lang) becomes the app language
    cfg.language = i18n::get().code().to_string();
    step(&t!("files copied to {}", "arquivos copiados para {}", "archivos copiados a {}", p.install_dir.display()));

    // 2. stream settings of the current profile
    let profile = p.profile_dir()?;
    let service_path = profile.join("service.json");
    let relay_server = format!("rtmp://{}/live", cfg.listen);
    let service = read_json(&service_path).unwrap_or(json!({}));
    let server = service["settings"]["server"].as_str().unwrap_or("");
    let mut imported = false;
    if !server.contains(&cfg.listen) {
        if service_path.exists() {
            std::fs::copy(&service_path, p.service_backup())?;
            backup_once(&service_path)?;
        }
        let key = service["settings"]["key"].as_str().unwrap_or("");
        let svc = service["settings"]["service"].as_str().unwrap_or("");
        if let Some(url) = destination_from_obs(server, svc) {
            cfg.upstream_url = url;
            imported = true;
        }
        if !key.is_empty() {
            cfg.stream_key = key.to_string();
        }
        if imported {
            step(&t!("destination imported from OBS: {}", "destino importado do OBS: {}", "destino importado de OBS: {}", cfg.upstream_url));
        }
    }
    if interactive && !imported && cfg.stream_key.is_empty() {
        ask_destination(&mut cfg);
    }
    cfg.save(&p.config())?;
    write_json(
        &service_path,
        &json!({
            "type": "rtmp_custom",
            "settings": { "server": relay_server, "key": "delay", "use_auth": false, "bwtest": false }
        }),
    )?;
    step(&t!("OBS now streams to {relay_server}", "OBS agora transmite para {relay_server}", "OBS ahora transmite a {relay_server}"));

    // 3. disable OBS' own stream delay
    let basic_ini = profile.join("basic.ini");
    if basic_ini.exists() {
        set_tracked_ini(&basic_ini, "Output", "DelayEnable", "false")?;
        step(&t!("OBS' built-in Stream Delay turned off", "Stream Delay nativo do OBS desligado", "Retraso de transmisión nativo de OBS desactivado"));
    }

    // 4. script in every scene collection
    let lua = p.lua();
    for c in p.scene_collections()? {
        let mut v = read_json(&c)?;
        // drop copies of this script loaded from other folders (older installs)
        let before = v["modules"]["scripts-tool"].as_array().map_or(0, |a| a.len());
        if let Some(arr) = v["modules"]["scripts-tool"].as_array_mut() {
            let want = slash(&lua).to_lowercase();
            arr.retain(|s| {
                let path = s["path"].as_str().unwrap_or("").replace('\\', "/").to_lowercase();
                !path.ends_with("/obs-dynamic-delay.lua") || path == want
            });
        }
        let removed = before - v["modules"]["scripts-tool"].as_array().map_or(0, |a| a.len());
        if removed > 0 {
            backup_once(&c)?;
            write_json(&c, &v)?;
            step(&t!("old copy of the script removed from OBS", "cópia antiga do script removida do OBS", "copia antigua del script eliminada de OBS"));
        }
        if script_index(&v, &lua).is_none() {
            backup_once(&c)?;
            if !v["modules"].is_object() {
                v["modules"] = json!({});
            }
            if !v["modules"]["scripts-tool"].is_array() {
                v["modules"]["scripts-tool"] = json!([]);
            }
            v["modules"]["scripts-tool"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "path": slash(&lua), "settings": {} }));
            write_json(&c, &v)?;
        }
    }
    step(&t!(
        "script added to OBS (hotkeys in Settings > Hotkeys > Dynamic Delay)",
        "script adicionado ao OBS (atalhos em Configurações > Atalhos > Delay dinâmico)",
        "script agregado a OBS (atajos en Configuración > Atajos > Delay dinámico)"
    ));

    // 5. dock with the control panel
    // the relay rewrites this file at every start; write it now so the dock works right away
    std::fs::write(p.dock_html(), crate::control::dock_html(&cfg))?;
    let dock_url = format!("file:///{}", slash(&p.dock_html()).trim_start_matches('/'));
    // Journal only the flag we change, before writing it. The dock is removed by identity.
    set_tracked_ini(&p.user_ini(), "General", "EnableCustomServerVodTrack", "true")?;
    edit_ini(&p.user_ini(), |t| {
        let mut docks: Vec<Value> = ini_get(t, "BasicWindow", "ExtraBrowserDocks")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        docks.retain(|d| !is_our_dock(d));
        docks.push(json!({ "title": dock_title(), "url": dock_url, "uuid": uuid() }));
        ini_set(t, "BasicWindow", "ExtraBrowserDocks", &Value::Array(docks).to_string())
    })?;
    let dock = dock_title();
    step(&t!("\"{dock}\" panel added (Docks menu)", "painel \"{dock}\" adicionado (menu Docks)", "panel \"{dock}\" agregado (menú Docks)"));

    println!("\n{}", t!("Done!", "Pronto!", "¡Listo!"));
    if cfg.stream_key.is_empty() {
        println!("{}", t!(
            "Only the stream key is missing: fill it in the \"{dock}\" panel inside OBS.",
            "Falta só a chave de transmissão: preencha no painel \"{dock}\" dentro do OBS.",
            "Solo falta la clave de transmisión: complétala en el panel \"{dock}\" dentro de OBS."
        ));
    }
    Ok(cfg)
}

fn uninstall_with_force(p: &Paths, force: bool) -> Result<()> {
    // Force only tolerates a failure; successful recovery must consume its backups
    // so a later install cannot reuse originals from the previous installation.
    match uninstall(p) {
        Err(e) if force => {
            step(&format!("WARNING: incomplete OBS recovery: {e:#}"));
            step("Forced removal allowed; remaining recovery files kept in OBS profiles and the application folder. Manual OBS recovery may be required.");
            Ok(())
        }
        result => result,
    }
}

fn uninstall(p: &Paths) -> Result<()> {
    // Keep all recovery files until every restoration succeeds. A retry is idempotent.
    let mut cleanup = Vec::new();
    for c in p.scene_collections()? {
        let mut v = read_json(&c)?;
        // this script from any folder: also copies left by an old install or a folder moved by hand
        if let Some(arr) = v["modules"]["scripts-tool"].as_array_mut() {
            let before = arr.len();
            arr.retain(|s| !s["path"].as_str().unwrap_or("").replace('\\', "/").to_lowercase().ends_with("/obs-dynamic-delay.lua"));
            if arr.len() != before {
                write_json(&c, &v)?;
            }
        }
    }
    step(&t!("script removed from OBS", "script removido do OBS", "script eliminado de OBS"));
    // the VOD track switch as it was before the install (not there = removed again)
    let user_ini = p.user_ini();
    let user_backup = backup_of(&user_ini);
    let legacy_user = if !changes_of(&user_ini).exists() && user_backup.exists() {
        Some(std::fs::read_to_string(&user_backup)?)
    } else {
        None
    };
    if p.user_ini().exists() {
        edit_ini(&p.user_ini(), |t| {
            let mut docks: Vec<Value> = ini_get(t, "BasicWindow", "ExtraBrowserDocks")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            docks.retain(|d| !is_our_dock(d));
            let t = ini_set(t, "BasicWindow", "ExtraBrowserDocks", &Value::Array(docks).to_string());
            // Legacy installs always set this flag to true. Without a backup (or after
            // a previous successful uninstall), leave the user's setting alone.
            if let Some(before) = &legacy_user
                && ini_get(&t, "General", "EnableCustomServerVodTrack").as_deref() == Some("true")
            {
                match ini_get(before.trim_start_matches('\u{feff}'), "General", "EnableCustomServerVodTrack") {
                    Some(v) => return ini_set(&t, "General", "EnableCustomServerVodTrack", &v),
                    None => return ini_remove(&t, "General", "EnableCustomServerVodTrack"),
                }
            }
            t
        })?;
        restore_tracked_ini(&user_ini, &[("General", "EnableCustomServerVodTrack")])?;
        cleanup.push(changes_of(&user_ini));
        step(&t!("panel removed", "painel removido", "panel eliminado"));
    }
    // every profile that streams to the relay gets its own original settings back
    let saved = std::fs::read_to_string(p.config()).ok().and_then(|t| toml::from_str::<Config>(&t).ok());
    let listen = saved.as_ref().map_or_else(|| Config::default().listen, |c| c.listen.clone());
    let mut restored = 0;
    for prof in p.profiles()? {
        let svc = prof.join("service.json");
        let kept = backup_of(&svc);
        let to_relay = match read_json(&svc) {
            Ok(v) => v["settings"]["server"].as_str().is_some_and(|s| s.contains(&listen)),
            Err(e) => {
                // A damaged unrelated profile must not prevent uninstall. Retain all
                // its evidence; a readable relay address in malformed JSON is unsafe.
                let relay_evidence = std::fs::read_to_string(&svc).ok()
                    .is_some_and(|text| text.contains(&listen));
                if relay_evidence { return Err(e).context("damaged relay profile; recovery files kept"); }
                step(&format!("WARNING: skipping unreadable profile {}: {e:#}", prof.display()));
                continue;
            }
        };
        if to_relay {
            let current = p.profile_dir().ok().is_some_and(|c| c == prof);
            if kept.exists() {
                write_json(&svc, &original_service(&kept, &listen)?)?;
                restored += 1;
            } else if current && p.service_backup().exists() {
                // installs made before the copy in the profile existed
                write_json(&svc, &original_service(&p.service_backup(), &listen)?)?;
                restored += 1;
            } else if let Some(c) = &saved {
                // no copy at all (removed by hand, or an old test install): stream straight to
                // the relay's own destination instead of a relay that is gone
                write_json(&svc, &service_for(&c.upstream_url, &c.stream_key))?;
                restored += 1;
            } else {
                bail!("cannot restore {}: no original service or relay configuration; recovery files kept", svc.display());
            }
        }
        cleanup.push(kept);
        restore_profile(&prof, &mut cleanup)?;
    }
    if restored > 0 {
        step(&t!("original stream settings restored", "configuração de transmissão original restaurada", "configuración de transmisión original restaurada"));
    }
    step(&t!(
        "OBS settings restored; later manual changes kept",
        "Configurações do OBS restauradas; alterações manuais posteriores mantidas",
        "Configuración de OBS restaurada; cambios manuales posteriores conservados"
    ));
    for f in p.scene_collections()?.into_iter().chain([p.user_ini()]) {
        cleanup.push(backup_of(&f));
    }
    for f in cleanup {
        match std::fs::remove_file(&f) {
            Ok(()) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e).with_context(|| format!("removing {}", f.display())),
        }
    }
    println!(
        "\n{}",
        t!(
            "Uninstalled. The folder {} can be deleted.",
            "Desinstalado. A pasta {} pode ser apagada.",
            "Desinstalado. La carpeta {} se puede borrar.",
            p.install_dir.display()
        )
    );
    Ok(())
}

fn ask_destination(cfg: &mut Config) {
    println!("\n{}", t!("Where do you stream to?", "Para onde você transmite?", "¿A dónde transmites?"));
    println!("{}", t!("  1 = Twitch   2 = YouTube   3 = Kick / other (paste URL)", "  1 = Twitch   2 = YouTube   3 = Kick / outra (colar URL)", "  1 = Twitch   2 = YouTube   3 = Kick / otra (pegar URL)"));
    match ask(&t!("Option (Enter = Twitch): ", "Opção (Enter = Twitch): ", "Opción (Enter = Twitch): ")).as_str() {
        "2" => cfg.upstream_url = YOUTUBE_URL.into(),
        "3" => {
            let url = ask(&t!("Server URL (rtmp:// or rtmps://): ", "URL do servidor (rtmp:// ou rtmps://): ", "URL del servidor (rtmp:// o rtmps://): "));
            if url.starts_with("rtmp://") || url.starts_with("rtmps://") {
                cfg.upstream_url = url;
            } else {
                println!("{}", t!("Invalid URL, using Twitch. Change it later in the panel.", "URL inválida, usando Twitch. Troque depois no painel.", "URL inválida, se usará Twitch. Cámbiala después en el panel."));
                cfg.upstream_url = TWITCH_URL.into();
            }
        }
        _ => cfg.upstream_url = TWITCH_URL.into(),
    }
    cfg.stream_key = ask(&t!("Stream key (Enter = fill in later in the panel): ", "Chave de transmissão (Enter = preencher depois no painel): ", "Clave de transmisión (Enter = completarla después en el panel): "));
}

fn step(msg: &str) {
    println!("  [ok] {msg}");
    STEPS.lock().unwrap().push(msg.to_string());
}

fn ask(prompt: &str) -> String {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut s = String::new();
    let _ = std::io::stdin().lock().read_line(&mut s);
    s.trim().to_string()
}

fn obs_running() -> bool {
    if std::env::var_os("DD_SKIP_OBS_CHECK").is_some() {
        return false;
    }
    if cfg!(windows) {
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq obs64.exe", "/NH"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("obs64.exe"))
    } else {
        std::process::Command::new("pgrep").args(["-x", "obs"]).status().is_ok_and(|s| s.success())
    }
}

fn wait_obs_closed() {
    if !obs_running() {
        return;
    }
    println!("\n{}", t!(
        "Close OBS to continue (it rewrites its settings when it closes).",
        "Feche o OBS para continuar (ele regrava as configurações ao fechar).",
        "Cierra OBS para continuar (reescribe su configuración al cerrarse)."
    ));
    print!("{}", t!("Waiting for OBS to close", "Aguardando o OBS fechar", "Esperando a que OBS se cierre"));
    while obs_running() {
        print!(".");
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_secs(1));
    }
    println!(" ok\n");
    std::thread::sleep(Duration::from_millis(500));
}

pub fn launch_obs() {
    let candidates: &[&str] = if cfg!(windows) {
        &[r"C:\Program Files\obs-studio\bin\64bit\obs64.exe", r"C:\Program Files (x86)\obs-studio\bin\64bit\obs64.exe"]
    } else if cfg!(target_os = "macos") {
        &["/Applications/OBS.app/Contents/MacOS/OBS"]
    } else {
        &["/usr/bin/obs"]
    };
    for c in candidates {
        let exe = Path::new(c);
        if exe.exists() {
            let _ = std::process::Command::new(exe).current_dir(exe.parent().unwrap()).spawn();
            return;
        }
    }
    println!("{}", t!("OBS not found, please open it yourself.", "Nao achei o OBS para abrir, abra ele normalmente.", "No se encontró OBS, ábrelo tú mismo."));
}

fn copy_with_retry(from: &Path, to: &Path) -> Result<()> {
    if std::fs::copy(from, to).is_ok() {
        return Ok(());
    }
    // An installed relay may still be running: ask it to quit, then retry.
    if let Ok(cfg) = Config::load_or_create(&to.with_file_name("config.toml"))
        && let Ok(s) = std::net::UdpSocket::bind("127.0.0.1:0") {
            let _ = s.send_to(b"quit", &cfg.udp_listen);
        }
    for _ in 0..20 {
        std::thread::sleep(Duration::from_millis(250));
        if std::fs::copy(from, to).is_ok() {
            return Ok(());
        }
    }
    bail!("{}", t!("could not copy to {} (is the relay still running?)", "não consegui copiar para {} (o relay ainda está aberto?)", "no se pudo copiar a {} (¿el relay sigue abierto?)", to.display()))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn script_index(v: &Value, lua: &Path) -> Option<usize> {
    let want = slash(lua).to_lowercase();
    v["modules"]["scripts-tool"]
        .as_array()?
        .iter()
        .position(|s| s["path"].as_str().is_some_and(|p| p.replace('\\', "/").to_lowercase() == want))
}

fn read_json(p: &Path) -> Result<Value> {
    let t = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
    serde_json::from_str(t.trim_start_matches('\u{feff}')).with_context(|| format!("reading {}", p.display()))
}

fn write_json(p: &Path, v: &Value) -> Result<()> {
    atomic_write(p, serde_json::to_string_pretty(v)?.as_bytes())
}

fn original_service(p: &Path, listen: &str) -> Result<Value> {
    let service = read_json(p)?;
    anyhow::ensure!(service["type"].as_str().is_some_and(|t| !t.is_empty()) && service["settings"].is_object(),
        "invalid original service: {}", p.display());
    anyhow::ensure!(!service["settings"]["server"].as_str().is_some_and(|s| s.contains(listen)),
        "backup still points to relay: {}", p.display());
    Ok(service)
}

fn atomic_write(p: &Path, contents: &[u8]) -> Result<()> {
    let tmp = p.with_file_name(format!("{}.{}.dd-tmp", p.file_name().context("missing file name")?.to_string_lossy(), uuid()));
    let result = (|| -> Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        if p.exists() { file.set_permissions(std::fs::metadata(p)?.permissions())?; }
        file.write_all(contents)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, p).with_context(|| format!("replacing {}", p.display()))
    })();
    if result.is_err() { let _ = std::fs::remove_file(&tmp); }
    result
}

/// OBS stream settings that go straight to `url` (the Twitch service itself for Twitch).
fn service_for(url: &str, key: &str) -> Value {
    if url.trim_end_matches('/') == crate::config::TWITCH_URL {
        json!({ "type": "rtmp_common", "settings": { "service": "Twitch", "server": "auto", "key": key } })
    } else {
        json!({ "type": "rtmp_custom", "settings": { "server": url, "key": key, "use_auth": false, "bwtest": false } })
    }
}

fn backup_of(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}.dd-backup", p.display()))
}

/// Shared with the Lua bridge. An absent `before` means no explicit user value.
/// INI values are strings; encoder JSON values retain their JSON type.
#[derive(serde::Serialize, serde::Deserialize)]
struct SettingChange {
    before: Option<Value>,
    applied: Value,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Changes {
    version: u32,
    #[serde(default)]
    legacy_backup: bool,
    fields: std::collections::BTreeMap<String, SettingChange>,
}

fn changes_of(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}.dd-changes.json", p.display()))
}

fn read_changes(p: &Path) -> Result<Changes> {
    let changes: Changes = serde_json::from_value(read_json(&changes_of(p))?)?;
    anyhow::ensure!(changes.version == 1, "unsupported change journal for {}", p.display());
    Ok(changes)
}

fn record_change(p: &Path, key: &str, before: Option<Value>, applied: Value) -> Result<()> {
    if before.as_ref() == Some(&applied) {
        return Ok(());
    }
    let mut changes = if changes_of(p).exists() {
        read_changes(p)?
    } else {
        Changes { version: 1, legacy_backup: backup_of(p).exists(), fields: Default::default() }
    };
    let original = match changes.fields.get(key) {
        Some(previous) if before.as_ref() == Some(&previous.applied) => previous.before.clone(),
        _ => before,
    };
    changes.fields.insert(key.into(), SettingChange { before: original, applied });
    write_json(&changes_of(p), &serde_json::to_value(changes)?)
}

fn set_tracked_ini(p: &Path, section: &str, key: &str, value: &str) -> Result<()> {
    let text = if p.exists() { std::fs::read_to_string(p)? } else { String::new() };
    let before = ini_get(text.trim_start_matches('\u{feff}'), section, key);
    record_change(p, &format!("{section}.{key}"), before.map(Value::String), json!(value))?;
    edit_ini(p, |t| ini_set(t, section, key, value))
}

fn restore_tracked_ini(p: &Path, keys: &[(&str, &str)]) -> Result<()> {
    if !changes_of(p).exists() || !p.exists() { return Ok(()); }
    let changes = read_changes(p)?;
    for (section, key) in keys {
        if let Some(change) = changes.fields.get(&format!("{section}.{key}")) {
            anyhow::ensure!(change.applied.is_string() && change.before.as_ref().is_none_or(Value::is_string),
                "invalid INI change journal for {}", p.display());
        }
    }
    edit_ini(p, |text| {
        let mut text = text.to_string();
        for (section, key) in keys {
            if let Some(change) = changes.fields.get(&format!("{section}.{key}"))
                && ini_get(&text, section, key).map(Value::String).as_ref() == Some(&change.applied)
            {
                text = match change.before.as_ref().and_then(Value::as_str) {
                    Some(value) => ini_set(&text, section, key, value),
                    None => ini_remove(&text, section, key),
                };
            }
        }
        text
    })
}

fn restore_profile(prof: &Path, cleanup: &mut Vec<PathBuf>) -> Result<()> {
    let ini = prof.join("basic.ini");
    let orig = backup_of(&ini);
    if changes_of(&ini).exists() {
        let legacy = read_changes(&ini)?.legacy_backup;
        restore_tracked_ini(&ini, &[("Output", "DelayEnable"), ("SimpleOutput", "VBitrate")])?;
        cleanup.push(changes_of(&ini));
        if !legacy { cleanup.push(orig); }
    } else if orig.exists() && ini.exists() {
        // Old installers always disabled DelayEnable, but a bitrate equal to a
        // platform cap is NOT evidence that the script changed it. Keep that backup.
        let before = std::fs::read_to_string(&orig)?;
        edit_ini(&ini, |t| {
            if ini_get(t, "Output", "DelayEnable").as_deref() == Some("false") {
                return match ini_get(before.trim_start_matches('\u{feff}'), "Output", "DelayEnable") {
                    Some(v) => ini_set(t, "Output", "DelayEnable", &v),
                    None => ini_remove(t, "Output", "DelayEnable"),
                };
            }
            t.to_string()
        })?;
    }
    let enc = prof.join("streamEncoder.json");
    let orig = backup_of(&enc);
    if changes_of(&enc).exists() {
        let changes = read_changes(&enc)?;
        if enc.exists() {
            let mut now = read_json(&enc)?;
            for key in ["bitrate", "keyint_sec"] {
                if let Some(change) = changes.fields.get(key)
                    && now.get(key) == Some(&change.applied)
                {
                    match &change.before {
                        Some(value) => now[key] = value.clone(),
                        None => { now.as_object_mut().context("encoder settings are not an object")?.remove(key); },
                    }
                }
            }
            write_json(&enc, &now)?;
        }
        cleanup.push(changes_of(&enc));
        if !changes.legacy_backup { cleanup.push(orig); }
    } else if orig.exists() {
        step(&format!("legacy encoder backup kept for manual recovery: {}", orig.display()));
    }
    Ok(())
}

fn backup_once(p: &Path) -> Result<()> {
    let b = backup_of(p);
    if !b.exists() {
        std::fs::copy(p, b)?;
    }
    Ok(())
}

fn edit_ini(p: &Path, f: impl FnOnce(&str) -> String) -> Result<()> {
    let raw = if p.exists() { std::fs::read_to_string(p)? } else { String::new() };
    let bom = raw.starts_with('\u{feff}');
    let text = raw.trim_start_matches('\u{feff}');
    if p.exists() {
        backup_once(p)?;
    }
    let out = f(text);
    atomic_write(p, if bom { format!("\u{feff}{out}") } else { out }.as_bytes())
}

pub fn ini_get(text: &str, section: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = l == format!("[{section}]");
        } else if in_section
            && let Some((k, v)) = l.split_once('=')
                && k.trim() == key {
                    return Some(v.to_string());
                }
    }
    None
}

/// Sets `key=value` inside `[section]`, adding the key or the section when missing.
/// Removes `key` from `[section]`.
pub fn ini_remove(text: &str, section: &str, key: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let header = format!("[{section}]");
    let mut in_section = false;
    let mut out: Vec<&str> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = l == header;
        } else if in_section && l.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
            continue;
        }
        out.push(line);
    }
    let mut s = out.join(nl);
    s.push_str(nl);
    s
}

pub fn ini_set(text: &str, section: &str, key: &str, value: &str) -> String {
    let nl = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let header = format!("[{section}]");
    let mut out: Vec<String> = Vec::new();
    let (mut in_section, mut found_section, mut done) = (false, false, false);
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            if in_section && !done {
                // insert before the blank lines that end the section
                let at = out.iter().rposition(|x| !x.trim().is_empty()).map_or(out.len(), |i| i + 1);
                out.insert(at, format!("{key}={value}"));
                done = true;
            }
            in_section = l == header;
            found_section |= in_section;
        } else if in_section && !done
            && let Some((k, _)) = l.split_once('=')
                && k.trim() == key {
                    out.push(format!("{key}={value}"));
                    done = true;
                    continue;
                }
        out.push(line.to_string());
    }
    if in_section && !done {
        out.push(format!("{key}={value}"));
        done = true;
    }
    if !found_section {
        if out.last().is_some_and(|l| !l.trim().is_empty()) {
            out.push(String::new());
        }
        out.push(header);
        out.push(format!("{key}={value}"));
        done = true;
    }
    debug_assert!(done);
    let mut s = out.join(nl);
    s.push_str(nl);
    s
}

fn uuid() -> String {
    let s = std::collections::hash_map::RandomState::new();
    let (a, b) = (s.hash_one(std::process::id()), s.hash_one(std::time::SystemTime::now()));
    format!(
        "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        (b >> 48) & 0xfff,
        b & 0xffff_ffff_ffff
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Install, let the OBS script cap the encoder, switch profile, uninstall: every
    /// profile and setting must be back as it was, with no backup files left.
    #[test]
    fn uninstall_puts_everything_back() {
        let root = std::env::temp_dir().join(format!("dd-uninstall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let obs = root.join("obs-studio");
        let prof = obs.join("basic/profiles/Main");
        let other = obs.join("basic/profiles/Other");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::create_dir_all(obs.join("basic/scenes")).unwrap();
        let service = r#"{"type":"rtmp_common","settings":{"service":"Twitch","server":"auto","key":"live_123"}}"#;
        std::fs::write(prof.join("service.json"), service).unwrap();
        std::fs::write(other.join("service.json"), service).unwrap();
        let basic = "[Output]\r\nMode=Simple\r\nDelayEnable=true\r\n\r\n[SimpleOutput]\r\nVBitrate=9000\r\n";
        std::fs::write(prof.join("basic.ini"), basic).unwrap();
        std::fs::write(prof.join("streamEncoder.json"), r#"{"bitrate":9000,"rate_control":"CBR"}"#).unwrap();
        std::fs::write(obs.join("basic/scenes/Main.json"), r#"{"name":"Main","modules":{}}"#).unwrap();
        std::fs::write(obs.join("user.ini"), "[Basic]\r\nProfileDir=Main\r\n").unwrap();
        let p = Paths { obs_dir: obs.clone(), install_dir: root.join("obs-dynamic-delay") };

        install(&p, false).unwrap();
        let svc = read_json(&prof.join("service.json")).unwrap();
        assert_eq!(svc["type"], "rtmp_custom");
        // OBS shows and sends the Twitch VOD track on the custom server
        let user = std::fs::read_to_string(obs.join("user.ini")).unwrap();
        assert_eq!(ini_get(&user, "General", "EnableCustomServerVodTrack").as_deref(), Some("true"));
        // What the OBS script journals on the first Twitch stream.
        let encoder = prof.join("streamEncoder.json");
        record_change(&encoder, "bitrate", Some(json!(9000)), json!(6000)).unwrap();
        record_change(&encoder, "keyint_sec", None, json!(2)).unwrap();
        std::fs::write(prof.join("streamEncoder.json"), r#"{"bitrate":6000,"rate_control":"CBR","keyint_sec":2}"#).unwrap();
        set_tracked_ini(&prof.join("basic.ini"), "SimpleOutput", "VBitrate", "6000").unwrap();
        // the streamer switches to another profile before uninstalling
        std::fs::write(obs.join("user.ini"), std::fs::read_to_string(obs.join("user.ini")).unwrap().replace("ProfileDir=Main", "ProfileDir=Other")).unwrap();
        // simulate the old install layout too: the app-folder copy alone must not touch "Other"
        uninstall(&p).unwrap();

        assert_eq!(read_json(&prof.join("service.json")).unwrap(), serde_json::from_str::<Value>(service).unwrap());
        assert_eq!(read_json(&other.join("service.json")).unwrap(), serde_json::from_str::<Value>(service).unwrap());
        let ini = std::fs::read_to_string(prof.join("basic.ini")).unwrap();
        assert_eq!(ini_get(&ini, "Output", "DelayEnable").as_deref(), Some("true"));
        assert_eq!(ini_get(&ini, "SimpleOutput", "VBitrate").as_deref(), Some("9000"));
        let enc = read_json(&prof.join("streamEncoder.json")).unwrap();
        assert_eq!(enc["bitrate"], 9000);
        assert!(enc.get("keyint_sec").is_none());
        let scene = read_json(&obs.join("basic/scenes/Main.json")).unwrap();
        assert_eq!(scene["modules"]["scripts-tool"].as_array().map(|a| a.len()), Some(0));
        let user = std::fs::read_to_string(obs.join("user.ini")).unwrap();
        assert!(!user.contains(dock_title()) || !user.contains("dock.html"));
        // it was not set before the install: gone again
        assert_eq!(ini_get(&user, "General", "EnableCustomServerVodTrack"), None);
        let leftovers: Vec<_> = walk(&obs).into_iter().filter(|f| {
            let name = f.to_string_lossy();
            name.ends_with(".dd-backup") || name.ends_with(".dd-changes.json")
        }).collect();
        assert!(leftovers.is_empty(), "backup files left: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restore_only_fields_we_changed_and_preserve_later_edits() {
        let root = std::env::temp_dir().join(format!("dd-fields-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let enc = root.join("streamEncoder.json");
        // The script changes only keyframes. 8000 is a manual bitrate, even though
        // it happens to equal Kick's cap (the old heuristic restored it incorrectly).
        write_json(&enc, &json!({"bitrate": 6000, "keyint_sec": 0})).unwrap();
        record_change(&enc, "keyint_sec", Some(json!(0)), json!(2)).unwrap();
        write_json(&enc, &json!({"bitrate": 8000, "keyint_sec": 2})).unwrap();
        restore_profile(&root, &mut vec![]).unwrap();
        assert_eq!(read_json(&enc).unwrap(), json!({"bitrate":8000,"keyint_sec":0}));

        record_change(&enc, "bitrate", Some(json!(8000)), json!(6000)).unwrap();
        write_json(&enc, &json!({"bitrate":7500,"keyint_sec":4})).unwrap();
        restore_profile(&root, &mut vec![]).unwrap();
        assert_eq!(read_json(&enc).unwrap(), json!({"bitrate":7500,"keyint_sec":4}));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repeated_caps_keep_the_original_and_rebase_after_manual_edit() {
        let root = std::env::temp_dir().join(format!("dd-recap-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let enc = root.join("streamEncoder.json");
        record_change(&enc, "bitrate", Some(json!(10000)), json!(8000)).unwrap();
        record_change(&enc, "bitrate", Some(json!(8000)), json!(6000)).unwrap();
        assert_eq!(read_changes(&enc).unwrap().fields["bitrate"].before, Some(json!(10000)));
        record_change(&enc, "bitrate", Some(json!(9500)), json!(6000)).unwrap();
        assert_eq!(read_changes(&enc).unwrap().fields["bitrate"].before, Some(json!(9500)));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_journal_prevents_changes_and_legacy_caps_are_not_guessed() {
        let root = std::env::temp_dir().join(format!("dd-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let ini = root.join("basic.ini");
        std::fs::write(&ini, "[Output]\nDelayEnable=true\n").unwrap();
        std::fs::write(changes_of(&ini), "broken").unwrap();
        assert!(set_tracked_ini(&ini, "Output", "DelayEnable", "false").is_err());
        assert!(std::fs::read_to_string(&ini).unwrap().contains("DelayEnable=true"));
        std::fs::remove_file(changes_of(&ini)).unwrap();
        let enc = root.join("streamEncoder.json");
        write_json(&backup_of(&enc), &json!({"bitrate":6000,"keyint_sec":0})).unwrap();
        write_json(&enc, &json!({"bitrate":8000,"keyint_sec":2})).unwrap();
        restore_profile(&root, &mut vec![]).unwrap();
        assert_eq!(read_json(&enc).unwrap()["bitrate"], 8000);
        assert!(backup_of(&enc).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn absent_ini_keys_and_manual_vod_switch_survive_repeated_uninstall() {
        let root = std::env::temp_dir().join(format!("dd-absent-{}", std::process::id()));
        let prof = root.join("basic/profiles/Main");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(prof.join("basic.ini"), "[Output]\nMode=Simple\n").unwrap();
        let user = root.join("user.ini");
        std::fs::write(&user, "[Basic]\nProfileDir=Main\n[General]\nEnableCustomServerVodTrack=true\n").unwrap();
        let p = Paths { obs_dir: root.clone(), install_dir: root.join("relay") };
        install(&p, false).unwrap();
        uninstall(&p).unwrap();
        uninstall(&p).unwrap();
        assert_eq!(ini_get(&std::fs::read_to_string(prof.join("basic.ini")).unwrap(), "Output", "DelayEnable"), None);
        assert_eq!(ini_get(&std::fs::read_to_string(&user).unwrap(), "General", "EnableCustomServerVodTrack").as_deref(), Some("true"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_uninstall_keeps_all_recovery_files_and_can_be_retried() {
        let root = std::env::temp_dir().join(format!("dd-retry-{}", std::process::id()));
        let p = Paths { obs_dir: root.join("obs"), install_dir: root.join("relay") };
        for name in ["A", "B"] {
            let prof = p.obs_dir.join("basic/profiles").join(name);
            std::fs::create_dir_all(&prof).unwrap();
            std::fs::write(p.user_ini(), format!("[Basic]\nProfileDir={name}\n")).unwrap();
            write_json(&prof.join("service.json"), &service_for(TWITCH_URL, name)).unwrap();
            std::fs::write(prof.join("basic.ini"), "[Output]\nDelayEnable=true\n").unwrap();
            install(&p, false).unwrap();
        }
        let bad = p.obs_dir.join("basic/profiles/B/service.json.dd-backup");
        std::fs::write(&bad, "broken").unwrap();
        assert!(uninstall(&p).is_err());
        for name in ["A", "B"] {
            let prof = p.obs_dir.join("basic/profiles").join(name);
            assert!(prof.join("service.json.dd-backup").exists());
            assert!(prof.join("basic.ini.dd-changes.json").exists());
        }
        assert!(p.lua().exists());
        write_json(&bad, &service_for(TWITCH_URL, "B")).unwrap();
        uninstall(&p).unwrap();
        for name in ["A", "B"] {
            let prof = p.obs_dir.join("basic/profiles").join(name);
            assert_eq!(read_json(&prof.join("service.json")).unwrap()["settings"]["key"], name);
            assert_eq!(ini_get(&std::fs::read_to_string(prof.join("basic.ini")).unwrap(), "Output", "DelayEnable").as_deref(), Some("true"));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unreadable_unrelated_profile_does_not_block_uninstall() {
        let root = std::env::temp_dir().join(format!("dd-unrelated-{}", uuid()));
        let p = Paths { obs_dir: root.join("obs"), install_dir: root.join("relay") };
        let prof = p.obs_dir.join("basic/profiles/Unrelated");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(prof.join("service.json"), "broken JSON").unwrap();
        std::fs::write(prof.join("service.json.dd-backup"), "keep me").unwrap();
        uninstall(&p).unwrap();
        assert_eq!(std::fs::read_to_string(prof.join("service.json.dd-backup")).unwrap(), "keep me");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn force_preserves_recovery_after_missing_config_and_corrupt_relay() {
        let root = std::env::temp_dir().join(format!("dd-force-{}", uuid()));
        let p = Paths { obs_dir: root.join("obs"), install_dir: root.join("relay") };
        let prof = p.obs_dir.join("basic/profiles/Main");
        std::fs::create_dir_all(&prof).unwrap();
        let listen = Config::default().listen;
        write_json(&prof.join("service.json"), &service_for(&format!("rtmp://{listen}/live"), "test")).unwrap();
        assert!(uninstall(&p).is_err());
        uninstall_with_force(&p, true).unwrap();
        std::fs::write(prof.join("service.json.dd-backup"), "corrupt backup").unwrap();
        assert!(uninstall(&p).is_err());
        uninstall_with_force(&p, true).unwrap();
        assert_eq!(std::fs::read_to_string(prof.join("service.json.dd-backup")).unwrap(), "corrupt backup");
        std::fs::write(prof.join("service.json"), format!("broken rtmp://{listen}/live")).unwrap();
        assert!(uninstall(&p).is_err());
        uninstall_with_force(&p, true).unwrap();
        assert!(prof.join("service.json.dd-backup").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn forced_success_cleans_backups_and_reinstall_restores_current_service() {
        let root = std::env::temp_dir().join(format!("dd-force-success-{}", uuid()));
        let p = Paths { obs_dir: root.join("obs"), install_dir: root.join("relay") };
        let prof = p.obs_dir.join("basic/profiles/Main");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::write(p.user_ini(), "[Basic]\nProfileDir=Main\n").unwrap();
        write_json(&prof.join("service.json"), &service_for(TWITCH_URL, "original")).unwrap();
        std::fs::write(prof.join("basic.ini"), "[Output]\nDelayEnable=true\n").unwrap();
        install(&p, false).unwrap();
        uninstall_with_force(&p, true).unwrap();
        assert!(!prof.join("service.json.dd-backup").exists());
        assert!(!prof.join("basic.ini.dd-backup").exists());
        assert!(!prof.join("basic.ini.dd-changes.json").exists());
        assert_eq!(read_json(&prof.join("service.json")).unwrap()["settings"]["key"], "original");
        // A new install must capture YouTube, not resurrect Twitch's old backup.
        let youtube = service_for(YOUTUBE_URL, "youtube-original");
        write_json(&prof.join("service.json"), &youtube).unwrap();
        std::fs::write(prof.join("basic.ini"), "[Output]\nDelayEnable=false\n").unwrap();
        install(&p, false).unwrap();
        uninstall(&p).unwrap();
        assert_eq!(read_json(&prof.join("service.json")).unwrap(), youtube);
        assert_eq!(ini_get(&std::fs::read_to_string(prof.join("basic.ini")).unwrap(),
            "Output", "DelayEnable").as_deref(), Some("false"));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = vec![];
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() { out.extend(walk(&p)) } else { out.push(p) }
        }
        out
    }

    /// No copy of the original stream settings anywhere: OBS goes straight to the destination.
    #[test]
    fn uninstall_without_any_backup_streams_direct() {
        let root = std::env::temp_dir().join(format!("dd-nobackup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let obs = root.join("obs-studio");
        let prof = obs.join("basic/profiles/Main");
        std::fs::create_dir_all(&prof).unwrap();
        std::fs::create_dir_all(obs.join("basic/scenes")).unwrap();
        std::fs::write(obs.join("user.ini"), "[Basic]\r\nProfileDir=Main\r\n").unwrap();
        let relay = r#"{"type":"rtmp_custom","settings":{"server":"rtmp://127.0.0.1:1935/live","key":"delay"}}"#;
        std::fs::write(prof.join("service.json"), relay).unwrap();
        let p = Paths { obs_dir: obs.clone(), install_dir: root.join("obs-dynamic-delay") };
        std::fs::create_dir_all(&p.install_dir).unwrap();
        let cfg = Config { stream_key: "live_abc".into(), ..Config::default() };
        cfg.save(&p.config()).unwrap();
        uninstall(&p).unwrap();
        let svc = read_json(&prof.join("service.json")).unwrap();
        assert_eq!(svc["type"], "rtmp_common");
        assert_eq!(svc["settings"]["service"], "Twitch");
        assert_eq!(svc["settings"]["key"], "live_abc");
        assert_eq!(service_for("rtmps://fa723fc1b171.global-contribute.live-video.net/app", "sk")["settings"]["server"],
            "rtmps://fa723fc1b171.global-contribute.live-video.net/app");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ini_removes_only_that_key() {
        let t = "[General]\r\nA=1\r\nB=2\r\n\r\n[Other]\r\nB=3\r\n";
        let o = ini_remove(t, "General", "B");
        assert_eq!(ini_get(&o, "General", "B"), None);
        assert_eq!(ini_get(&o, "General", "A").as_deref(), Some("1"));
        assert_eq!(ini_get(&o, "Other", "B").as_deref(), Some("3"));
        assert!(o.contains("\r\n"));
    }

    #[test]
    fn ini_replaces_and_inserts() {
        let t = "[General]\r\nA=1\r\n\r\n[Output]\r\nMode=Simple\r\nDelayEnable=true\r\n\r\n[Video]\r\nX=1\r\n";
        let o = ini_set(t, "Output", "DelayEnable", "false");
        assert!(o.contains("[Output]\r\nMode=Simple\r\nDelayEnable=false\r\n"));
        assert_eq!(ini_get(&o, "Output", "DelayEnable").as_deref(), Some("false"));

        let o = ini_set(t, "Output", "New", "7");
        assert!(o.contains("DelayEnable=true\r\nNew=7\r\n\r\n[Video]"), "{o}");

        let o = ini_set("[A]\nx=1\n", "B", "y", "2");
        assert_eq!(o, "[A]\nx=1\n\n[B]\ny=2\n");

        let o = ini_set("[A]\nx=1\n", "A", "y", "2");
        assert_eq!(o, "[A]\nx=1\ny=2\n");
    }

    #[test]
    fn uuid_shape() {
        let u = uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(u.matches('-').count(), 4);
    }
}

/// Windowed installer: native message boxes, no console.
#[cfg(windows)]
mod gui {
    use std::time::Duration;

    use windows_sys::Win32::System::Console::FreeConsole;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IDCANCEL, IDNO, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_ICONWARNING, MB_OKCANCEL,
        MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MB_YESNO, MB_YESNOCANCEL, MessageBoxW,
    };

    use super::{Mode, Paths, STEPS, dock_title, install, launch_obs, obs_running, uninstall};
    use crate::t;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn msg(text: &str, flags: u32) -> i32 {
        let title = t!("Dynamic Delay for OBS", "Delay dinâmico para OBS", "Delay dinámico para OBS");
        unsafe {
            MessageBoxW(std::ptr::null_mut(), wide(text).as_ptr(), wide(&title).as_ptr(), flags | MB_SETFOREGROUND | MB_TOPMOST)
        }
    }

    pub fn run() {
        // double-clicked: drop the empty console window
        unsafe {
            FreeConsole();
        }
        let credit = format!("\n\n{}", t!("Developed by ragnarcb - github.com/ragnarcb", "Desenvolvido por ragnarcb - github.com/ragnarcb", "Desarrollado por ragnarcb - github.com/ragnarcb"));
        let p = match Paths::detect() {
            Ok(p) => p,
            Err(e) => {
                msg(&format!("{e:#}"), MB_OK | MB_ICONERROR);
                return;
            }
        };
        let dock = dock_title();
        let mode = if p.is_installed() {
            let text = t!(
                "Dynamic Delay is already installed.\n\nYes = update / reinstall (keeps your settings)\nNo = uninstall\nCancel = close",
                "O Delay dinâmico já está instalado.\n\nSim = atualizar / reinstalar (mantém sua configuração)\nNão = desinstalar\nCancelar = fechar",
                "El Delay dinámico ya está instalado.\n\nSí = actualizar / reinstalar (conserva tu configuración)\nNo = desinstalar\nCancelar = cerrar"
            );
            match msg(&(text + &credit), MB_YESNOCANCEL | MB_ICONQUESTION) {
                IDYES => Mode::Install,
                IDNO => Mode::Uninstall,
                _ => return,
            }
        } else {
            let text = t!(
                "Install Dynamic Delay into OBS?\n\n- adds the \"{dock}\" panel and the script to OBS\n- makes OBS stream through the relay (your current settings are backed up)\n- turns off OBS' built-in Stream Delay",
                "Instalar o Delay dinâmico no OBS?\n\n- adiciona o painel \"{dock}\" e o script ao OBS\n- faz o OBS transmitir pelo relay (a configuração atual fica salva)\n- desliga o Stream Delay nativo do OBS",
                "¿Instalar el Delay dinámico en OBS?\n\n- agrega el panel \"{dock}\" y el script a OBS\n- hace que OBS transmita a través del relay (tu configuración actual queda respaldada)\n- desactiva el Retraso de transmisión nativo de OBS"
            );
            if msg(&(text + &credit), MB_YESNO | MB_ICONQUESTION) != IDYES {
                return;
            }
            Mode::Install
        };
        while obs_running() {
            let text = t!(
                "Close OBS to continue (it rewrites its settings when it closes), then click OK.",
                "Feche o OBS para continuar (ele regrava as configurações ao fechar) e clique em OK.",
                "Cierra OBS para continuar (reescribe su configuración al cerrarse) y luego haz clic en Aceptar."
            );
            if msg(&text, MB_OKCANCEL | MB_ICONWARNING) == IDCANCEL {
                return;
            }
            std::thread::sleep(Duration::from_millis(800));
        }
        let result = match mode {
            Mode::Uninstall => uninstall(&p).map(|_| None),
            _ => install(&p, false).map(Some),
        };
        let steps = STEPS.lock().unwrap().iter().map(|s| format!("- {s}")).collect::<Vec<_>>().join("\n");
        match result {
            Ok(Some(cfg)) => {
                let mut text = format!("{}\n\n{steps}", t!("Done!", "Pronto!", "¡Listo!"));
                if cfg.stream_key.is_empty() {
                    text += &format!("\n\n{}", t!(
                        "Only the stream key is missing: fill it in the \"{dock}\" panel inside OBS (Docks menu).",
                        "Falta só a chave de transmissão: preencha no painel \"{dock}\" dentro do OBS (menu Docks).",
                        "Solo falta la clave de transmisión: complétala en el panel \"{dock}\" dentro de OBS (menú Docks)."
                    ));
                }
                text += &format!("\n\n{}", t!("Open OBS now?", "Abrir o OBS agora?", "¿Abrir OBS ahora?"));
                if msg(&text, MB_YESNO | MB_ICONINFORMATION) == IDYES {
                    launch_obs();
                }
            }
            Ok(None) => {
                msg(&format!("{}\n\n{steps}", t!("Uninstalled.", "Desinstalado.", "Desinstalado.")), MB_OK | MB_ICONINFORMATION);
            }
            Err(e) => {
                msg(&format!("{}\n\n{e:#}", t!("Something went wrong:", "Algo deu errado:", "Algo salió mal:")), MB_OK | MB_ICONERROR);
            }
        }
    }
}
