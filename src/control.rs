//! HTTP control panel/API and UDP command port.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;
use tower_http::cors::CorsLayer;

use crate::EngineMsg;
use crate::config::destination_from_obs;
use crate::status::{Cmd, ObsAction, ObsInfo, Shared, Status};

pub const PANEL: &str = include_str!("panel.html");
const CLOCK: &str = include_str!("clock.html");

#[derive(Clone)]
struct AppState {
    tx: UnboundedSender<EngineMsg>,
    shared: Arc<Shared>,
}

pub async fn serve_http(listen: String, tx: UnboundedSender<EngineMsg>, shared: Arc<Shared>) -> Result<()> {
    let state = AppState { tx, shared };
    let app = Router::new()
        .route("/", get(|| async { Html(PANEL) }))
        .route("/clock", get(|| async { Html(CLOCK) }))
        .route("/api/status", get(status_handler))
        .route("/api/on", get(on).post(on))
        .route("/api/off", get(off).post(off))
        .route("/api/toggle", get(toggle).post(toggle))
        .route("/api/delay/{secs}", get(set_delay).post(set_delay))
        .route("/api/add/{secs}", get(add_delay).post(add_delay))
        .route("/api/config", get(get_config).post(set_config))
        .route("/api/obs/configure", post(obs_configure))
        .route("/api/obs/restore", post(obs_restore))
        // the OBS dock loads the panel from a local file, so allow cross-origin calls
        .layer(CorsLayer::permissive())
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    log::info!("control panel at http://{listen}/");
    axum::serve(listener, app).await?;
    Ok(())
}

fn send(s: &AppState, cmd: Cmd) -> Json<serde_json::Value> {
    let _ = s.tx.send(EngineMsg::Cmd(cmd));
    Json(serde_json::json!({ "ok": true }))
}

#[derive(Serialize)]
struct FullStatus {
    #[serde(flatten)]
    status: Status,
    obs: ObsInfo,
}

async fn status_handler(State(s): State<AppState>) -> impl IntoResponse {
    Json(FullStatus {
        status: s.shared.status.lock().unwrap().clone(),
        obs: s.shared.bridge.lock().unwrap().info(),
    })
}
async fn on(State(s): State<AppState>) -> impl IntoResponse {
    send(&s, Cmd::On)
}
async fn off(State(s): State<AppState>) -> impl IntoResponse {
    send(&s, Cmd::Off)
}
async fn toggle(State(s): State<AppState>) -> impl IntoResponse {
    send(&s, Cmd::Toggle)
}
async fn set_delay(State(s): State<AppState>, Path(secs): Path<u32>) -> impl IntoResponse {
    send(&s, Cmd::Set(secs))
}
async fn add_delay(State(s): State<AppState>, Path(secs): Path<i64>) -> impl IntoResponse {
    send(&s, Cmd::Add(secs))
}

#[derive(Serialize, Deserialize)]
struct ConfigView {
    upstream_url: String,
    /// Only reported as set/unset; never sent back in full.
    #[serde(default)]
    stream_key_set: bool,
    /// When present in a POST, replaces the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stream_key: Option<String>,
    start_enabled: bool,
    delay_seconds: u32,
    #[serde(default)]
    grow_mode: Option<String>,
    #[serde(default)]
    delay_scene: Option<String>,
    #[serde(default)]
    max_delay_seconds: u32,
}

async fn get_config(State(s): State<AppState>) -> impl IntoResponse {
    let c = s.shared.config.lock().unwrap().clone();
    Json(ConfigView {
        upstream_url: c.upstream_url,
        stream_key_set: !c.stream_key.is_empty(),
        stream_key: None,
        start_enabled: c.start_enabled,
        delay_seconds: c.delay_seconds,
        grow_mode: Some(c.grow_mode),
        delay_scene: Some(c.delay_scene),
        max_delay_seconds: c.max_delay_seconds,
    })
}

async fn set_config(State(s): State<AppState>, Json(v): Json<ConfigView>) -> impl IntoResponse {
    let url = v.upstream_url.trim().to_string();
    if !(url.starts_with("rtmp://") || url.starts_with("rtmps://")) {
        return Json(serde_json::json!({ "ok": false, "error": "a URL precisa começar com rtmp:// ou rtmps://" }));
    }
    {
        let mut c = s.shared.config.lock().unwrap();
        c.upstream_url = url;
        if let Some(k) = v.stream_key {
            c.stream_key = k.trim().to_string();
        }
        c.start_enabled = v.start_enabled;
        if let Some(m) = v.grow_mode.filter(|m| ["rewind", "scene", "freeze"].contains(&m.as_str())) {
            c.grow_mode = m;
        }
        if let Some(sc) = v.delay_scene {
            c.delay_scene = sc.trim().to_string();
        }
    }
    s.shared.save_config();
    let _ = s.tx.send(EngineMsg::Cmd(Cmd::Set(v.delay_seconds)));
    let live = s.shared.status.lock().unwrap().engine.is_some();
    log::info!("config updated from panel");
    Json(serde_json::json!({ "ok": true, "next_stream": live }))
}

fn queue_obs_action(s: &AppState, action: ObsAction) -> Json<serde_json::Value> {
    let mut b = s.shared.bridge.lock().unwrap();
    if !b.info().script {
        return Json(serde_json::json!({
            "ok": false,
            "error": "o script do OBS não está rodando (Ferramentas > Scripts)"
        }));
    }
    b.pending.push_back(action);
    Json(serde_json::json!({ "ok": true }))
}

async fn obs_configure(State(s): State<AppState>) -> impl IntoResponse {
    queue_obs_action(&s, ObsAction::Configure)
}
async fn obs_restore(State(s): State<AppState>) -> impl IntoResponse {
    queue_obs_action(&s, ObsAction::Restore)
}

/// Text commands over UDP, used by the OBS script:
/// * delay commands (`toggle`, `set 30`, ...), see [`Cmd::parse`]
/// * `status`: answered with a human readable summary
/// * `poll <configured 0|1>`: answered with a pending action (`configure`, `restore`,
///   `scene_show\t<name>`, `scene_back`) or `none`
/// * `scene_shown` / `scene_failed`: outcome of `scene_show`
/// * `scenes\t<name>\t<name>...`: scene list for the panel
/// * `import\t<server>\t<key>\t<service>`: destination found in OBS' stream settings
/// * `result <text>`: outcome of an action, shown in the panel
pub async fn serve_udp(sock: UdpSocket, tx: UnboundedSender<EngineMsg>, shared: Arc<Shared>) -> Result<()> {
    log::info!("UDP commands on {}", sock.local_addr()?);
    let mut buf = vec![0u8; 4096];
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
                let reply = {
                    let mut b = shared.bridge.lock().unwrap();
                    b.last_poll = Some(Instant::now());
                    b.obs_configured = rest.trim() == "1";
                    match b.pending.pop_front() {
                        Some(ObsAction::Configure) => "configure".to_string(),
                        Some(ObsAction::Restore) => "restore".to_string(),
                        Some(ObsAction::ShowScene(name)) => format!("scene_show\t{name}"),
                        Some(ObsAction::SceneBack) => "scene_back".to_string(),
                        None => "none".to_string(),
                    }
                };
                let _ = sock.send_to(reply.as_bytes(), from).await;
            }
            "import" => import_from_obs(&shared, rest),
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
            "result" => {
                log::info!("OBS: {}", rest.trim());
                shared.bridge.lock().unwrap().message = Some((Instant::now(), rest.trim().to_string()));
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
