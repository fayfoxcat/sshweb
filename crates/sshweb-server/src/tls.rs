//! Optional TLS support for the HTTP server (`--tls-cert` / `--tls-key`).
//!
//! Two local [`Listener`] wrappers (`NoDelayListener` for plain HTTP,
//! `TlsListener` wrapping it for HTTPS) keep the whole axum stack intact. The
//! client peer address is carried in the local `PeerInfo` connect-info type so
//! the auth rate limiter (`ConnectInfo<PeerInfo>`) works on both paths.

use std::fs::File;
use std::io::BufReader;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use rustls::ServerConfig;
use tokio::net::TcpListener as TokioTcpListener;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;

/// Connect info carrying the client peer address. A local type (not
/// `SocketAddr`) so the orphan rule allows implementing [`Connected`] for our
/// own listeners without conflicting with axum's `SocketAddr` impls.
#[derive(Clone, Debug)]
pub struct PeerInfo(pub SocketAddr);

/// Load a rustls server config from PEM certificate and private key files.
pub fn load_server_config(cert_path: &Path, key_path: &Path) -> Result<ServerConfig> {
    let certs = rustls_pemfile::certs(&mut BufReader::new(
        File::open(cert_path).with_context(|| format!("open TLS cert {}", cert_path.display()))?,
    ))
    .collect::<std::result::Result<Vec<_>, _>>()
    .context("parse TLS certificate")?;
    let key = rustls_pemfile::private_key(&mut BufReader::new(
        File::open(key_path).with_context(|| format!("open TLS key {}", key_path.display()))?,
    ))
    .context("parse TLS private key")?
    .ok_or_else(|| anyhow!("no private key found in {}", key_path.display()))?;
    // Use the ring provider explicitly (no reliance on a process-default
    // provider having been installed; ring is already a dependency via russh).
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .context("TLS protocol versions")?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build TLS server config")?;
    Ok(config)
}

/// A [`Listener`] that sets `TCP_NODELAY` on each accepted stream (replaces the
/// axum `tap_io` wrapper, which would conflict with the [`Connected`] impls
/// below).
pub struct NoDelayListener {
    inner: TokioTcpListener,
}

impl NoDelayListener {
    /// Wrap a `TcpListener`, applying `TCP_NODELAY` to every accepted stream.
    pub fn new(inner: TokioTcpListener) -> Self {
        Self { inner }
    }
}

impl Listener for NoDelayListener {
    type Addr = SocketAddr;
    type Io = tokio::net::TcpStream;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (stream, addr) = self.inner.accept().await.unwrap_or_else(|_| {
            // Unreachable in practice: axum 0.8's Listener::accept is infallible.
            panic!("TCP accept failed")
        });
        if let Err(err) = stream.set_nodelay(true) {
            tracing::debug!("failed to set TCP_NODELAY: {err:#}");
        }
        (stream, addr)
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// A [`Listener`] that wraps each accepted stream in a TLS session. axum 0.8's
/// [`Listener::accept`] is infallible, so a TLS handshake failure (a plaintext
/// client hitting the TLS port) simply accepts the next connection.
pub struct TlsListener<L> {
    inner: L,
    acceptor: TlsAcceptor,
}

impl<L> TlsListener<L> {
    /// Wrap a listener with a TLS server configuration.
    pub fn new(inner: L, config: ServerConfig) -> Self {
        Self {
            inner,
            acceptor: TlsAcceptor::from(Arc::new(config)),
        }
    }
}

impl<L: Listener> Listener for TlsListener<L> {
    type Addr = L::Addr;
    type Io = TlsStream<L::Io>;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (raw, addr) = self.inner.accept().await;
            match self.acceptor.accept(raw).await {
                Ok(tls) => return (tls, addr),
                Err(_) => continue, // handshake failed; accept the next connection
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// Provide the peer address on the plain HTTP path.
impl Connected<IncomingStream<'_, NoDelayListener>> for PeerInfo {
    fn connect_info(stream: IncomingStream<'_, NoDelayListener>) -> Self {
        PeerInfo(*stream.remote_addr())
    }
}

/// Provide the peer address on the TLS path.
impl<L> Connected<IncomingStream<'_, TlsListener<L>>> for PeerInfo
where
    L: Listener<Addr = SocketAddr>,
{
    fn connect_info(stream: IncomingStream<'_, TlsListener<L>>) -> Self {
        PeerInfo(*stream.remote_addr())
    }
}
