//! HTTP control panel/API (token protected, optionally on the LAN) and the UDP
//! command port used by the OBS script.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;
use tower_http::cors::CorsLayer;

use crate::EngineMsg;
use crate::config::{Config, destination_from_obs};
use crate::i18n::{self, Lang};
use crate::status::{AudioSource, Cmd, ObsAction, ObsInfo, ObsVideo, Shared, Status};
use crate::t;

pub const PANEL: &str = include_str!("panel.html");
const DECK: &str = include_str!("deck.html");
const OVERLAY: &str = include_str!("overlay.html");
pub const AUTHOR_URL: &str = "https://github.com/ragnarcb";
pub const RELEASES_URL: &str = "https://github.com/ragnarcb/obs-dynamic-delay/releases/latest";
pub const DISCORD_URL: &str = "https://discord.gg/crctbnQ2f8";

/// The panel with the API address and token baked in, for the OBS dock (a local file).
pub fn dock_html(cfg: &Config) -> String {
    let inject = format!(
        "<script>window.DD_API = {}; window.DD_TOKEN = {};</script>\n</head>",
        json!(format!("http://127.0.0.1:{}", cfg.http_port())),
        json!(cfg.api_token)
    );
    PANEL.replacen("</head>", &inject, 1)
}

#[derive(Clone)]
struct AppState {
    tx: UnboundedSender<EngineMsg>,
    shared: Arc<Shared>,
}

/// Serves the panel and API; rebinds when "LAN access" is switched.
pub async fn serve_http(tx: UnboundedSender<EngineMsg>, shared: Arc<Shared>) -> Result<()> {
    let state = AppState { tx, shared: shared.clone() };
    let api = Router::new()
        .route("/api/status", get(status_handler))
        .route("/api/cmd/{cmd}", get(cmd_handler).post(cmd_handler))
        .route("/api/cmd/{cmd}/{arg}", get(cmd_arg_handler).post(cmd_arg_handler))
        // short aliases kept for Stream Deck buttons made with older versions
        .route("/api/on", get(|s: State<AppState>| run(s, Cmd::On)).post(|s: State<AppState>| run(s, Cmd::On)))
        .route("/api/off", get(|s: State<AppState>| run(s, Cmd::Off)).post(|s: State<AppState>| run(s, Cmd::Off)))
        .route("/api/toggle", get(|s: State<AppState>| run(s, Cmd::Toggle)).post(|s: State<AppState>| run(s, Cmd::Toggle)))
        .route("/api/delay/{secs}", get(set_delay).post(set_delay))
        .route("/api/add/{secs}", get(add_delay).post(add_delay))
        .route("/api/config", get(get_config).post(set_config))
        .route("/api/obs/configure", post(obs_configure))
        .route("/api/obs/restore", post(obs_restore))
        .route("/api/obs/fps/{fps}", post(obs_fps))
        .route("/api/obs/overlay", post(obs_overlay))
        .route("/api/obs/stream/{action}", post(obs_stream))
        .route("/api/update/check", post(update_check))
        .route("/api/output/{id}/{action}", post(output_action))
        .route("/api/open/{target}", post(open_target))
        .route("/api/lan", get(lan_info))
        .route("/api/deck/press/{index}", post(deck_press))
        .route("/api/events", get(events_handler))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));
    let app = Router::new()
        .route("/", get(|| async { Html(PANEL) }))
        .route("/deck", get(|| async { Html(DECK) }))
        // read-only and without the token: it goes into an OBS Browser Source
        .route("/overlay", get(overlay_page))
        .route("/overlay/state", get(overlay_state))
        .merge(api)
        // the dock is a local file (origin "null"); every API call still needs the token
        .layer(CorsLayer::permissive())
        .with_state(state);

    loop {
        let (lan, port) = {
            let c = shared.config.lock().unwrap();
            (c.features.phone, c.http_port())
        };
        let ip = if lan { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST };
        let listener = tokio::net::TcpListener::bind(SocketAddr::from((ip, port))).await?;
        log::info!("control panel on {ip}:{port}");
        let watch = shared.clone();
        axum::serve(listener, app.clone())
            .with_graceful_shutdown(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if watch.config.lock().unwrap().features.phone != lan {
                        break;
                    }
                }
            })
            .await?;
    }
}

