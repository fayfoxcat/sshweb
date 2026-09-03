//! SSH connection establishment with support for proxy, jump hosts, and
//! preferred MAC algorithms.

use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use bytes::Bytes;
use parking_lot::Mutex;
use russh::client;
use russh::ChannelMsg;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::config::ConfigStore;
use crate::web::protocol::{JumpHost, ProxyConfig, ServerConfig};

/// Upper bound on establishing an SSH connection (transport + auth), shared by
/// the terminal shell path, the SFTP pool and the SOCKS5 tunnel so a stalled
/// proxy / jump tunnel fails fast instead of hanging forever.
pub const SSH_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// Bidirectional byte stream trait (proxy/tunnel/direct connection).
pub(crate) trait ByteStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> ByteStream for T {}

/// SSH client handler with optional host-key verification (TOFU, 安全审查 H2).
///
/// When `expected` is `Some` (a fingerprint recorded from a previous
/// connection to this target) the presented key must match it; when `None`
/// (first connect) any key is accepted and the actual key is captured into
/// `seen` so the caller can record it after a successful authentication.
#[derive(Clone)]
pub struct SshHandler {
    expected: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
}

impl SshHandler {
    fn new(expected: Option<String>) -> (Self, Arc<Mutex<Option<String>>>) {
        let seen = Arc::new(Mutex::new(None));
        (
            Self {
                expected,
                seen: Arc::clone(&seen),
            },
            seen,
        )
    }

    fn fingerprint(key: &ssh_key::PublicKey) -> String {
        key.fingerprint(ssh_key::HashAlg::Sha256).to_string()
    }
}

impl client::Handler for SshHandler {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let fp = Self::fingerprint(key);
        *self.seen.lock() = Some(fp.clone());
        Ok(match &self.expected {
            Some(expected) => expected == &fp,
            None => true, // first connect: accept, caller records after auth
        })
    }
}

/// Log that a requested MAC algorithm is not supported by this build.
fn warn_unsupported_mac(name: &str) {
    tracing::warn!(mac = %name, "MAC algorithm not supported by this build");
}

/// Map a user-facing MAC alias to russh's `mac::Name`, or `None` if
/// unsupported. The terminal (plain russh) and the SFTP pool
/// (openssh-sftp-client over a russh channel) both authenticate through
/// `crate::ssh::connect`, so this is the single decision point for which MAC
/// algorithms are negotiated.
fn mac_alias(alias: &str) -> Option<russh::mac::Name> {
    use russh::mac;
    match alias {
        "hmac-sha2-512-etm" => Some(mac::HMAC_SHA512_ETM),
        "hmac-sha2-256-etm" => Some(mac::HMAC_SHA256_ETM),
        "hmac-sha2-512" => Some(mac::HMAC_SHA512),
        "hmac-sha2-256" => Some(mac::HMAC_SHA256),
        "hmac-sha1" => Some(mac::HMAC_SHA1),
        _ => None,
    }
}

/// Iterate the server's requested MAC algorithms, keeping only the ones this
/// build supports (warning about the skipped ones), as russh `mac::Name`s.
fn supported_macs(server: &ServerConfig) -> impl Iterator<Item = russh::mac::Name> + '_ {
    server.macs.iter().filter_map(|name| match mac_alias(name) {
        Some(n) => Some(n),
        None => {
            warn_unsupported_mac(name);
            None
        }
    })
}

/// Build the russh client config, applying preferred MAC algorithms.
fn make_config(server: &ServerConfig) -> client::Config {
    let mut config = client::Config::default();
    let names: Vec<russh::mac::Name> = supported_macs(server).collect();
    if !names.is_empty() {
        tracing::debug!(macs = ?names.iter().map(|n| n.as_ref()).collect::<Vec<_>>(), "setting preferred MACs");
        config.preferred.mac = std::borrow::Cow::Owned(names);
    }
    config
}

/// Whether the target needs a custom transport (HTTP/SOCKS5 proxy or SSH jump
/// hosts) rather than a plain TCP connection. Used by both the terminal
/// (`connect`) and the SFTP pool (`connect_session`) so the two paths never
/// drift on this decision.
pub fn uses_custom_transport(server: &ServerConfig) -> bool {
    server.proxy.is_some() || !server.hosts.is_empty()
}

