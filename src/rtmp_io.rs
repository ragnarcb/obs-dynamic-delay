//! Small async helpers around the sans-IO rml_rtmp handshake.

use anyhow::{Result, bail};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// Runs the RTMP handshake. Returns bytes received after the handshake.
pub async fn handshake<S: Io>(stream: &mut S, peer: PeerType) -> Result<Vec<u8>> {
    let is_client = peer == PeerType::Client;
    let mut hs = Handshake::new(peer);
    if is_client {
        let p0p1 = hs.generate_outbound_p0_and_p1()?;
        stream.write_all(&p0p1).await?;
    }
    let mut buf = vec![0u8; 8192];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            bail!("connection closed during handshake");
        }
        match hs.process_bytes(&buf[..n])? {
            HandshakeProcessResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).await?;
                }
            }
            HandshakeProcessResult::Completed { response_bytes, remaining_bytes } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).await?;
                }
                return Ok(remaining_bytes);
            }
        }
    }
}