async fn require_token(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let token = s.shared.config.lock().unwrap().api_token.clone();
    let header = req.headers().get("x-dd-token").and_then(|v| v.to_str().ok()).map(str::to_string);
    let query = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")).map(str::to_string));
    let given = header.or(query).unwrap_or_default();
    if !token.is_empty() && same_secret(given.as_bytes(), token.as_bytes()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, Json(json!({ "ok": false, "error": "invalid token" }))).into_response()
    }
}

/// Compares two secrets in constant time (for equal lengths), so the answer time
/// does not tell how much of a guessed token was right.
fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn run(State(s): State<AppState>, cmd: Cmd) -> impl std::future::Future<Output = Json<Value>> {
    let _ = s.tx.send(EngineMsg::Cmd(cmd));
    async { Json(json!({ "ok": true })) }
}

#[derive(serde::Serialize)]
struct FullStatus {
    #[serde(flatten)]
    status: Status,
    obs: ObsInfo,
}

async fn status_handler(State(s): State<AppState>) -> impl IntoResponse {
    Json(FullStatus { status: s.shared.status.lock().unwrap().clone(), obs: s.shared.bridge.lock().unwrap().info() })
}

async fn cmd_handler(s: State<AppState>, Path(cmd): Path<String>) -> Json<Value> {
    match Cmd::parse(&cmd) {
        Some(c) if !matches!(c, Cmd::Quit | Cmd::Stay) => run(s, c).await,
        _ => Json(json!({ "ok": false, "error": "unknown command" })),
    }
}

async fn cmd_arg_handler(s: State<AppState>, Path((cmd, arg)): Path<(String, String)>) -> Json<Value> {
    match Cmd::parse(&format!("{cmd} {arg}")) {
        Some(c) if !matches!(c, Cmd::Quit | Cmd::Stay) => run(s, c).await,
        _ => Json(json!({ "ok": false, "error": "unknown command" })),
    }
}

async fn set_delay(s: State<AppState>, Path(secs): Path<u32>) -> Json<Value> {
    run(s, Cmd::Set(secs)).await
}
async fn add_delay(s: State<AppState>, Path(secs): Path<i64>) -> Json<Value> {
    run(s, Cmd::Add(secs)).await
}

/// The config without secrets: keys are reported as set/unset only.
fn public_config(c: &Config) -> Value {
    let mut v = serde_json::to_value(c).unwrap_or_default();
    let o = v.as_object_mut().unwrap();
    o.remove("api_token");
    o.insert("stream_key".into(), json!(""));
    o.insert("stream_key_set".into(), json!(!c.stream_key.is_empty()));
    if let Some(dests) = o.get_mut("destinations").and_then(Value::as_array_mut) {
        for (d, src) in dests.iter_mut().zip(&c.destinations) {
            d["key"] = json!("");
            d["key_set"] = json!(!src.key.is_empty());
        }
    }
    o.insert("clips_path".into(), json!(c.clips_path().display().to_string()));
    v
}

async fn get_config(State(s): State<AppState>) -> impl IntoResponse {
    Json(public_config(&s.shared.config.lock().unwrap()))
}

