//! Console installer shown when the exe is double-clicked: copies the relay and
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
    [Lang::En, Lang::Pt].iter().any(|l| d["title"] == i18n::dock_title(*l))
}
const EXE_NAME: &str = if cfg!(windows) { "obs-dynamic-delay.exe" } else { "obs-dynamic-delay" };

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Ask,
    Install,
    Uninstall,
}

pub fn wizard(mode: Mode) -> Result<()> {
    println!("==============================================");
    println!("{}", t!("  Dynamic Delay for OBS", "  Delay dinamico para OBS"));
    println!("{}", t!("  Developed by ragnarcb", "  Desenvolvido por ragnarcb"));
    println!("  {}", crate::control::AUTHOR_URL);
    println!("==============================================\n");
    let r = run(mode);
    if let Err(e) = &r {
        println!("\n{} {e:#}", t!("ERROR:", "ERRO:"));
    }
    if mode == Mode::Ask {
        ask(&t!("\nPress Enter to close.", "\nPressione Enter para fechar."));
    }
    r
}

fn run(mode: Mode) -> Result<()> {
    let p = Paths::detect()?;
    let installed = p.is_installed();
    let mode = match mode {
        Mode::Ask if installed => {
            println!("{}", t!("Already installed in {}.", "Ja esta instalado em {}.", p.install_dir.display()));
            match ask(&t!(
                "Type 1 to update/reinstall, 2 to uninstall, or Enter to quit: ",
                "Digite 1 para atualizar/reinstalar, 2 para desinstalar, ou Enter para sair: "
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
            println!("{}", t!("This installs the dynamic delay into your OBS:", "Isto vai instalar o delay dinamico no seu OBS:"));
            println!("{}", t!(
                "  - adds the script and a \"{dock}\" panel to the OBS window",
                "  - adiciona o script e um painel \"{dock}\" na tela do OBS"
            ));
            println!("{}", t!(
                "  - makes OBS stream through the relay (your current settings are backed up)",
                "  - faz o OBS transmitir pelo relay (a configuracao atual fica salva)"
            ));
            println!("{}", t!("  - turns off OBS' built-in Stream Delay\n", "  - desliga o Stream Delay nativo do OBS\n"));
            if ask(&t!("Install now? [Y/n] ", "Instalar agora? [S/n] ")).to_lowercase().starts_with('n') {
                return Ok(());
            }
            Mode::Install
        }
        m => m,
    };
    wait_obs_closed();
    match mode {
        Mode::Uninstall => uninstall(&p),
        _ => install(&p),
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
                    "nao achei a configuracao do OBS em {} (abra o OBS pelo menos uma vez)",
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
            .context(t!("no OBS profile found", "nenhum perfil do OBS encontrado"))
    }
    fn scene_collections(&self) -> Vec<PathBuf> {
        let dir = self.obs_dir.join("basic").join("scenes");
        let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
        rd.filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect()
    }
    fn is_installed(&self) -> bool {
        self.lua().exists()
            && self.scene_collections().iter().any(|c| {
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

fn install(p: &Paths) -> Result<()> {
    std::fs::create_dir_all(&p.install_dir)?;

    // 1. files
    let me = std::env::current_exe()?;
    let target_exe = p.install_dir.join(EXE_NAME);
    if !same_file(&me, &target_exe) {
        copy_with_retry(&me, &target_exe)?;
    }
    std::fs::write(p.lua(), LUA)?;
    let mut cfg = Config::load_or_create(&p.config())?;
    // the installer's language (English or Portuguese build) becomes the app language
    cfg.language = i18n::get().code().to_string();
    step(&t!("files copied to {}", "arquivos copiados para {}", p.install_dir.display()));

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
            step(&t!("destination imported from OBS: {}", "destino importado do OBS: {}", cfg.upstream_url));
        }
    }
    if !imported && cfg.stream_key.is_empty() {
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
    step(&t!("OBS now streams to {relay_server}", "OBS agora transmite para {relay_server}"));

    // 3. disable OBS' own stream delay
    let basic_ini = profile.join("basic.ini");
    if basic_ini.exists() {
        edit_ini(&basic_ini, |t| ini_set(t, "Output", "DelayEnable", "false"))?;
        step(&t!("OBS' built-in Stream Delay turned off", "Stream Delay nativo do OBS desligado"));
    }

    // 4. script in every scene collection
    let lua = p.lua();
    for c in p.scene_collections() {
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
            step(&t!("old copy of the script removed from OBS", "copia antiga do script removida do OBS"));
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
        "script adicionado ao OBS (atalhos em Configuracoes > Atalhos > Delay dinamico)"
    ));

    // 5. dock with the control panel
    // the relay rewrites this file at every start; write it now so the dock works right away
    std::fs::write(p.dock_html(), crate::control::dock_html(&cfg))?;
    let dock_url = format!("file:///{}", slash(&p.dock_html()).trim_start_matches('/'));
    edit_ini(&p.user_ini(), |t| {
        let mut docks: Vec<Value> = ini_get(t, "BasicWindow", "ExtraBrowserDocks")
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        docks.retain(|d| !is_our_dock(d));
        docks.push(json!({ "title": dock_title(), "url": dock_url, "uuid": uuid() }));
        ini_set(t, "BasicWindow", "ExtraBrowserDocks", &Value::Array(docks).to_string())
    })?;
    let dock = dock_title();
    step(&t!("\"{dock}\" panel added (Docks menu)", "painel \"{dock}\" adicionado (menu Docks)"));

    println!("\n{}", t!("Done!", "Pronto!"));
    if cfg.stream_key.is_empty() {
        println!("{}", t!(
            "Only the stream key is missing: fill it in the \"{dock}\" panel inside OBS.",
            "Falta so a chave de transmissao: preencha no painel \"{dock}\" dentro do OBS."
        ));
    }
    if ask(&t!("Open OBS now? [Y/n] ", "Abrir o OBS agora? [S/n] ")).to_lowercase().starts_with('n') {
        return Ok(());
    }
    launch_obs();
    Ok(())
}

fn uninstall(p: &Paths) -> Result<()> {
    let lua = p.lua();
    for c in p.scene_collections() {
        let mut v = read_json(&c)?;
        if let Some(i) = script_index(&v, &lua) {
            v["modules"]["scripts-tool"].as_array_mut().unwrap().remove(i);
            write_json(&c, &v)?;
        }
    }
    step(&t!("script removed from OBS", "script removido do OBS"));
    if p.user_ini().exists() {
        edit_ini(&p.user_ini(), |t| {
            let mut docks: Vec<Value> = ini_get(t, "BasicWindow", "ExtraBrowserDocks")
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            docks.retain(|d| !is_our_dock(d));
            ini_set(t, "BasicWindow", "ExtraBrowserDocks", &Value::Array(docks).to_string())
        })?;
        step(&t!("panel removed", "painel removido"));
    }
    if p.service_backup().exists() {
        std::fs::copy(p.service_backup(), p.profile_dir()?.join("service.json"))?;
        step(&t!("original stream settings restored", "configuracao de transmissao original restaurada"));
    }
    println!(
        "\n{}",
        t!(
            "Uninstalled. The folder {} can be deleted.",
            "Desinstalado. A pasta {} pode ser apagada.",
            p.install_dir.display()
        )
    );
    Ok(())
}

fn ask_destination(cfg: &mut Config) {
    println!("\n{}", t!("Where do you stream to?", "Para onde voce transmite?"));
    println!("{}", t!("  1 = Twitch   2 = YouTube   3 = Kick / other (paste URL)", "  1 = Twitch   2 = YouTube   3 = Kick / outra (colar URL)"));
    match ask(&t!("Option (Enter = Twitch): ", "Opcao (Enter = Twitch): ")).as_str() {
        "2" => cfg.upstream_url = YOUTUBE_URL.into(),
        "3" => {
            let url = ask(&t!("Server URL (rtmp:// or rtmps://): ", "URL do servidor (rtmp:// ou rtmps://): "));
            if url.starts_with("rtmp://") || url.starts_with("rtmps://") {
                cfg.upstream_url = url;
            } else {
                println!("{}", t!("Invalid URL, using Twitch. Change it later in the panel.", "URL invalida, usando Twitch. Troque depois no painel."));
                cfg.upstream_url = TWITCH_URL.into();
            }
        }
        _ => cfg.upstream_url = TWITCH_URL.into(),
    }
    cfg.stream_key = ask(&t!("Stream key (Enter = fill in later in the panel): ", "Chave de transmissao (Enter = preencher depois no painel): "));
}

fn step(msg: &str) {
    println!("  [ok] {msg}");
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
        "Feche o OBS para continuar (ele regrava as configuracoes ao fechar)."
    ));
    print!("{}", t!("Waiting for OBS to close", "Aguardando o OBS fechar"));
    while obs_running() {
        print!(".");
        let _ = std::io::stdout().flush();
        std::thread::sleep(Duration::from_secs(1));
    }
    println!(" ok\n");
    std::thread::sleep(Duration::from_millis(500));
}

fn launch_obs() {
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
    println!("{}", t!("OBS not found, please open it yourself.", "Nao achei o OBS para abrir, abra ele normalmente."));
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
    bail!("{}", t!("could not copy to {} (is the relay still running?)", "nao consegui copiar para {} (o relay ainda esta aberto?)", to.display()))
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
    std::fs::write(p, serde_json::to_string_pretty(v)?).with_context(|| format!("writing {}", p.display()))
}

fn backup_once(p: &Path) -> Result<()> {
    let b = PathBuf::from(format!("{}.dd-backup", p.display()));
    if !b.exists() {
        std::fs::copy(p, b)?;
    }
    Ok(())
}

fn edit_ini(p: &Path, f: impl FnOnce(&str) -> String) -> Result<()> {
    let raw = std::fs::read_to_string(p).unwrap_or_default();
    let bom = raw.starts_with('\u{feff}');
    let text = raw.trim_start_matches('\u{feff}');
    if p.exists() {
        backup_once(p)?;
    }
    let out = f(text);
    std::fs::write(p, if bom { format!("\u{feff}{out}") } else { out })
        .with_context(|| format!("writing {}", p.display()))
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
