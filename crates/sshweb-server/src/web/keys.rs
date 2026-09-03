//! Saved SSH key management: list / generate / delete, plus one-click install
//! of a public key onto a target server over a bootstrap password connection,
//! and a connection test for unsaved server configurations.

use std::sync::Arc;

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::auth::cookie_header;
use super::{error_response, require_auth};
use crate::config::ConfigStore;
use crate::web::protocol::ServerConfig;
use crate::ServerState;

/// Upper bound on the bootstrap SSH connection and install command.
const INSTALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Upper bound on a connection test (transport + authentication only).
const TEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateKeyPayload {
    /// Optional user-facing name for the new key.
    #[serde(default)]
    name: String,
}

/// `PUT /api/keys/{id}` body: the new user-facing name for the key.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RenameKeyPayload {
    name: String,
}

/// Validate that a server config has a host and username, returning an error
/// response otherwise.
fn validate_host_user(server: &ServerConfig) -> Option<Response> {
    (server.host.is_empty() || server.username.is_empty())
        .then(|| error_response(StatusCode::BAD_REQUEST, "主机或用户名不能为空"))
}

/// Human-readable `user@host:port` label for a target server.
fn target_label(server: &ServerConfig) -> String {
    server.target_key()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallPayload {
    /// The target server (connection fields). The server connects with the
    /// config's OWN authentication (key-mode private key or password), not a
    /// forced one.
    server: ServerConfig,
    /// ID of the saved key whose public part is installed.
    key_id: String,
    /// Legacy bootstrap password, kept for old-frontend compatibility: only
    /// used when a password-mode server carries no password of its own.
    #[serde(default)]
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TestConnectionPayload {
    /// The (possibly unsaved) server configuration to test.
    server: ServerConfig,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallResult {
    message: String,
}

/// `GET /api/keys` — list saved keys (public parts only).
pub(crate) async fn list_keys(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Response {
    let config = state.config();
    match config.list_keys(cookie_header(&headers)) {
        Ok(keys) => Json(keys).into_response(),
        Err(_) => super::unauthorized(),
    }
}

/// `POST /api/keys` — generate a new Ed25519 keypair and persist it.
pub(crate) async fn create_key(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<CreateKeyPayload>,
) -> Response {
    let name = payload.name.trim().to_string();
    let name = if name.is_empty() {
        format!("sshweb-{}", sshweb_core::rand_alphanumeric(4))
    } else {
        name
    };
    let config = state.config();
    match config.create_key(cookie_header(&headers), name) {
        Ok(key) => (StatusCode::CREATED, Json(key)).into_response(),
        Err(_) => super::unauthorized(),
    }
}

/// `DELETE /api/keys/{id}` — delete a saved key.
pub(crate) async fn delete_key(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let config = state.config();
    match config.delete_key(cookie_header(&headers), &id) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error_response(StatusCode::NOT_FOUND, "密钥不存在"),
        Err(_) => super::unauthorized(),
    }
}

/// `PUT /api/keys/{id}` — rename a saved key.
pub(crate) async fn rename_key(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<RenameKeyPayload>,
) -> Response {
    let name = payload.name.trim().to_string();
    if name.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "密钥名称不能为空");
    }
    let config = state.config();
    match config.rename_key(cookie_header(&headers), &id, name) {
        Ok(key) => Json(key).into_response(),
        Err(err) if err.to_string().contains("密钥不存在") => {
            error_response(StatusCode::NOT_FOUND, "密钥不存在")
        }
        Err(_) => super::unauthorized(),
    }
}

/// `POST /api/test-connection` — try to establish an SSH transport connection
/// and authenticate to the (possibly unsaved) server configuration, so the
/// form's "测试" button can validate credentials before saving. No shell or
/// SFTP subsystem is opened; the connection is dropped right after auth.
pub(crate) async fn test_connection(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<TestConnectionPayload>,
) -> Response {
    if let Some(resp) = require_auth(&state, &headers) {
        return resp;
    }
    let mut server = payload.server;
    if let Some(resp) = validate_host_user(&server) {
        return resp;
    }
    // Key mode: resolve the saved key (rejects a missing/deleted key clearly).
    if let Err(err) = state.config().resolve_auth(&mut server) {
        return error_response(StatusCode::BAD_REQUEST, err.to_string());
    }
    if server.auth_method.as_deref() != Some("key") && server.password.is_empty() {
        return error_response(StatusCode::BAD_REQUEST, "请填写 SSH 密码");
    }
    match tokio::time::timeout(
        TEST_TIMEOUT,
        crate::ssh::connect(&server, Some(&state.config())),
    )
    .await
    {
        Ok(Ok(_)) => Json(InstallResult {
            message: format!("连接成功：{}", target_label(&server)),
        })
        .into_response(),
        Ok(Err(err)) => error_response(StatusCode::BAD_REQUEST, format!("连接失败：{err:#}")),
        Err(_) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            format!("连接超时：{}", target_label(&server)),
        ),
    }
}