/// Merges `patch` into `dst`: objects are merged key by key, anything else
/// (numbers, strings, arrays) is replaced.
fn merge_json(dst: &mut Value, patch: &Value) {
    match (dst, patch) {
        (Value::Object(d), Value::Object(p)) => {
            for (k, v) in p {
                merge_json(d.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (d, p) => *d = p.clone(),
    }
}

/// Saved key of a posted destination sent without one: found by its id, so a
/// rename keeps the key. Without an id (older clients) only an unambiguous name counts.
fn saved_key(cur: &Config, posted: &Value) -> String {
    let id = posted.get("id").and_then(Value::as_u64).unwrap_or(0);
    let old = if id != 0 {
        cur.destinations.iter().find(|o| o.id == id)
    } else {
        let name = posted.get("name").and_then(Value::as_str).unwrap_or("");
        let mut same = cur.destinations.iter().filter(|o| o.name == name);
        match (same.next(), same.next()) {
            (Some(o), None) => Some(o),
            _ => None,
        }
    };
    old.map(|o| o.key.clone()).unwrap_or_default()
}

/// Merges the posted fields into the config. Secrets are only replaced when sent;
/// addresses, ports and the token cannot be changed from here. Nested settings
/// (`features`, `overlay`, ...) can be sent in part, e.g. `{"features":{"chat":true}}`.
fn merge_config(cur: &Config, patch: &Value) -> Result<Config, String> {
    let mut v = serde_json::to_value(cur).map_err(|e| e.to_string())?;
    let Some(p) = patch.as_object() else { return Err("expected a JSON object".into()) };
    for (k, val) in p {
        if matches!(k.as_str(), "api_token" | "listen" | "http_listen" | "udp_listen" | "stream_key_set" | "clips_path") {
            continue;
        }
        if k == "stream_key" && val.as_str().is_none_or(str::is_empty) {
            continue; // keep the saved key unless a new one is typed
        }
        if k == "destinations" {
            let mut dests = val.clone();
            if let Some(arr) = dests.as_array_mut() {
                for d in arr.iter_mut() {
                    let typed = d.get("key").and_then(Value::as_str).is_some_and(|k| !k.is_empty());
                    if !typed {
                        d["key"] = json!(saved_key(cur, d));
                    }
                    if let Some(o) = d.as_object_mut() {
                        o.remove("key_set");
                    }
                }
            }
            v[k] = dests;
            continue;
        }
        merge_json(&mut v[k], val);
    }
    let mut cfg: Config = serde_json::from_value(v).map_err(|e| e.to_string())?;
    let valid = |u: &str| u.starts_with("rtmp://") || u.starts_with("rtmps://");
    if !valid(cfg.upstream_url.trim()) || cfg.destinations.iter().any(|d| !valid(d.url.trim())) {
        return Err(t!("the URL must start with rtmp:// or rtmps://", "a URL precisa começar com rtmp:// ou rtmps://", "la URL debe empezar con rtmp:// o rtmps://"));
    }
    cfg.upstream_url = cfg.upstream_url.trim().to_string();
    cfg.stream_key = cfg.stream_key.trim().to_string();
    for d in &mut cfg.destinations {
        d.url = d.url.trim().to_string();
        d.key = d.key.trim().to_string();
        if d.name.trim().is_empty() {
            d.name = url::Url::parse(&d.url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or("?".into());
        }
    }
    cfg.normalize();
    Ok(cfg)
}

async fn set_config(State(s): State<AppState>, Json(patch): Json<Value>) -> impl IntoResponse {
    // merged and stored under one lock, so two saves at once cannot undo each other
    let (delay, delay_changed, dest_changed) = {
        let mut cur = s.shared.config.lock().unwrap();
        let cfg = match merge_config(&cur, &patch) {
            Ok(c) => c,
            Err(e) => return Json(json!({ "ok": false, "error": e })),
        };
        let delay_changed = cfg.delay_seconds != cur.delay_seconds;
        let dest_changed = cfg.upstream_url != cur.upstream_url
            || cfg.stream_key != cur.stream_key
            || cfg.main_auto_start != cur.main_auto_start
            || cfg.main_video_kbps != cur.main_video_kbps
            || cfg.destinations != cur.destinations;
        if cfg.language != cur.language {
            i18n::set(Lang::parse(&cfg.language));
        }
        let delay = cfg.delay_seconds;
        *cur = cfg;
        (delay, delay_changed, dest_changed)
    };
    s.shared.save_config();
    if delay_changed {
        let _ = s.tx.send(EngineMsg::Cmd(Cmd::Set(delay)));
    }
    if dest_changed {
        // applied to the running stream right away
        let _ = s.tx.send(EngineMsg::DestinationsChanged);
    }
    Json(json!({ "ok": true, "next_stream": false }))
}

async fn overlay_page(State(s): State<AppState>) -> Response {
    if !s.shared.config.lock().unwrap().features.overlay {
        return (StatusCode::NOT_FOUND, "overlay is turned off").into_response();
    }
    Html(OVERLAY).into_response()
}

/// Only what the on-screen widget shows, plus its look.
async fn overlay_state(State(s): State<AppState>) -> Json<Value> {
    let (on, look, language) = {
        let c = s.shared.config.lock().unwrap();
        (c.features.overlay, c.overlay.clone(), c.language.clone())
    };
    if !on {
        return Json(json!({ "on": false }));
    }
    let st = s.shared.status.lock().unwrap();
    Json(json!({
        "on": true,
        "lang": language,
        "look": look,
        "enabled": st.enabled,
        "delay_seconds": st.delay_seconds,
        "live": st.engine.is_some(),
        "phase": st.engine.as_ref().map(|e| e.phase),
        "current_ms": st.engine.as_ref().map_or(0, |e| e.current_ms),
        "target_ms": st.engine.as_ref().map_or(0, |e| e.target_ms),
        "replaying": st.engine.as_ref().is_some_and(|e| e.replaying),
        "auto_off_s": st.auto_off_s,
    }))
}

/// Push events for bots (Server-Sent Events): one JSON object per change, with
/// its `type` also as the SSE event name. A `hello` with the current state comes first.
async fn events_handler(State(s): State<AppState>) -> impl IntoResponse {
    let rx = s.shared.events.subscribe();
    let lan = s.shared.config.lock().unwrap().features.phone;
    let hello = {
        let st = s.shared.status.lock().unwrap();
        json!({
            "type": "hello",
            "version": st.version,
            "enabled": st.enabled,
            "delay_seconds": st.delay_seconds,
            "phase": st.engine.as_ref().map_or(json!("offline"), |e| json!(e.phase)),
            "panic": st.panic,
            "auto_off_s": st.auto_off_s,
        })
    };
    let first = futures_util::stream::once(async move { Ok::<_, std::convert::Infallible>(SseEvent::default().event("hello").data(hello.to_string())) });
    let tick = tokio::time::interval(Duration::from_secs(1));
    let rest = futures_util::stream::unfold((rx, s.shared.clone(), tick), move |(mut rx, shared, mut tick)| async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            tokio::select! {
                r = rx.recv() => match r {
                    Ok(ev) => return Some((Ok(SseEvent::default().event(ev.kind).data(ev.json)), (rx, shared, tick))),
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => return None,
                },
                // the server rebinds when "LAN access" is switched: end the stream so it can
                _ = tick.tick() => {
                    if shared.config.lock().unwrap().features.phone != lan {
                        return None;
                    }
                }
            }
        }
    });
    Sse::new(futures_util::StreamExt::chain(first, rest)).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("keep-alive"))
}

/// Adds the widget to the current OBS scene as a Browser Source.
async fn obs_overlay(State(s): State<AppState>) -> impl IntoResponse {
    let port = s.shared.config.lock().unwrap().http_port();
    queue_obs_action(&s, ObsAction::AddOverlay(format!("http://127.0.0.1:{port}/overlay")))
}

fn queue_obs_action(s: &AppState, action: ObsAction) -> Json<Value> {
    let mut b = s.shared.bridge.lock().unwrap();
    if !b.info().script {
        return Json(json!({
            "ok": false,
            "error": t!("the OBS script is not running (Tools > Scripts)", "o script do OBS não está rodando (Ferramentas > Scripts)", "el script de OBS no se está ejecutando (Herramientas > Scripts)")
        }));
    }
    b.pending.push_back(action);
    Json(json!({ "ok": true }))
}

/// Starts or stops the stream in OBS ("Go live" in the panel). The destinations
/// set to start with the stream go live with it.
async fn obs_stream(State(s): State<AppState>, Path(action): Path<String>) -> impl IntoResponse {
    let want = match action.as_str() {
        "start" => true,
        "stop" => false,
        _ => return Json(json!({ "ok": false, "error": "use start or stop" })),
    };
    if want {
        let c = s.shared.config.lock().unwrap();
        let extras = c.features.multistream && c.destinations.iter().any(|d| d.enabled && d.auto_start && !d.url.is_empty());
        if !c.main_auto_start && !extras {
            return Json(json!({
                "ok": false,
                "error": t!("Pick at least one platform to go live on.", "Escolha pelo menos uma plataforma para entrar ao vivo.", "Elige al menos una plataforma para entrar en directo.")
            }));
        }
    }
    if s.shared.bridge.lock().unwrap().streaming == want {
        return Json(json!({ "ok": true }));
    }
    queue_obs_action(&s, ObsAction::ToggleStream)
}

async fn obs_configure(State(s): State<AppState>) -> impl IntoResponse {
    queue_obs_action(&s, ObsAction::Configure)
}
async fn obs_restore(State(s): State<AppState>) -> impl IntoResponse {
    queue_obs_action(&s, ObsAction::Restore)
}
/// Starts or stops one destination of the running stream (`start`, `stop`, `toggle`).
async fn output_action(State(s): State<AppState>, Path((id, action)): Path<(u64, String)>) -> Json<Value> {
    let cmd = match action.as_str() {
        "start" => Cmd::OutputStart(id),
        "stop" => Cmd::OutputStop(id),
        "toggle" => Cmd::OutputToggle(id),
        _ => return Json(json!({ "ok": false, "error": "use start, stop or toggle" })),
    };
    if !s.shared.status.lock().unwrap().outputs.iter().any(|o| o.id == id) {
        return Json(json!({ "ok": false, "error": t!("no such destination (is the stream running?)", "destino não encontrado (a live está rodando?)", "no se encontró el destino (¿la transmisión está en curso?)") }));
    }
    let _ = s.tx.send(EngineMsg::Cmd(cmd));
    Json(json!({ "ok": true }))
}

async fn update_check(State(s): State<AppState>) -> impl IntoResponse {
    if !s.shared.config.lock().unwrap().features.update_check {
        return Json(json!({ "ok": false, "error": t!("the update notice is turned off", "o aviso de atualização está desligado", "el aviso de actualización está desactivado") }));
    }
    s.shared.update_now.store(true, std::sync::atomic::Ordering::Relaxed);
    Json(json!({ "ok": true }))
}

async fn obs_fps(State(s): State<AppState>, Path(fps): Path<u32>) -> impl IntoResponse {
    if ![24, 25, 30, 48, 50, 60].contains(&fps) {
        return Json(json!({ "ok": false, "error": t!("unsupported frame rate", "taxa de quadros não suportada", "velocidad de fotogramas no compatible") }));
    }
    queue_obs_action(&s, ObsAction::SetFps(fps))
}

/// Opens one of a fixed set of places on the streamer's PC (never an arbitrary URL).
async fn open_target(State(s): State<AppState>, Path(target): Path<String>) -> impl IntoResponse {
    let what = match target.as_str() {
        "author" => AUTHOR_URL.to_string(),
        "releases" => RELEASES_URL.to_string(),
        "discord" => DISCORD_URL.to_string(),
        "support" => match i18n::get() {
            Lang::Pt => "https://github.com/ragnarcb/obs-dynamic-delay/blob/main/README.pt-BR.md#apoie-o-projeto".to_string(),
            Lang::Es => "https://github.com/ragnarcb/obs-dynamic-delay/blob/main/README.es.md#apoya-el-proyecto".to_string(),
            Lang::En => "https://github.com/ragnarcb/obs-dynamic-delay#support-the-project".to_string(),
        },
        "deck" => {
            let c = s.shared.config.lock().unwrap();
            format!("http://127.0.0.1:{}/deck?token={}", c.http_port(), c.api_token)
        }
        "clips" => {
            let dir = s.shared.config.lock().unwrap().clips_path();
            let _ = std::fs::create_dir_all(&dir);
            dir.display().to_string()
        }
        _ => return Json(json!({ "ok": false })),
    };
    let r = if cfg!(windows) {
        if target == "clips" {
            std::process::Command::new("explorer").arg(&what).spawn()
        } else {
            std::process::Command::new("cmd").args(["/C", "start", "", &what]).spawn()
        }
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(&what).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(&what).spawn()
    };
    Json(json!({ "ok": r.is_ok() }))
}

/// Runs the action of a phone deck key. The phone only sends the key number;
/// what it does comes from the saved deck, so a key can never do anything else.
async fn deck_press(State(s): State<AppState>, Path(index): Path<usize>) -> Json<Value> {
    let (on, key) = {
        let c = s.shared.config.lock().unwrap();
        (c.features.phone, c.deck.keys.get(index).cloned())
    };
    if !on {
        return Json(json!({ "ok": false, "error": t!("Phone deck is turned off.", "O deck no celular está desativado.", "El deck en el celular está desactivado.") }));
    }
    let Some(key) = key else { return Json(json!({ "ok": false, "error": "no such key" })) };
    let secs = key.arg.trim().parse::<u32>().ok();
    let cmd = match key.action.as_str() {
        "delay.toggle" => Some(Cmd::Toggle),
        "delay.on" => Some(Cmd::On),
        "delay.off" => Some(Cmd::Off),
        "delay.set" => secs.map(Cmd::Set),
        "delay.add" => key.arg.trim().parse::<i64>().ok().map(Cmd::Add),
        // on, then off after the minutes (empty or 0: cancel the timer)
        "delay.timer" => match secs.unwrap_or(0) {
            0 => Some(Cmd::AutoOff(0)),
            m => Some(Cmd::OnFor(m, None)),
        },
        "censor" => Some(Cmd::Censor(secs)),
        "replay" => Some(Cmd::Replay(secs)),
        "clip" => Some(Cmd::Clip(secs)),
        "panic" => Some(Cmd::Panic),
        "catchup" => Some(Cmd::CatchUp),
        "dest.toggle" => {
            // by name; "main"/"principal" (or empty) is the main destination
            let st = s.shared.status.lock().unwrap();
            let arg = key.arg.trim().to_lowercase();
            let id = if arg.is_empty() || arg == "main" || arg == "principal" {
                st.outputs.iter().find(|o| o.id == 0).map(|o| o.id)
            } else {
                st.outputs.iter().find(|o| o.name.to_lowercase() == arg).map(|o| o.id)
            };
            match id {
                Some(id) => Some(Cmd::OutputToggle(id)),
                None => {
                    return Json(json!({ "ok": false, "error": t!("start the stream first (or check the destination name)", "inicie a live primeiro (ou confira o nome do destino)", "inicia la transmisión primero (o revisa el nombre del destino)") }));
                }
            }
        }
        _ => None,
    };
    if let Some(cmd) = cmd {
        let _ = s.tx.send(EngineMsg::Cmd(cmd));
        if key.action == "delay.set" {
            let _ = s.tx.send(EngineMsg::Cmd(Cmd::On));
        }
        return Json(json!({ "ok": true }));
    }
    let action = match key.action.as_str() {
        "obs.scene" if !key.arg.is_empty() => ObsAction::Scene(key.arg.clone()),
        "obs.mute" if !key.arg.is_empty() => ObsAction::ToggleMute(key.arg.clone()),
        "obs.stream" => ObsAction::ToggleStream,
        "obs.record" => ObsAction::ToggleRecord,
        _ => return Json(json!({ "ok": false, "error": "key not set up" })),
    };
    queue_obs_action(&s, action)
}

/// Address of this PC on the local network.
fn lan_ip() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?; // no packet is sent, this only picks the route
    s.local_addr().ok().map(|a| a.ip())
}

