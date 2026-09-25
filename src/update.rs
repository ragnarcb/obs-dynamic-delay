//! Update notice: asks GitHub for the latest release when the relay starts,
//! every few hours and when the panel asks ("Check now"). With the feature
//! off it makes no connection at all.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::status::Shared;
use crate::t;

const HOST: &str = "api.github.com";
const PATH: &str = "/repos/ragnarcb/obs-dynamic-delay/releases/latest";
const EVERY: Duration = Duration::from_secs(6 * 3600);
/// After a failure (offline, GitHub down), try again sooner.
const RETRY: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug, Default, Serialize)]
pub struct UpdateStatus {
    /// Latest published version, without the "v".
    pub latest: Option<String>,
    /// `latest` is newer than this relay.
    pub available: bool,
    /// Local time of the last check ("14:05").
    pub checked_at: Option<String>,
    pub checking: bool,
    pub error: Option<String>,
}

pub async fn run(shared: Arc<Shared>) {
    let mut next: Option<Instant> = Some(Instant::now());
    loop {
        let on = shared.config.lock().unwrap().features.update_check;
        let asked = shared.update_now.swap(false, Ordering::Relaxed);
        if !on {
            shared.status.lock().unwrap().update = None;
            next = Some(Instant::now()); // check as soon as it is switched back on
        } else if asked || next.is_some_and(|t| Instant::now() >= t) {
            shared.status.lock().unwrap().update.get_or_insert_with(UpdateStatus::default).checking = true;
            let result = latest_version().await;
            let found = apply(&shared, result);
            next = Some(Instant::now() + if found { EVERY } else { RETRY });
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Stores the result; returns false when the check failed.
fn apply(shared: &Shared, result: Result<String>) -> bool {
    let current = env!("CARGO_PKG_VERSION");
    let now = jiff::Zoned::now().strftime("%H:%M").to_string();
    let mut st = shared.status.lock().unwrap();
    let was = st.update.as_ref().and_then(|u| u.available.then(|| u.latest.clone())).flatten();
    let u = st.update.get_or_insert_with(UpdateStatus::default);
    u.checking = false;
    u.checked_at = Some(now);
    match result {
        Ok(latest) => {
            u.available = newer(&latest, current);
            u.latest = Some(latest.clone());
            u.error = None;
            let tell = u.available && was.as_deref() != Some(latest.as_str());
            drop(st);
            if tell {
                log::info!("update available: v{latest} (running v{current})");
                shared.event("ok", t!("Version {latest} is available (you have {current}).", "A versão {latest} está disponível (você tem a {current})."));
            }
            true
        }
        Err(e) => {
            log::warn!("update check failed: {e:#}");
            u.error = Some(format!("{e:#}"));
            false
        }
    }
}

/// `a` is a newer version than `b` ("0.10.0" > "0.9.1").
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.split('.').map(|x| x.trim().parse().unwrap_or(0)).collect() };
    let (x, y) = (parse(a), parse(b));
    for i in 0..x.len().max(y.len()) {
        let (p, q) = (x.get(i).copied().unwrap_or(0), y.get(i).copied().unwrap_or(0));
        if p != q {
            return p > q;
        }
    }
    false
}

async fn latest_version() -> Result<String> {
    tokio::time::timeout(Duration::from_secs(15), fetch()).await.context("timed out")?
}

async fn fetch() -> Result<String> {
    let tcp = TcpStream::connect((HOST, 443)).await.context("connecting to GitHub")?;
    let connector = tokio_native_tls::TlsConnector::from(tokio_native_tls::native_tls::TlsConnector::new()?);
    let mut s = connector.connect(HOST, tcp).await.context("TLS with GitHub")?;
    let req = format!(
        "GET {PATH} HTTP/1.1\r\nHost: {HOST}\r\nUser-Agent: obs-dynamic-delay/{}\r\nAccept: application/vnd.github+json\r\nConnection: close\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    s.write_all(req.as_bytes()).await?;
    let mut raw = Vec::new();
    s.take(2 << 20).read_to_end(&mut raw).await?;
    parse_response(&raw)
}

/// Reads the tag of the latest release out of the HTTP response.
fn parse_response(raw: &[u8]) -> Result<String> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n").context("bad HTTP response")?;
    let code = head.split_whitespace().nth(1).unwrap_or("");
    if code != "200" {
        bail!("GitHub answered {code}");
    }
    let body = if head.to_ascii_lowercase().contains("transfer-encoding: chunked") { dechunk(body) } else { body.to_string() };
    let v: serde_json::Value = serde_json::from_str(&body).context("reading the GitHub answer")?;
    let tag = v["tag_name"].as_str().context("no tag_name")?;
    Ok(tag.trim_start_matches('v').to_string())
}

fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, after)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16).unwrap_or(0);
        if n == 0 || after.len() < n {
            break;
        }
        out.push_str(&after[..n]);
        rest = after[n..].trim_start_matches("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("0.9.0", "0.8.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.99.1"));
        assert!(!newer("0.8.0", "0.8.0"));
        assert!(!newer("0.7.5", "0.8.0"));
    }

    #[test]
    fn reads_plain_and_chunked_answers() {
        let plain = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"tag_name\":\"v0.9.0\",\"name\":\"x\"}";
        assert_eq!(parse_response(plain).unwrap(), "0.9.0");
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n{\"tag_na\r\n12\r\nme\":\"v1.2.3\",\"a\":1\r\n1\r\n}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked).unwrap(), "1.2.3");
        assert!(parse_response(b"HTTP/1.1 403 Forbidden\r\n\r\n{}").is_err());
    }
}
