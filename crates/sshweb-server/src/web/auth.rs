//! Authentication and encrypted-config handlers.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use super::{error_response, require_auth_any, unauthorized};
use crate::tls::PeerInfo;
use crate::ServerState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AuthStatus {
    setup: bool,
    authenticated: bool,
    /// The session logged in with the one-time setup key and must set an
    /// access password before any other use.
    pending_change: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PasswordPayload {
    password: String,
    #[serde(default)]
    confirmation: String,
    /// Current password, only used by the change-password endpoint.
    #[serde(default)]
    old_password: String,
}

pub(crate) fn cookie_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
}

/// Extract a single cookie value by name from a `Cookie` header.
fn cookie_value<'a>(header: Option<&'a str>, name: &str) -> Option<&'a str> {
    header?.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Build the auth cookie. `Secure` is only set when serving over HTTPS, so the
/// cookie keeps working on plain HTTP while still being protected from
/// sniffing on a TLS deployment.
fn auth_cookie(token: &str, secure: bool) -> String {
    format!(
        "{}={token}; Path=/; HttpOnly; SameSite=Lax; {}Max-Age=2592000",
        crate::config::cookie_name(),
        if secure { "Secure; " } else { "" },
    )
}

fn cleared_auth_cookie() -> String {
    format!(
        "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        crate::config::cookie_name()
    )
}

pub(crate) async fn get_auth_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Json<AuthStatus> {
    let config = state.config();
    let token =
        cookie_header(&headers).and_then(|h| cookie_value(Some(h), crate::config::cookie_name()));
    Json(AuthStatus {
        setup: config.is_setup(),
        authenticated: config.is_authenticated(cookie_header(&headers)),
        pending_change: token.map(|t| config.is_pending_change(t)).unwrap_or(false),
    })
}

/// First-boot / normal login. When the store is not set up yet the only
/// accepted credential is the one-time setup key printed at startup; the
/// resulting session is forced to set an access password before any other use.
///
/// Per-source-IP failures are rate limited (安全审查 M1).
pub(crate) async fn post_auth_login(
    ConnectInfo(addr): ConnectInfo<PeerInfo>,
    State(state): State<Arc<ServerState>>,
    Json(payload): Json<PasswordPayload>,
) -> Response {
    let ip = addr.0.ip();
    if state.auth_limiter().is_blocked(ip) {
        return error_response(StatusCode::TOO_MANY_REQUESTS, "尝试次数过多，请稍后再试");
    }
    let config = state.config();
    match config.login(&payload.password) {
        Ok(token) => {
            state.auth_limiter().clear(ip);
            Response::builder()
                .status(StatusCode::NO_CONTENT)
                .header(
                    header::SET_COOKIE,
                    auth_cookie(&token, config.secure_cookies()),
                )
                .body(Body::empty())
                .expect("valid auth response")
        }
        Err(err) => {
            state.auth_limiter().record_failure(ip);
            error_response(StatusCode::UNAUTHORIZED, err.to_string())
        }
    }
}

/// Change the page access password. A setup-key session (pending change) is
/// allowed here to set its first password; a normal session must already be
/// authenticated and supply the current password.
pub(crate) async fn post_auth_change_password(
    ConnectInfo(addr): ConnectInfo<PeerInfo>,
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(payload): Json<PasswordPayload>,
) -> Response {
    let ip = addr.0.ip();
    if state.auth_limiter().is_blocked(ip) {
        return error_response(StatusCode::TOO_MANY_REQUESTS, "尝试次数过多，请稍后再试");
    }
    let config = state.config();
    if let Some(resp) = require_auth_any(&state, &headers) {
        return resp;
    }
    match config.change_password(
        cookie_header(&headers),
        &payload.old_password,
        &payload.password,
        &payload.confirmation,
    ) {
        Ok(()) => {
            state.auth_limiter().clear(ip);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            state.auth_limiter().record_failure(ip);
            error_response(StatusCode::BAD_REQUEST, err.to_string())
        }
    }
}

pub(crate) async fn post_auth_logout(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Response {
    state.config().logout(cookie_header(&headers));
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::SET_COOKIE, cleared_auth_cookie())
        .body(Body::empty())
        .expect("valid logout response")
}

pub(crate) async fn get_config(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Response {
    match state.config().settings(cookie_header(&headers)) {
        Ok(settings) => Json(settings).into_response(),
        Err(_) => unauthorized(),
    }
}

pub(crate) async fn put_config(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(settings): Json<crate::config::ServerSettings>,
) -> Response {
    match state
        .config()
        .save_settings(cookie_header(&headers), settings)
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => unauthorized(),
    }
}

pub(crate) async fn import_config(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(settings): Json<crate::config::ServerSettings>,
) -> Response {
    match state
        .config()
        .import_settings(cookie_header(&headers), settings)
    {
        Ok(settings) => Json(settings).into_response(),
        Err(_) => unauthorized(),
    }
}
