//! Reusable SFTP connection pool with a cached browse probe.
//!
//! The SFTP transport is an [`openssh_sftp_client::Sftp`] built over a
//! **russh** SSH channel (`request_subsystem("sftp")`). Because the pool
//! authenticates through `crate::ssh::connect`, the terminal, SFTP and SOCKS5
//! paths all share the same proxy / jump / MAC / key rules, and the browse
//! probe (`pwd; id -un`) runs on the **same connection** as the SFTP
//! subsystem (已知坑 31).

use std::future::Future;
use std::sync::Arc;

use anyhow::{Context, Result};
use openssh_sftp_client::{Sftp, SftpOptions};
use tokio::sync::Mutex;

use crate::config::ConfigStore;
use crate::ssh::{self, SshHandler};
use crate::web::protocol::ServerConfig;

/// A reusable SFTP connection to a remote server, together with the cached
/// browsing probe (login directory + user) gathered on the same connection.
/// `config` (when present) enables SSH host-key verification on the underlying
/// connection (TOFU, 安全审查 H2).
#[derive(Clone)]
pub struct SftpPool {
    inner: Arc<Mutex<Option<(Arc<Sftp>, (String, String))>>>,
    config: Option<Arc<ConfigStore>>,
}

impl Default for SftpPool {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            config: None,
        }
    }
}

/// Upper bound on how long establishing an SFTP connection may take. Without
/// this, a stalled proxy / jump tunnel hangs forever (russh's connect has no
/// built-in timeout) and blocks every subsequent SFTP operation for the shell.
/// Shared with the SSH shell and SOCKS5 tunnel paths
/// (`ssh::SSH_CONNECT_TIMEOUT`).
const CONNECT_TIMEOUT: std::time::Duration = crate::ssh::SSH_CONNECT_TIMEOUT;

/// Error of a combined SFTP connect + probe, so the caller can tell an SSH
/// transport/authentication failure from an SFTP-permission failure.
pub enum SftpConnectError {
    /// The SSH connection could not be established (network, auth, tunnel…).
    Connect(anyhow::Error),
    /// SSH authentication worked but the SFTP subsystem is unavailable; the
    /// identified user is known (used for the "no SFTP permission" notice).
    NoSftp {
        /// The login user that authenticated successfully.
        user: String,
        /// The SFTP subsystem error.
        error: anyhow::Error,
    },
}

impl From<anyhow::Error> for SftpConnectError {
    fn from(err: anyhow::Error) -> Self {
        SftpConnectError::Connect(err)
    }
}

impl SftpPool {
    /// A pool that passes the config store (host-key TOFU) to every SSH
    /// connect.
    pub fn new(config: Arc<ConfigStore>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            config: Some(config),
        }
    }

    /// Get (or lazily establish) a connected SFTP client for the server.
    ///
    /// The connection is established **outside** the pool lock, so a slow or
    /// hanging connect never blocks other operations for the same shell. The
    /// lock is only used to (re-)store the ready client.
    pub(crate) async fn client(&self, server: &ServerConfig) -> Result<Arc<Sftp>> {
        self.client_probe(server)
            .await
            .map(|(client, _, _)| client)
            .map_err(|err| match err {
                SftpConnectError::Connect(err) | SftpConnectError::NoSftp { error: err, .. } => err,
            })
    }

    /// Get (or lazily establish) a connected SFTP client, also probing the
    /// login directory and user **over the same SSH connection** — previously
    /// this was a separate SSH handshake, so opening a server's file list
    /// paid two round-trips before the first listing appeared.
    ///
    /// The probe result is cached next to the client; a warm pool answers
    /// instantly with no reconnection.
    pub(crate) async fn client_probe(
        &self,
        server: &ServerConfig,
    ) -> Result<(Arc<Sftp>, String, String), SftpConnectError> {
        // Fast path: reuse an existing connection and its probe.
        {
            let guard = self.inner.lock().await;
            if let Some((client, (cwd, user))) = guard.as_ref() {
                return Ok((client.clone(), cwd.clone(), user.clone()));
            }
        }

        let (client, cwd, user) = tokio::time::timeout(
            CONNECT_TIMEOUT,
            open_sftp_probe(server, self.config.as_deref()),
        )
        .await
        .map_err(|_| anyhow::anyhow!("SFTP 连接超时（{CONNECT_TIMEOUT:?}）"))??;

        // Another task may have connected while we were waiting; prefer it.
        let mut guard = self.inner.lock().await;
        if let Some((existing, (cwd, user))) = guard.as_ref() {
            return Ok((existing.clone(), cwd.clone(), user.clone()));
        }
        *guard = Some((client.clone(), (cwd.clone(), user.clone())));
        Ok((client, cwd, user))
    }

    /// Drop any cached connection (e.g. on error). Dropping the [`Sftp`] also
    /// ends its read/flush tasks and closes the underlying SSH channel.
    pub(crate) async fn invalidate(&self) {
        // Take the client out of the pool before dropping it. Dropping while
        // holding the pool lock would make a slow runtime shutdown serialize
        // every later SFTP operation unnecessarily.
        self.inner.lock().await.take();
    }
}