/// When `host_keys` is provided the target's SSH host key is verified against
/// the stored fingerprint (TOFU, 安全审查 H2): the first successful connection
/// records the key, later connections reject a mismatched key.
pub async fn connect(
    server: &ServerConfig,
    host_keys: Option<&ConfigStore>,
) -> Result<client::Handle<SshHandler>> {
    let config = Arc::new(make_config(server));
    let target = server.target_key();
    let expected = host_keys.and_then(|c| c.host_key_for(&target));
    let (handler, seen) = SshHandler::new(expected.clone());
    tracing::debug!(
        proxy = server.proxy.is_some(),
        jumps = server.hosts.len(),
        "establishing ssh connection"
    );

    let mut handle = if !uses_custom_transport(server) {
        // Plain direct connection: use russh's built-in TCP connect.
        client::connect(config, (server.host.as_str(), server.port), handler)
            .await
            .map_err(|e| anyhow!("SSH connect failed: {e}"))?
    } else {
        // Proxy / jump: handshake over a custom stream.
        let stream = connect_stream(server).await?;
        client::connect_stream(config, stream, handler)
            .await
            .map_err(|e| anyhow!("SSH connect failed: {e}"))?
    };

    // Authenticate to the target host with the shared credential decision
    // (key-mode when `resolve_auth` populated the private key, else password).
    authenticate(
        &mut handle,
        &server.username,
        &AuthCredential::resolve(server.private_key.as_deref(), &server.password),
        "SSH",
    )
    .await?;

    // Record the host key on the first successful connection (TOFU).
    if expected.is_none() {
        if let Some(fp) = seen.lock().clone() {
            if let Some(c) = host_keys {
                c.record_host_key(&target, fp);
            }
        }
    }

    Ok(handle)
}

/// The credential used to authenticate an SSH connection: a resolved private
/// key (key-mode) or the configured password.
///
/// [`Self::resolve`] is the **single decision point** between key and password
/// auth, shared by the terminal shell path (`ssh.rs`) and the SFTP pool
/// (`sftp/pool.rs`). `resolve_auth` (config.rs) guarantees a key-mode server
/// has its private key populated by the time either path runs, so this choice
/// can never silently fall back to a password (已知坑 36).
pub enum AuthCredential {
    /// OpenSSH PEM private key (public-key authentication).
    Key(String),
    /// Password authentication.
    Password(String),
}

impl AuthCredential {
    /// Choose key-mode when a resolved private key is present, else password.
    /// Do not re-implement this `match` in the connection paths.
    pub fn resolve(private_key: Option<&str>, password: &str) -> Self {
        match private_key {
            Some(pem) => AuthCredential::Key(pem.to_string()),
            None => AuthCredential::Password(password.to_string()),
        }
    }
}

/// Authenticate a connected SSH handle with the resolved credential.
/// `context` names the hop in error messages ("SSH" for the target host,
/// "jump" for jump hosts).
async fn authenticate(
    handle: &mut client::Handle<SshHandler>,
    username: &str,
    credential: &AuthCredential,
    context: &str,
) -> Result<()> {
    match credential {
        AuthCredential::Key(pem) => {
            let key = ssh_key::PrivateKey::from_openssh(pem.as_bytes())
                .map_err(|e| anyhow!("invalid saved SSH private key: {e}"))?;
            let hash = handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten();
            let auth = handle
                .authenticate_publickey(
                    username,
                    russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|e| anyhow!("{context} authentication failed: {e}"))?;
            if !auth.success() {
                bail!("{context} key authentication rejected for user {username}");
            }
        }
        AuthCredential::Password(password) => {
            let auth = handle
                .authenticate_password(username, password)
                .await
                .map_err(|e| anyhow!("{context} authentication failed: {e}"))?;
            if !auth.success() {
                bail!("{context} authentication rejected for user {username}");
            }
        }
    }
    Ok(())
}

