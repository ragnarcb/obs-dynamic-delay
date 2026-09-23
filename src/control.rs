//! HTTP control panel/API and UDP command port.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;

use crate::EngineMsg;
use crate::status::{Cmd, Status};

const PANEL: &str = include_str!("panel.html");

#[derive(Clone)]
struct AppState {
    tx: UnboundedSender<EngineMsg>,
    status: Arc<Mutex<Status>>,
}

pub async fn serve_http(listen: String, tx: UnboundedSender<EngineMsg>, status: Arc<Mutex<Status>>) -> Result<()> {
    let state = AppState { tx, status };
    let app = Router::new()
        .route("/", get(|| async { Html(PANEL) }))
        .route("/api/status", get(status_handler))
        .route("/api/on", get(on).post(on))
        .route("/api/off", get(off).post(off))
        .route("/api/toggle", get(toggle).post(toggle))
        .route("/api/delay/{secs}", get(set_delay).post(set_delay))
        .route("/api/add/{secs}", get(add_delay).post(add_delay))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    log::info!("control panel at http://{listen}/");
    axum::serve(listener, app).await?;
    Ok(())
}

fn send(s: &AppState, cmd: Cmd) -> impl IntoResponse + use<> {
    let _ = s.tx.send(EngineMsg::Cmd(cmd));
    Json(serde_json::json!({ "ok": true }))
}

async fn status_handler(State(s): State<AppState>) -> impl IntoResponse {
    Json(s.status.lock().unwrap().clone())
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

/// Text commands over UDP (used by the OBS script). `status` is answered with a summary.
pub async fn serve_udp(sock: UdpSocket, tx: UnboundedSender<EngineMsg>, status: Arc<Mutex<Status>>) -> Result<()> {
    log::info!("UDP commands on {}", sock.local_addr()?);
    let mut buf = [0u8; 256];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else { continue };
        let text = String::from_utf8_lossy(&buf[..n]);
        if text.trim() == "status" {
            let summary = status.lock().unwrap().summary();
            let _ = sock.send_to(summary.as_bytes(), from).await;
            continue;
        }
        match Cmd::parse(&text) {
            Some(cmd) => {
                let _ = tx.send(EngineMsg::Cmd(cmd));
            }
            None => log::warn!("unknown UDP command {text:?}"),
        }
    }
}
