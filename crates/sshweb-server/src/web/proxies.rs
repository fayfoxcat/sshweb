//! SOCKS5 隧道管理端点(开启 / 列表 / 停止)。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::{error_response, require_auth};
use crate::web::protocol::ServerConfig;
use crate::ServerState;

/// 开启隧道的请求体:完整服务器配置(与 `create`/`sftpConnect` 同构,含密码;
/// 服务端在此 `resolve_auth` 解析密钥)+ 本地端口偏好。
#[derive(Deserialize)]
pub(crate) struct StartProxyRequest {
    /// 完整服务器配置。
    server: ServerConfig,
    /// 本地监听端口偏好(0 = 自动分配,从 10801 起)。
    #[serde(default)]
    port: u16,
}

/// 开启某服务器的 SOCKS5 隧道。
pub(crate) async fn start_proxy(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<StartProxyRequest>,
) -> Response {
    if let Some(resp) = require_auth(&state, &headers) {
        return resp;
    }
    let mut server = body.server;
    // Key-mode 服务器在此解析私钥(与 WS 的 sftpConnect 一致);失败明确报错。
    if let Err(err) = state.config().resolve_auth(&mut server) {
        return error_response(StatusCode::BAD_REQUEST, err.to_string());
    }
    match state.proxies().start(server, body.port).await {
        Ok(status) => Json(status).into_response(),
        Err(err) => error_response(StatusCode::CONFLICT, err.to_string()),
    }
}

/// 列出所有运行中的隧道。
pub(crate) async fn list_proxies(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = require_auth(&state, &headers) {
        return resp;
    }
    Json(state.proxies().list().await).into_response()
}

/// 停止某服务器的隧道。路径参数为 URL 编码的 `user@host:port`。
pub(crate) async fn stop_proxy(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(server_key): Path<String>,
) -> Response {
    if let Some(resp) = require_auth(&state, &headers) {
        return resp;
    }
    if state.proxies().stop(&server_key).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        error_response(StatusCode::NOT_FOUND, "隧道不存在")
    }
}