/// Open a byte stream to `host:port` honouring proxy / jump settings.
pub(crate) async fn connect_stream(server: &ServerConfig) -> Result<Box<dyn ByteStream>> {
    let (host, port) = (server.host.clone(), server.port);

    // Proxy first (applies to the outermost hop to the target).
    if let Some(proxy) = &server.proxy {
        let stream = connect_via_proxy(proxy, &host, port).await?;
        return Ok(stream);
    }

    // Jump hosts: connect through each in order.
    if !server.hosts.is_empty() {
        return connect_via_jumps(server, &server.hosts).await;
    }

    // Direct connection.
    let stream = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .with_context(|| format!("TCP connect to {host}:{port} failed"))?;
    Ok(Box::new(stream))
}

/// Connect through a chain of SSH jump hosts to the target.
async fn connect_via_jumps(
    server: &ServerConfig,
    hosts: &[JumpHost],
) -> Result<Box<dyn ByteStream>> {
    let mut iter = hosts.iter();
    let first = iter
        .next()
        .ok_or_else(|| anyhow!("empty jump host chain"))?;

    let mut handle = ssh_connect_host(first, None).await?;
    let mut stream: Box<dyn ByteStream> =
        Box::new(open_target(&mut handle, &server.host, server.port).await?);

    // Remaining jump hosts, if any.
    for host in iter {
        handle = ssh_connect_host(host, Some(stream)).await?;
        stream = Box::new(open_target(&mut handle, &server.host, server.port).await?);
    }

    Ok(stream)
}

async fn ssh_connect_host(
    host: &JumpHost,
    transport: Option<Box<dyn ByteStream>>,
) -> Result<client::Handle<SshHandler>> {
    let config = Arc::new(client::Config::default());
    let (handler, _seen) = SshHandler::new(None);
    let mut handle = match transport {
        Some(stream) => client::connect_stream(config, stream, handler)
            .await
            .map_err(|e| anyhow!("jump SSH connect failed: {e}"))?,
        None => client::connect(config, (host.host.as_str(), host.port), handler)
            .await
            .map_err(|e| anyhow!("jump TCP connect to {} failed: {e}", host.host))?,
    };
    authenticate(
        &mut handle,
        &host.username,
        &AuthCredential::resolve(host.private_key.as_deref(), &host.password),
        "jump",
    )
    .await?;
    Ok(handle)
}

/// Open a direct-tcpip channel to the target through an SSH handle. Shared by
/// the jump-host chain (`connect_via_jumps`) and the SOCKS5 tunnel proxy
/// (`proxy.rs`) — both forward a local byte stream over the SSH connection.
pub(crate) async fn open_target(
    handle: &client::Handle<SshHandler>,
    host: &str,
    port: u16,
) -> Result<impl AsyncRead + AsyncWrite + Send + Unpin> {
    let channel = handle
        .channel_open_direct_tcpip(host, u32::from(port), "localhost", 0)
        .await
        .context("failed to open direct-tcpip channel")?;
    Ok(channel.into_stream())
}