/// `POST /api/keys/install` — install a saved key's public part onto the target
/// server's `~/.ssh/authorized_keys`.
///
/// The connection uses the **form's own authentication**: a key-mode server
/// connects with its selected saved private key, a password-mode server with
/// its password (the frontend merges the saved/typed password into the config
/// before sending). So any credential that can already reach the server works,
/// regardless of which key is being installed. The install is idempotent: the
/// key line is appended only if not already present.
pub(crate) async fn install_key(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<InstallPayload>,
) -> Response {
    if let Some(resp) = require_auth(&state, &headers) {
        return resp;
    }
    let config = state.config();
    let public_key = match config.key_public_key(&payload.key_id) {
        Some(key) => key,
        None => {
            tracing::warn!(key_id = %payload.key_id, "install: key not found");
            return error_response(StatusCode::BAD_REQUEST, "密钥不存在或已删除");
        }
    };
    if let Some(resp) = validate_host_user(&payload.server) {
        tracing::warn!(host = %payload.server.host, user = %payload.server.username, "install: empty host/user");
        return resp;
    }

    // Connect with the form's own authentication, not a forced password. The
    // `password` field is kept for old-frontend compatibility: it only fills
    // in when a password-mode server carries no password at all.
    let mut server = payload.server.clone();
    if server.auth_method.as_deref() != Some("key") && server.password.is_empty() {
        server.password = payload.password;
    }
    if let Err(err) = config.resolve_auth(&mut server) {
        tracing::warn!(%err, "install: resolve_auth failed");
        return error_response(StatusCode::BAD_REQUEST, err.to_string());
    }
    // There must be a usable credential (private key or password), otherwise
    // the bootstrap connection cannot authenticate.
    if server.private_key.is_none() && server.password.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "请输入 SSH 密码，或选择密钥认证方式以连接服务器",
        );
    }

    let target = target_label(&server);
    match tokio::time::timeout(
        INSTALL_TIMEOUT,
        install_authorized_key(&server, &public_key, state.config()),
    )
    .await
    {
        Ok(Ok(())) => Json(InstallResult {
            message: format!("公钥已安装到 {target}"),
        })
        .into_response(),
        Ok(Err(err)) => {
            tracing::warn!(target = %target, ?err, "install_authorized_key failed");
            let msg = err.to_string();
            // Auth rejection during the bootstrap connection means the one-time
            // password is wrong, or the server disallows password login for the
            // user (e.g. PermitRootLogin prohibit-password / PasswordAuthentication
            // no) — make that actionable instead of a bare ssh error.
            let hint = if msg.contains("authentication rejected")
                || msg.contains("authentication failed")
            {
                "；请检查一次性密码是否正确，或该服务器是否允许该用户密码登录（如 root \
                 被禁止密码登录）"
            } else {
                ""
            };
            error_response(
                StatusCode::BAD_REQUEST,
                format!("安装公钥失败（{target}）：{err:#}{hint}"),
            )
        }
        Err(_) => error_response(StatusCode::GATEWAY_TIMEOUT, "安装公钥超时，请稍后重试"),
    }
}

/// Connect to `server` over a bootstrap password connection and append
/// `public_key` to `~/.ssh/authorized_keys` idempotently.
async fn install_authorized_key(
    server: &ServerConfig,
    public_key: &str,
    config: Arc<ConfigStore>,
) -> Result<()> {
    let handle = crate::ssh::connect(server, Some(&config)).await?;
    let mut channel = handle.channel_open_session().await?;
    let quoted = crate::utils::shell_quote(public_key);
    let command = format!(
        "mkdir -p ~/.ssh && chmod 700 ~/.ssh && (grep -qF '{quoted}' ~/.ssh/authorized_keys \
         2>/dev/null || printf '%s\\n' '{quoted}' >> ~/.ssh/authorized_keys); chmod 600 \
         ~/.ssh/authorized_keys"
    );
    channel.exec(true, command).await?;

    // Read until the channel closes. `Eof` may precede `ExitStatus`, so keep
    // reading through it (the classifier reports `Eof` separately from
    // `Closed`; matching russh-extra's command loop).
    let mut status: Option<u32> = None;
    let mut output = Vec::new();
    loop {
        match crate::ssh::channel_event(channel.wait().await) {
            crate::ssh::ChannelEvent::Data(data) => output.extend_from_slice(&data),
            crate::ssh::ChannelEvent::Extended(data) => output.extend_from_slice(&data),
            crate::ssh::ChannelEvent::ExitStatus(code) => status = Some(code),
            // `Eof` falls through (`_`): keep reading for the exit status.
            crate::ssh::ChannelEvent::Closed => break,
            _ => {}
        }
    }
    if status == Some(0) {
        Ok(())
    } else {
        let snippet = String::from_utf8_lossy(&output)
            .trim()
            .chars()
            .take(300)
            .collect::<String>();
        anyhow::bail!(
            "远程命令退出码 {:?}{}",
            status,
            if snippet.is_empty() {
                String::new()
            } else {
                format!("：{snippet}")
            }
        )
    }
}
