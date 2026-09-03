//! SFTP file download + streaming ZIP archive handlers.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, RawQuery, State};
use axum::http::header;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::stream::{self, Stream};

use super::auth_session;
use crate::sftp;
use crate::ServerState;

/// Handler for an SFTP file download with HTTP Range support.
///
/// Streams the file from the shell's filesystem (local disk or remote SFTP)
/// directly to the browser's native downloader: no whole-file buffering, and
/// the browser can pause / resume / cancel the transfer. A later request with
/// a `Range` header resumes from the given byte offset.
pub(crate) async fn get_sftp_download(
    Path((name, sid)): Path<(String, u32)>,
    State(state): State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let session = match auth_session(&state, &headers, &name) {
        Ok(session) => session,
        Err(resp) => return resp,
    };
    let sid = sshweb_core::Sid(sid);
    let path = match params.get("path") {
        Some(path) if !path.is_empty() => path.clone(),
        _ => return (StatusCode::BAD_REQUEST, "missing path").into_response(),
    };

    let size = match session.download_size(sid, &path).await {
        Ok(size) => size,
        Err(err) => {
            tracing::warn!(?err, %sid, path = %path, "download stat failed");
            return (StatusCode::NOT_FOUND, "file not found").into_response();
        }
    };

    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
    let (start, end, partial) = match parse_range(range, size) {
        Some(rng) => (rng.0, rng.1, true),
        None if range.is_some() => {
            // Unsupported / unsatisfiable range.
            return (StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable").into_response();
        }
        None => (0u64, size.saturating_sub(1), false),
    };
    let length = end.saturating_sub(start) + 1;

    let reader = match session.download_reader(sid, &path, start).await {
        Ok(reader) => reader,
        Err(err) => {
            tracing::warn!(?err, %sid, path = %path, "download open failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "failed to open file").into_response();
        }
    };

    let filename = sftp::file_basename(&path);
    // Strip characters that could break the Content-Disposition header.
    let safe_filename = sanitize_disposition_name(&filename);

    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, length)
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{safe_filename}\""),
        );
    if partial {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    builder
        .body(Body::from_stream(download_stream(reader, length)))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Stream a ZIP archive of multiple files / folders for a shell.
///
/// The paths arrive as repeated `?path=<percent-encoded absolute path>`
/// parameters. The archive is generated on the fly and streamed to the
/// browser's native downloader — no server-side temp file and no whole-archive
/// buffer in memory. Blocking local walking runs inside `spawn_blocking` in
/// the session.
pub(crate) async fn get_sftp_archive(
    Path((name, sid)): Path<(String, u32)>,
    State(state): State<Arc<ServerState>>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Response {
    let session = match auth_session(&state, &headers, &name) {
        Ok(session) => session,
        Err(resp) => return resp,
    };
    let (paths, filename, flat) = parse_archive_params(raw.as_deref());
    if paths.is_empty() {
        return (StatusCode::BAD_REQUEST, "missing path").into_response();
    }
    let stream = match session.sftp_archive_stream(sshweb_core::Sid(sid), paths, flat) {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%sid, ?err, "archive stream setup failed");
            return (StatusCode::BAD_REQUEST, "cannot archive").into_response();
        }
    };
    // The frontend supplies the desired archive name (folder.zip or a
    // `a、b等N个文件.zip` label); default to archive.zip when absent. Using the
    // Content-Disposition here (not just the <a download> hint) makes the name
    // reliable even where the browser prefers the server header.
    let filename = filename.unwrap_or_else(|| "archive.zip".to_string());
    let safe = sanitize_disposition_name(&filename);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{safe}\""),
        )
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// Strip characters that could break a `Content-Disposition` filename header
/// (quotes, CR/LF, backslash). Shared by the single-file download and the ZIP
/// archive handlers.
fn sanitize_disposition_name(filename: &str) -> String {
    filename
        .chars()
        .filter(|c| !matches!(c, '"' | '\r' | '\n' | '\\'))
        .collect()
}

/// Collect every `path=` query parameter plus the optional `filename` archive
/// name and `flat` unwrap flag (`?path=a&path=b&filename=foo.zip&flat=1`), each
/// percent-decoded.
fn parse_archive_params(raw: Option<&str>) -> (Vec<String>, Option<String>, bool) {
    let Some(raw) = raw else {
        return (Vec::new(), None, false);
    };
    let mut paths = Vec::new();
    let mut filename = None;
    let mut flat = false;
    for pair in raw.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "path" {
            paths.push(percent_decode(value));
        } else if key == "filename" {
            filename = Some(percent_decode(value));
        } else if key == "flat" {
            flat = value == "1" || value == "true";
        }
    }
    (paths, filename, flat)
}

fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() && hex(bytes[i + 1]).zip(hex(bytes[i + 2])).is_some() => {
                let h = hex(bytes[i + 1]).unwrap();
                let l = hex(bytes[i + 2]).unwrap();
                out.push(h * 16 + l);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse a single-range `Range` header into an inclusive `(start, end)`.
///
/// Supports `bytes=start-end`, `bytes=start-` and `bytes=-suffix`. Returns
/// `None` for unsupported / multi-range / unsatisfiable requests.
fn parse_range(range: Option<&str>, size: u64) -> Option<(u64, u64)> {
    if size == 0 {
        return None;
    }
    let spec = range?.strip_prefix("bytes=")?;
    let (start_s, end_s) = spec.split_once('-')?;
    let last = size - 1;
    if start_s.is_empty() {
        // Suffix range: last `suffix` bytes.
        let suffix: u64 = end_s.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let start = size.saturating_sub(suffix);
        Some((start, last))
    } else {
        let start: u64 = start_s.parse().ok()?;
        if start >= size {
            return None;
        }
        let end = if end_s.is_empty() {
            last
        } else {
            end_s.parse::<u64>().ok()?.min(last)
        };
        if end < start {
            return None;
        }
        Some((start, end))
    }
}

/// Build a byte stream from a [`sftp::DownloadReader`], producing at most
/// `length` bytes (the requested range).
fn download_stream(
    reader: sftp::DownloadReader,
    length: u64,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    // Match the remote SFTP read limit (OpenSSH advertises 256 KiB) so each
    // chunk is one round-trip on high-latency links; the reader clamps to the
    // server's actual limit if smaller.
    const CHUNK: u64 = 256 * 1024;
    stream::unfold((reader, length), |(mut reader, mut remaining)| async move {
        if remaining == 0 {
            return None;
        }
        let want = remaining.min(CHUNK) as usize;
        let mut buf = vec![0u8; want];
        let n = match reader.read(&mut buf).await {
            Ok(n) => n,
            Err(err) => {
                tracing::warn!(?err, "download read failed");
                return Some((
                    Err(std::io::Error::other("download read failed")),
                    (reader, 0),
                ));
            }
        };
        if n == 0 {
            return None;
        }
        remaining -= n as u64;
        Some((Ok(Bytes::from(buf[..n].to_vec())), (reader, remaining)))
    })
}
