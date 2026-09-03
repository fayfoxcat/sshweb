use std::{future::Future, sync::Arc};

use anyhow::Result;
use axum::extract::connect_info::Connected;
use axum::http::header;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::serve::{IncomingStream, Listener};
use tower_http::trace::TraceLayer;

use crate::tls::PeerInfo;
use crate::{web, ServerState};

/// Bind and listen from the application, with a state and termination signal.
///
/// The listener must expose `SocketAddr` (plain `TapIo<TcpListener, _>` or the
/// TLS-wrapped `TlsListener`) so `ConnectInfo<SocketAddr>` — the client peer
/// address used by the auth rate limiter — is available to handlers.
pub(crate) async fn start_server<L>(
    state: Arc<ServerState>,
    listener: L,
    signal: impl Future<Output = ()> + Send + 'static,
) -> Result<()>
where
    L: Listener<Addr = std::net::SocketAddr> + Send + 'static,
    for<'a> PeerInfo: Connected<IncomingStream<'a, L>>,
{
    let secure = state.config().secure_cookies();
    let svc = web::app()
        .with_state(state.clone())
        .layer(middleware::from_fn(move |req, next| {
            hsts(req, next, secure)
        }))
        .layer(TraceLayer::new_for_http());

    axum::serve(
        listener,
        svc.into_make_service_with_connect_info::<PeerInfo>(),
    )
    .with_graceful_shutdown(signal)
    .await?;

    Ok(())
}

/// Add `Strict-Transport-Security` when serving over HTTPS (安全审查 M2). On
/// plain HTTP the header is omitted (browsers ignore it over HTTP anyway).
async fn hsts(req: Request<axum::body::Body>, next: Next, secure: bool) -> Response {
    let mut res = next.run(req).await;
    if secure {
        res.headers_mut().insert(
            header::STRICT_TRANSPORT_SECURITY,
            header::HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    res
}