/// Establish a fresh authenticated SFTP session for a server and, on the same
/// SSH connection, probe the login directory and user (`pwd; id -un`).
///
/// The SFTP identity is **always** the configured user — a terminal's
/// `su`/`sudo` state never crosses SSH connections and is only followed when
/// the startup snippet declares it explicitly. Probing here removes two
/// extra SSH handshakes from the old design (one probe connection + one
/// permission-check connection before the pool's first listing connection).
///
/// The russh `Handle` may be dropped once the two channels are open: each
/// channel holds its own `Arc<Session>`, so the connection stays alive as
/// long as the [`Sftp`] (which owns the split channel stream) does.
pub async fn open_sftp_probe(
    server: &ServerConfig,
    host_keys: Option<&ConfigStore>,
) -> Result<(Arc<Sftp>, String, String), SftpConnectError> {
    let handle = ssh::connect(server, host_keys)
        .await
        .map_err(SftpConnectError::Connect)?;

    // One connection, two channels: probe (`pwd; id -un`) and the SFTP
    // subsystem run **in parallel**, so the first file-manager open pays a
    // single round-trip for both instead of two serial ones (matters on
    // high-latency remote hosts). `flush_interval(0)` makes every SFTP
    // request leave immediately instead of batching for 0.5 ms, cutting the
    // per-operation latency further.
    let (probe_res, sftp_res) = tokio::join!(probe_dir_user(&handle), async {
        let channel = handle.channel_open_session().await.map_err(|e| {
            SftpConnectError::Connect(anyhow::anyhow!("open SFTP channel failed: {e}"))
        })?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| SftpConnectError::NoSftp {
                user: String::new(),
                error: anyhow::anyhow!("SFTP subsystem failed: {e}"),
            })?;
        let stream = channel.into_stream();
        let (read_half, write_half) = tokio::io::split(stream);
        Sftp::new(
            write_half,
            read_half,
            SftpOptions::default().flush_interval(std::time::Duration::ZERO),
        )
        .await
        .map_err(|e| SftpConnectError::NoSftp {
            user: String::new(),
            error: anyhow::anyhow!("SFTP handshake failed: {e}"),
        })
    });

    let (cwd, user) = probe_res.map_err(SftpConnectError::Connect)?;
    let sftp = match sftp_res {
        Ok(sftp) => sftp,
        // Fill in the probed user for the "no SFTP permission" notice (the
        // parallel branch doesn't know it yet).
        Err(SftpConnectError::NoSftp { user: u0, error }) if u0.is_empty() => {
            return Err(SftpConnectError::NoSftp { user, error })
        }
        Err(err) => return Err(err),
    };

    Ok((Arc::new(sftp), cwd, user))
}

/// Run `pwd; printf '\nUSER=%s\n' "$(id -un)"` over an exec channel and parse
/// the login directory and user.
async fn probe_dir_user(
    handle: &russh::client::Handle<SshHandler>,
) -> Result<(String, String), anyhow::Error> {
    use crate::ssh::ChannelEvent;

    let mut channel = handle.channel_open_session().await?;
    channel
        .exec(true, "pwd; printf '\\nUSER=%s\\n' \"$(id -un)\"")
        .await?;
    let mut output = Vec::new();
    loop {
        match crate::ssh::channel_event(channel.wait().await) {
            ChannelEvent::Data(data) => output.extend_from_slice(&data),
            ChannelEvent::Closed => break,
            _ => {}
        }
    }
    parse_probe_output(&String::from_utf8_lossy(&output))
}

/// Parse the output of the `pwd; printf '\nUSER=%s\n' "$(id -un)"` probe.
fn parse_probe_output(text: &str) -> Result<(String, String), anyhow::Error> {
    let mut cwd = String::new();
    let mut user = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("USER=") {
            user = rest.trim().to_string();
        } else if !line.trim().is_empty() && !line.starts_with('$') {
            cwd = line.trim().to_string();
        }
    }
    if cwd.is_empty() {
        return Err(anyhow::anyhow!("SSH 探测未能读取工作目录"));
    }
    Ok((cwd, user))
}

/// Run a remote SFTP operation against the pool's client, invalidating the
/// connection on any error so the next operation reconnects fresh (see
/// 已知坑 20/25). Errors get the `label` context.
pub(crate) async fn with_remote<T, E, Fut>(
    pool: &SftpPool,
    server: &ServerConfig,
    label: &'static str,
    f: impl FnOnce(Arc<Sftp>) -> Fut,
) -> Result<T>
where
    E: Into<anyhow::Error>,
    Fut: Future<Output = std::result::Result<T, E>>,
{
    let client = pool.client(server).await?;
    match f(client.clone()).await {
        Ok(value) => Ok(value),
        Err(err) => {
            pool.invalidate().await;
            Err(err.into()).context(label)
        }
    }
}