async fn lan_info(State(s): State<AppState>) -> impl IntoResponse {
    let (enabled, port, token) = {
        let c = s.shared.config.lock().unwrap();
        (c.features.phone, c.http_port(), c.api_token.clone())
    };
    let Some(ip) = lan_ip() else {
        return Json(json!({ "enabled": enabled, "url": null, "qr": null }));
    };
    let url = format!("http://{ip}:{port}/deck?token={token}");
    let qr = qrcode::QrCode::new(url.as_bytes())
        .map(|c| c.render::<qrcode::render::svg::Color>().min_dimensions(180, 180).quiet_zone(true).build())
        .ok();
    Json(json!({ "enabled": enabled, "url": url, "qr": qr }))
}

/// Text commands over UDP, used by the OBS script:
/// * delay commands (`toggle`, `set 30`, `censor`, ...), see [`Cmd::parse`]
/// * `status`: answered with a human readable summary
/// * `poll <configured 0|1>`: answered with a pending [`ObsAction`] or `none`
/// * `scene_shown` / `scene_failed`: outcome of `scene_show`
/// * `scenes\t<name>...`: scene list; `program\t<name>`: scene now on air
/// * `import\t<server>\t<key>\t<service>`: destination found in OBS' stream settings
///   (only from the socket the script polls with, see [`from_script`])
/// * `result [ok|error\t]<text>`: outcome of an action, shown in the panel
pub async fn serve_udp(sock: UdpSocket, tx: UnboundedSender<EngineMsg>, shared: Arc<Shared>) -> Result<()> {
    log::info!("UDP commands on {}", sock.local_addr()?);
    let mut buf = vec![0u8; 4096];
    // address the OBS script polls from, and when it last did
    let mut script: Option<(SocketAddr, Instant)> = None;
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else { continue };
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        let (word, rest) = text.split_once([' ', '\t']).unwrap_or((text.trim(), ""));
        match word {
            "status" => {
                let summary = shared.status.lock().unwrap().summary();
                let _ = sock.send_to(summary.as_bytes(), from).await;
            }
            "poll" => {
                script = Some((from, Instant::now()));
                let reply = {
                    let mut b = shared.bridge.lock().unwrap();
                    b.last_poll = Some(Instant::now());
                    b.obs_configured = rest.trim() == "1";
                    b.pending.pop_front().map_or("none".to_string(), |a| a.encode())
                };
                let _ = sock.send_to(reply.as_bytes(), from).await;
            }
            "import" if from_script(script, from, Instant::now()) => import_from_obs(&shared, rest),
            "import" => log::warn!("ignored an import from {from}: not the OBS script"),
            "scene_shown" => {
                let _ = tx.send(EngineMsg::SceneShown(Instant::now()));
            }
            "scene_failed" => {
                log::warn!("OBS could not show the delay scene: {}", rest.trim());
                let _ = tx.send(EngineMsg::SceneFailed);
            }
            "scenes" => {
                shared.bridge.lock().unwrap().scenes =
                    rest.split('\t').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
            }
            "audio" => {
                // audio\t<name>=<0|1>\t...
                shared.bridge.lock().unwrap().audio = rest
                    .split('\t')
                    .filter_map(|kv| kv.rsplit_once('='))
                    .map(|(name, m)| AudioSource { name: name.to_string(), muted: m == "1" })
                    .collect();
            }
            "obsstate" => {
                // obsstate\t<streaming 0|1>\t<recording 0|1>[\t<width>\t<height>\t<fps num>\t<fps den>]
                let mut it = rest.split('\t');
                let mut b = shared.bridge.lock().unwrap();
                b.streaming = it.next() == Some("1");
                b.recording = it.next() == Some("1");
                let n: Vec<u32> = it.filter_map(|x| x.trim().parse().ok()).collect();
                b.video = match n[..] {
                    [width, height, num, den] if width > 0 && num > 0 && den > 0 => {
                        Some(ObsVideo { width, height, fps: (num as f64 / den as f64 * 100.0).round() / 100.0 })
                    }
                    _ => None,
                };
            }
            "program" => {
                let name = rest.trim().to_string();
                shared.bridge.lock().unwrap().program_scene = name.clone();
                let _ = tx.send(EngineMsg::ProgramScene(name));
            }
            "result" => {
                let (error, text) = parse_result(rest);
                log::info!("OBS: {text}");
                shared.bridge.lock().unwrap().set_message(text, error);
            }
            _ => match Cmd::parse(&text) {
                Some(cmd) => {
                    let _ = tx.send(EngineMsg::Cmd(cmd));
                }
                None => log::warn!("unknown UDP command {text:?}"),
            },
        }
    }
}

