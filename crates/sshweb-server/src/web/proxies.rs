//! SOCKS5 代理管理端点(开启 / 列表 / 停止)。

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use super::{error_response, require_auth};
use crate::web::protocol::{ServerConfig, Socks5Tunnel};
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

/// 开启「本机」直连代理的请求体。
///
/// **专用端点,不把上面的 `server` 改成可选**:可选字段会让「客户端漏传」
/// 从今天的 422 变成「起一个 no-auth 直连代理」——一个静默且与安全相关的失败。
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartLocalProxyRequest {
    /// 本地监听端口偏好(0 / 缺省 = 自动分配,从 10801 起)。
    #[serde(default)]
    pub port: u16,
    /// 入站 SOCKS5 认证用户名(空 = no-auth)。
    #[serde(default)]
    pub username: String,
    /// 入站 SOCKS5 认证密码(`username` 非空时生效)。
    #[serde(default)]
    pub password: String,
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

/// 开启「本机」直连 SOCKS5 代理(键固定为 `local`,见 `proxy::LOCAL_PROXY_KEY`)。
pub(crate) async fn start_local_proxy(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(body): Json<StartLocalProxyRequest>,
) -> Response {
    if let Some(resp) = require_auth(&state, &headers) {
        return resp;
    }
    let tunnel = Socks5Tunnel {
        port: body.port,
        username: body.username,
        password: body.password,
    };
    match state.proxies().start_direct(&tunnel).await {
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
    stop(&state, &server_key).await
}

/// 停止「本机」直连代理(`DELETE /api/proxies/local`)。
///
/// **必须单独注册**,不能指望 `/proxies/{server_key}` 兜住它:路由树里同一段上
/// 静态节点优先于参数节点,`/proxies/local` 只挂 POST 时,对它发 DELETE 得到的是
/// **405**(`allow: POST`)而**不会**回落到参数路由——前端把它读成「服务端返回了
/// 无效响应」,本机代理就关不掉了(已知坑 86)。前端两种代理共用
/// `stopProxy(key)`,拼出的路径天然就是这一条,所以前端零改动。
pub(crate) async fn stop_local_proxy(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = require_auth(&state, &headers) {
        return resp;
    }
    stop(&state, crate::proxy::LOCAL_PROXY_KEY).await
}

/// 两种代理共用的停止逻辑:命中 204,key 不在注册表里 404。
async fn stop(state: &Arc<ServerState>, server_key: &str) -> Response {
    if state.proxies().stop(server_key).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        error_response(StatusCode::NOT_FOUND, "隧道不存在")
    }
}
