//! HTTP and WebSocket handlers for the sshweb web interface.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::uri::Uri;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::session::Session;
use crate::ServerState;

mod auth;
mod download;
mod embed;
mod keys;
pub mod protocol;
mod proxies;
pub(crate) mod ratelimit;
mod socket;

/// Query parameters for `/api/stats`: an optional target shell. When both
/// are supplied the payload describes that shell's server (remote SSH host or
/// the local machine); without them the local machine is reported.
#[derive(Deserialize)]
struct StatsQuery {
    session: Option<String>,
    sid: Option<u32>,
}

#[derive(Serialize)]
struct ErrorPayload {
    error: String,
}

/// Returns the web application server, routed with Axum.
///
/// The frontend is **embedded in the binary** (`embed::Assets`, the whole
/// `build/` directory) and served from memory — no `build/` directory needed
/// at runtime. Unknown non-API routes fall back to the SPA shell.
pub fn app() -> Router<Arc<ServerState>> {
    Router::new()
        .nest("/api", backend())
        .fallback(embed_handler)
        // The SPA entry HTML must never be cached: a stale index.html would
        // reference old hashed chunks, so users keep getting the previous
        // frontend after a rebuild. The hashed JS/CSS assets are immutable and
        // can be cached long-term; only the HTML is revalidated.
        .layer(axum::middleware::from_fn(no_cache_html))
}

/// Serve a non-API request from the embedded frontend assets.
async fn embed_handler(uri: Uri, headers: HeaderMap) -> Response {
    embed::serve(uri.path(), &headers)
}

/// Add `Cache-Control: no-cache` to HTML responses (the SPA shells and any
/// unhashed HTML), so the browser revalidates them and picks up new hashed
/// asset references after a frontend rebuild.
async fn no_cache_html(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut res = next.run(req).await;
    let is_html = res
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/html"))
        .unwrap_or(false);
    if is_html {
        res.headers_mut().insert(
            axum::http::header::CACHE_CONTROL,
            axum::http::HeaderValue::from_static("no-cache"),
        );
    }
    res
}

/// Routes for the backend web API server.
fn backend() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/auth/status", get(auth::get_auth_status))
        .route("/auth/login", axum::routing::post(auth::post_auth_login))
        .route(
            "/auth/password",
            axum::routing::post(auth::post_auth_change_password),
        )
        .route("/auth/logout", axum::routing::post(auth::post_auth_logout))
        .route("/config", get(auth::get_config).put(auth::put_config))
        .route("/config/import", axum::routing::post(auth::import_config))
        .route("/keys", get(keys::list_keys).post(keys::create_key))
        .route("/keys/install", axum::routing::post(keys::install_key))
        .route(
            "/keys/{id}",
            axum::routing::delete(keys::delete_key).put(keys::rename_key),
        )
        .route(
            "/test-connection",
            axum::routing::post(keys::test_connection),
        )
        .route(
            "/proxies",
            get(proxies::list_proxies).post(proxies::start_proxy),
        )
        .route(
            "/proxies/{server_key}",
            axum::routing::delete(proxies::stop_proxy),
        )
        .route("/s/{name}", any(socket::get_session_ws))
        .route(
            "/s/{name}/sftp/{sid}/download",
            get(download::get_sftp_download),
        )
        .route(
            "/s/{name}/sftp/{sid}/archive",
            get(download::get_sftp_archive),
        )
        .route("/stats", get(get_stats))
}

fn error_response(status: StatusCode, error: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorPayload {
            error: error.into(),
        }),
    )
        .into_response()
}

fn unauthorized() -> Response {
    error_response(StatusCode::UNAUTHORIZED, "需要登录")
}

/// authentication cookie. A session that logged in with the setup key and is
/// still pending a forced password change is also rejected here, so pending
/// sessions can only reach the status / change-password / logout endpoints.
pub(crate) fn require_auth(state: &ServerState, headers: &HeaderMap) -> Option<Response> {
    require_auth_inner(state, headers, true)
}

/// Like [`require_auth`], but allows sessions that are still pending a forced
/// password change (used by the change-password and logout endpoints).
pub(crate) fn require_auth_any(state: &ServerState, headers: &HeaderMap) -> Option<Response> {
    require_auth_inner(state, headers, false)
}

fn require_auth_inner(
    state: &ServerState,
    headers: &HeaderMap,
    reject_pending: bool,
) -> Option<Response> {
    let config = state.config();
    let cookie = auth::cookie_header(headers);
    if !config.is_authenticated(cookie) {
        return Some(unauthorized());
    }
    if reject_pending && config.is_pending_change_session(cookie) {
        return Some(error_response(
            StatusCode::FORBIDDEN,
            "首次登录需先设置访问密码",
        ));
    }
    None
}

/// Look up an active session by name, mapping an unknown name to the standard
/// 404 response.
fn find_session(state: &ServerState, name: &str) -> Result<Arc<Session>, Response> {
    state
        .get(name)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not found").into_response())
}

/// Require authentication and look up a session by name, returning the error
/// response to send when either step fails.
pub(crate) fn auth_session(
    state: &ServerState,
    headers: &HeaderMap,
    name: &str,
) -> Result<Arc<Session>, Response> {
    if let Some(resp) = require_auth(state, headers) {
        return Err(resp);
    }
    find_session(state, name)
}

/// Handler for the system statistics endpoint.
///
/// With `?session=<name>&sid=<n>` the values describe the server backing that
/// terminal: the remote SSH host for remote shells (sampled over the existing
/// connection), or the machine running sshweb-server for local shells. Without
/// the parameters the local machine is reported.
async fn get_stats(
    State(state): State<Arc<ServerState>>,
    Query(query): Query<StatsQuery>,
    headers: HeaderMap,
) -> Response {
    if let Some(resp) = require_auth(&state, &headers) {
        return resp;
    }
    if let (Some(name), Some(id)) = (query.session.as_deref(), query.sid) {
        if let Some(session) = state.get(name) {
            match session.shell_stats(sshweb_core::Sid(id)) {
                crate::session::ShellStats::Remote(stats) => return Json(stats).into_response(),
                crate::session::ShellStats::Machine => {
                    // Local shell: report this machine below.
                }
                crate::session::ShellStats::Unknown => {
                    return error_response(StatusCode::NOT_FOUND, "终端不存在");
                }
            }
        }
    }
    let stats = state.stats();
    let (down, up) = stats.net_rates();
    Json(crate::stats::HostStats {
        cpu: stats.cpu_usage(),
        memory: stats.memory_usage(),
        up,
        down,
        time: stats.now(),
    })
    .into_response()
}