/// An `import` rewrites the stream URL and key without the API token, so it is only
/// taken from the socket that sent a script `poll` in the last few seconds: another
/// program on the PC cannot send from that same address.
fn from_script(script: Option<(SocketAddr, Instant)>, from: SocketAddr, now: Instant) -> bool {
    script.is_some_and(|(addr, at)| addr == from && now.saturating_duration_since(at) < Duration::from_secs(10))
}

/// `result error\t<text>` / `result ok\t<text>` / `result <text>` (older scripts: a success).
fn parse_result(rest: &str) -> (bool, &str) {
    let rest = rest.trim();
    match rest.split_once('\t') {
        Some(("error", text)) => (true, text.trim()),
        Some(("ok", text)) => (false, text.trim()),
        _ => (false, rest),
    }
}

fn import_from_obs(shared: &Shared, rest: &str) {
    let mut parts = rest.split('\t');
    let server = parts.next().unwrap_or("").trim();
    let key = parts.next().unwrap_or("").trim();
    let service = parts.next().unwrap_or("").trim();
    let Some(url) = destination_from_obs(server, service) else {
        log::info!("nothing to import from OBS (server {server:?}, service {service:?})");
        return;
    };
    {
        let mut c = shared.config.lock().unwrap();
        c.upstream_url = url.clone();
        if !key.is_empty() {
            c.stream_key = key.to_string();
        }
    }
    shared.save_config();
    log::info!("imported destination {url} from OBS");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Destination, Features};

    #[test]
    fn merge_keeps_secrets_unless_typed() {
        let mut cur = Config::default();
        cur.stream_key = "main-key".into();
        cur.api_token = "tok".into();
        cur.destinations.push(Destination { id: 1, name: "YT".into(), url: "rtmp://yt/live2".into(), key: "yt-key".into(), enabled: true, auto_start: true, video_kbps: 0 });
        let public = public_config(&cur);
        assert_eq!(public["stream_key"], "");
        assert!(public.get("api_token").is_none());
        assert_eq!(public["destinations"][0]["key"], "");

        // the panel sends back what it got, with one change
        let mut patch = public.clone();
        patch["delay_seconds"] = json!(45);
        patch["api_token"] = json!("hacked");
        let out = merge_config(&cur, &patch).unwrap();
        assert_eq!(out.stream_key, "main-key");
        assert_eq!(out.destinations[0].key, "yt-key");
        assert_eq!(out.api_token, "tok");
        assert_eq!(out.delay_seconds, 45);

        let out = merge_config(&cur, &json!({ "stream_key": "new" })).unwrap();
        assert_eq!(out.stream_key, "new");
        assert!(merge_config(&cur, &json!({ "upstream_url": "http://evil" })).is_err());
    }

    #[test]
    fn nested_settings_merge_in_part() {
        let cur = Config::default();
        let out = merge_config(&cur, &json!({ "features": { "chat": true }, "overlay": { "scale": 150 } })).unwrap();
        assert!(out.features.chat);
        assert_eq!(Features { chat: true, ..cur.features.clone() }, out.features, "the other switches are kept");
        assert_eq!(out.overlay.scale, 150);
        assert_eq!(out.overlay.style, cur.overlay.style);
        // arrays are still replaced whole
        let out = merge_config(&cur, &json!({ "deck": { "keys": [{ "action": "panic" }] } })).unwrap();
        assert_eq!(out.deck.keys.len(), 1);
        assert_eq!(out.deck.columns, cur.deck.columns);
    }

    #[test]
    fn destination_keys_follow_the_id() {
        let mut cur = Config::default();
        let d = |id: u64, name: &str, key: &str| Destination { id, name: name.into(), url: "rtmp://a/b".into(), key: key.into(), ..Destination::default() };
        cur.destinations = vec![d(1, "Kick", "k1"), d(2, "Kick", "k2")];
        let mut patch = public_config(&cur);
        patch["destinations"][0]["name"] = json!("Kick main"); // renamed
        let out = merge_config(&cur, &json!({ "destinations": patch["destinations"] })).unwrap();
        assert_eq!(out.destinations[0].name, "Kick main");
        assert_eq!(out.destinations[0].key, "k1");
        assert_eq!(out.destinations[1].key, "k2", "same name, own key");
        assert_eq!((out.destinations[0].id, out.destinations[1].id), (1, 2));

        // a new destination (no id) gets its own id and no saved key
        let out = merge_config(&cur, &json!({ "destinations": [patch["destinations"][1], { "name": "Kick", "url": "rtmp://c/d" }] })).unwrap();
        assert_eq!(out.destinations[0].key, "k2");
        assert_eq!(out.destinations[1].key, "", "two saved Kicks: the name alone is not enough");
        assert_eq!(out.destinations[1].id, 3);
    }

    #[test]
    fn token_compare() {
        assert!(same_secret(b"abc", b"abc"));
        assert!(!same_secret(b"abc", b"abd"));
        assert!(!same_secret(b"abc", b"abcd"));
        assert!(!same_secret(b"", b"abc"));
    }

    #[test]
    fn import_only_from_the_script() {
        let script: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let other: SocketAddr = "127.0.0.1:50001".parse().unwrap();
        let now = Instant::now();
        assert!(!from_script(None, script, now));
        assert!(from_script(Some((script, now)), script, now + Duration::from_secs(1)));
        assert!(!from_script(Some((script, now)), other, now));
        assert!(!from_script(Some((script, now)), script, now + Duration::from_secs(30)), "the script stopped polling");
    }

    #[test]
    fn results_carry_their_kind() {
        assert_eq!(parse_result("error\tStop the stream first. "), (true, "Stop the stream first."));
        assert_eq!(parse_result("ok\tDone!"), (false, "Done!"));
        assert_eq!(parse_result("Done! OBS streams to rtmp://x"), (false, "Done! OBS streams to rtmp://x"));
    }
}
