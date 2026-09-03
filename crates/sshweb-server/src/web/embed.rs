//! Embedded frontend assets (the SvelteKit `build/` directory).
//!
//! The whole SPA is compiled into the `sshweb` binary at build time and served
//! from memory — no `build/` directory is needed at runtime. Precompressed
//! `.gz` / `.br` variants (produced by the adapter's gzip/brotli prerender)
//! are served when the client advertises the matching encoding.

use axum::body::Body;
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// The embedded `build/` directory (frontend static output). Path is relative
/// to this crate's manifest dir (`crates/sshweb-server`), so `../../build` is
/// the repo-root frontend build output.
#[derive(RustEmbed)]
#[folder = "../../build/"]
struct Assets;

/// SPA fallback shell: unknown routes (no leading dot / not a hashed asset)
/// serve `index.html` so client-side routing works.
const SPA_SHELL: &str = "index.html";

/// The MIME type of a path (subset that covers the build output).
fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "map" => "application/json",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Whether a path looks like a real file request (has a file extension)
/// rather than a client-side route. Such paths get a 404 when missing instead
/// of the SPA shell, so a stale hashed chunk never renders as HTML.
fn looks_like_file(path: &str) -> bool {
    match path.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.contains('.'),
        _ => false,
    }
}

/// Serve a request from the embedded assets, falling back to the SPA shell.
pub(crate) fn serve(path: &str, headers: &HeaderMap) -> Response {
    let path = path.trim_start_matches('/');

    // Prefer the client's accepted encodings (brotli > gzip) when a matching
    // precompressed variant exists.
    let accept_encoding = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let (data, encoding): (Option<std::borrow::Cow<[u8]>>, Option<&str>) =
        if accept_encoding.contains("br") {
            match Assets::get(&format!("{path}.br")) {
                Some(f) => (Some(f.data), Some("br")),
                None => (Assets::get(path).map(|f| f.data), None),
            }
        } else if accept_encoding.contains("gzip") {
            match Assets::get(&format!("{path}.gz")) {
                Some(f) => (Some(f.data), Some("gzip")),
                None => (Assets::get(path).map(|f| f.data), None),
            }
        } else {
            (Assets::get(path).map(|f| f.data), None)
        };

    // `is_shell` marks the SPA fallback: it is always served as HTML.
    let (data, encoding, is_shell) = match data {
        Some(d) => (d, encoding, false),
        None if looks_like_file(&path) => {
            // A missing real file (e.g. a stale hashed chunk from an old cached
            // index.html) is a hard 404, never the HTML shell.
            return StatusCode::NOT_FOUND.into_response();
        }
        None => {
            // SPA fallback: any non-file route serves the app shell.
            let Some(shell) = Assets::get(SPA_SHELL) else {
                return StatusCode::NOT_FOUND.into_response();
            };
            (shell.data, None, true)
        }
    };

    let mut builder = Response::builder().status(StatusCode::OK);
    // The SPA shell is always HTML — the requested path (e.g. `/` or a client
    // route) must not leak its own content type, or the browser would download
    // the page or parse it as JS.
    let ct = if is_shell {
        content_type(SPA_SHELL)
    } else {
        content_type(&path)
    };
    builder = builder.header(header::CONTENT_TYPE, ct);
    if let Some(enc) = encoding {
        builder = builder.header(header::CONTENT_ENCODING, enc);
        // `Content-Length` is derived by axum from the body.
    }
    builder
        .body(Body::from(data.as_ref().to_vec()))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}