/// Connect to `host:port` through an HTTP or SOCKS5 proxy.
async fn connect_via_proxy(
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> Result<Box<dyn ByteStream>> {
    let stream = tokio::net::TcpStream::connect((proxy.host.as_str(), proxy.port))
        .await
        .with_context(|| format!("proxy connect to {}:{} failed", proxy.host, proxy.port))?;
    match proxy.kind.as_str() {
        "http" => http_connect(stream, proxy, host, port).await,
        "socks5" => socks5_connect(stream, proxy, host, port).await,
        other => bail!("unsupported proxy kind: {other}"),
    }
}

/// HTTP CONNECT tunneling through a proxy.
async fn http_connect(
    mut stream: tokio::net::TcpStream,
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> Result<Box<dyn ByteStream>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let target = format!("{host}:{port}");
    let mut request = format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n");
    if !proxy.username.is_empty() {
        use base64::prelude::{Engine as _, BASE64_STANDARD};
        let token = BASE64_STANDARD.encode(format!("{}:{}", proxy.username, proxy.password));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");

    stream.write_all(request.as_bytes()).await?;
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let response = String::from_utf8_lossy(&buf[..n]);
    let mut lines = response.lines();
    let status = lines.next().unwrap_or("");
    if !status.contains(" 200 ") {
        bail!("HTTP proxy CONNECT failed: {status}");
    }
    Ok(Box::new(stream))
}

/// SOCKS5 handshake through a proxy.
async fn socks5_connect(
    mut stream: tokio::net::TcpStream,
    proxy: &ProxyConfig,
    host: &str,
    port: u16,
) -> Result<Box<dyn ByteStream>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // Greeting: version 5, number of methods, method 0 (no auth) / 2 (user/pass).
    let has_auth = !proxy.username.is_empty();
    stream
        .write_all(&[
            0x05,
            if has_auth { 2 } else { 1 },
            if has_auth { 0x02 } else { 0x00 },
        ])
        .await?;
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).await?;
    if resp[0] != 0x05 {
        bail!("SOCKS5 unsupported version");
    }
    if has_auth {
        if resp[1] == 0x02 {
            let user = proxy.username.as_bytes();
            let pass = proxy.password.as_bytes();
            let mut auth = vec![0x01, user.len() as u8];
            auth.extend_from_slice(user);
            auth.push(pass.len() as u8);
            auth.extend_from_slice(pass);
            stream.write_all(&auth).await?;
            let mut auth_resp = [0u8; 2];
            stream.read_exact(&mut auth_resp).await?;
            if auth_resp[1] != 0x00 {
                bail!("SOCKS5 authentication failed");
            }
        } else {
            bail!("SOCKS5 proxy requires no-auth only");
        }
    } else if resp[1] != 0x00 {
        bail!("SOCKS5 no acceptable auth method");
    }

    // Connect request: version 5, CONNECT (1), reserved (0), ATYP + address + port.
    let mut req = vec![0x05, 0x01, 0x00];
    if host.contains(':') {
        // IPv6
        let ip: std::net::Ipv6Addr = host.parse().map_err(|_| anyhow!("invalid IPv6 host"))?;
        req.push(0x04);
        req.extend_from_slice(&ip.octets());
    } else if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        req.push(0x01);
        req.extend_from_slice(&ip.octets());
    } else {
        req.push(0x03);
        req.push(host.len() as u8);
        req.extend_from_slice(host.as_bytes());
    }
    req.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&req).await?;

    // Read the reply (variable length).
    let mut reply = [0u8; 4];
    stream.read_exact(&mut reply).await?;
    if reply[1] != 0x00 {
        bail!("SOCKS5 connect failed, status {}", reply[1]);
    }
    // Skip address + port (varies by ATYP).
    let atyp = reply[3];
    let addr_len = match atyp {
        0x01 => 4,
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            len[0] as usize
        }
        0x04 => 16,
        _ => bail!("SOCKS5 unknown address type"),
    };
    let mut skip = vec![0u8; addr_len + 2];
    stream.read_exact(&mut skip).await?;

    Ok(Box::new(stream))
}

// ---- Channel read helper --------------------------------------------------

/// Classified outcome of one message read from a russh channel, so the
/// terminal / stats / key-install read loops don't each re-match the raw
/// [`ChannelMsg`] surface. `Eof` is kept separate from `Closed` because the
/// key-install loop (`web/keys.rs`) keeps reading after `Eof` to collect a
/// trailing `ExitStatus`.
pub(crate) enum ChannelEvent {
    /// Terminal data (stdout for PTY channels).
    Data(Bytes),
    /// Extended data (stderr).
    Extended(Bytes),
    /// The remote process's exit status.
    ExitStatus(u32),
    /// `Eof` from the peer: the channel is half-closed.
    Eof,
    /// `Close` or the stream ended: stop reading.
    Closed,
    /// Any other message (window adjust, xon/xoff, …): ignore and keep going.
    Ignore,
}

/// Classify one raw [`ChannelMsg`] from `Channel::wait()`.
pub(crate) fn channel_event(msg: Option<ChannelMsg>) -> ChannelEvent {
    match msg {
        Some(ChannelMsg::Data { data }) => ChannelEvent::Data(Bytes::copy_from_slice(&data)),
        Some(ChannelMsg::ExtendedData { data, .. }) => {
            ChannelEvent::Extended(Bytes::copy_from_slice(&data))
        }
        Some(ChannelMsg::ExitStatus { exit_status }) => ChannelEvent::ExitStatus(exit_status),
        Some(ChannelMsg::Eof) => ChannelEvent::Eof,
        Some(ChannelMsg::Close) | None => ChannelEvent::Closed,
        _ => ChannelEvent::Ignore,
    }
}
