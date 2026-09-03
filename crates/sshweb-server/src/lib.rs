//! The sshweb server, which provides web-based terminal access.
//!
//! Each browser connection opens a WebSocket to the server, which spawns a
//! local shell in a PTY and proxies terminal I/O between the browser and the
//! shell process.

#![warn(missing_docs)]

use std::{fmt::Debug, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Result;
use axum::extract::connect_info::Connected;
use axum::serve::{IncomingStream, Listener};
use tokio::net::TcpListener;
use utils::Shutdown;

use crate::state::ServerState;
use crate::tls::PeerInfo;

pub mod config;
mod listen;
pub mod proxy;
pub mod runner;
pub mod session;
pub mod sftp;
pub mod ssh;
pub mod state;
pub mod stats;
pub mod terminal;
pub mod tls;
pub mod utils;
pub mod web;

/// Options when constructing the application server.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct ServerOptions {
    /// Command used to spawn terminal shells.
    pub shell: Option<String>,
    /// Path to the encrypted server configuration file.
    pub config_path: Option<PathBuf>,
    /// Idle sessions (no attached WebSocket client) are reclaimed after this
    /// many seconds; defaults to `state::SESSION_RECLAIM_SECS`.
    pub session_ttl: Option<u64>,
    /// PEM TLS certificate; both it and `tls_key` must be set to serve HTTPS.
    pub tls_cert: Option<PathBuf>,
    /// PEM TLS private key; both it and `tls_cert` must be set to serve HTTPS.
    pub tls_key: Option<PathBuf>,
}

/// Stateful object that manages the sshweb server, with graceful termination.
pub struct Server {
    state: Arc<ServerState>,
    shutdown: Shutdown,
    tls: Option<(PathBuf, PathBuf)>,
}

impl Server {
    /// Create a new application server, but do not listen for connections yet.
    pub fn new(options: ServerOptions) -> Result<Self> {
        let tls = match (options.tls_cert.clone(), options.tls_key.clone()) {
            (Some(cert), Some(key)) => Some((cert, key)),
            _ => None,
        };
        Ok(Self {
            state: Arc::new(ServerState::new(options)?),
            shutdown: Shutdown::new(),
            tls,
        })
    }

    /// Returns the server's state object.
    pub fn state(&self) -> Arc<ServerState> {
        Arc::clone(&self.state)
    }

    /// Run the application server, listening on a connection stream. The
    /// listener must expose `SocketAddr` as its address so `ConnectInfo` (the
    /// auth rate limiter's client IP) is available; the concrete listeners are
    /// a `NoDelayListener` (plain HTTP) or `TlsListener<NoDelayListener>`
    /// (HTTPS, see `tls.rs`).
    pub async fn listen<L>(&self, listener: L) -> Result<()>
    where
        L: Listener<Addr = SocketAddr> + Send + 'static,
        for<'a> PeerInfo: Connected<IncomingStream<'a, L>>,
    {
        let state = self.state.clone();
        let terminated = self.shutdown.wait();
        tokio::spawn(async move {
            tokio::select! {
                _ = terminated => state.shutdown(),
            }
        });

        // Reap idle sessions (no attached client for the TTL) on a sweep.
        let sweep_state = self.state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                sweep_state.reclaim_idle();
            }
        });

        listen::start_server(self.state(), listener, self.shutdown.wait()).await
    }

    /// Convenience function to call [`Server::listen`] bound to a TCP address.
    ///
    /// This also sets `TCP_NODELAY` on the incoming connections for performance
    /// reasons, as a reasonable default. When TLS options are configured the
    /// listener wraps every accepted connection in a TLS session and the auth
    /// cookie is marked `Secure`.
    pub async fn bind(&self, addr: &SocketAddr) -> Result<()> {
        let raw = TcpListener::bind(addr).await?;
        match &self.tls {
            Some((cert, key)) => {
                let config = tls::load_server_config(cert, key)?;
                self.state.config().set_secure_cookies(true);
                let listener = tls::TlsListener::new(tls::NoDelayListener::new(raw), config);
                self.listen(listener).await
            }
            None => self.listen(tls::NoDelayListener::new(raw)).await,
        }
    }

    /// Send a graceful shutdown signal to the server.
    pub fn shutdown(&self) {
        // Stop receiving new network connections.
        self.shutdown.shutdown();
        // Terminate each of the existing sessions.
        self.state.shutdown();
    }
}
