//! Streaming file readers over local disk or SFTP, used to back HTTP Range
//! downloads without buffering the whole file.

use std::collections::VecDeque;

use anyhow::{Context, Result};
use bytes::{Buf, Bytes, BytesMut};

use super::pool::SftpPool;
use crate::web::protocol::ServerConfig;

/// Bytes per SFTP read request.
///
/// OpenSSH's sftp-server advertises 256 KiB, and the client library never asks
/// for more than the server allows in one request. A server with a smaller
/// limit shrinks the piece after its first short reply — see [`fill`].
const READ_PIECE_BYTES: u32 = 256 * 1024;

/// Reads kept in flight for one download.
///
/// The client library awaits every request's reply, so a reader that sends one
/// read at a time is stop-and-wait: throughput is `piece / RTT` no matter how
/// fast the link is — 5.3 MiB/s over a 20 ms RTT link, measured (已知坑 77 is
/// the upload twin of this). Eight 256 KiB reads in flight cover a 100 ms RTT
/// at 20 MiB/s, and the default SSH channel window is 2 MiB, so going wider
/// would mostly queue inside the transport.
const MAX_INFLIGHT_READS: usize = 8;

/// A chunked reader over a file (local disk or remote SFTP), used to stream
/// downloads with HTTP Range support without buffering the whole file.
pub enum DownloadReader {
    /// Reading from the local filesystem.
    Local {
        /// Open file handle positioned at the requested offset.
        file: tokio::fs::File,
        /// Number of bytes left to send in this range.
        remaining: u64,
    },
    /// Reading from a remote SFTP file, several pieces at a time.
    Remote {
        /// Handle the per-piece clones are made from. Clones share the remote
        /// handle (the CLOSE request is sent once, by the last one), so the
        /// piece's own offset — which the read request carries — is what makes
        /// them independent.
        file: openssh_sftp_client::file::File,
        /// Bytes requested per read; the server's own limit if that is less.
        piece: u32,
        /// Offset of the next piece to request.
        next: u64,
        /// End of the requested range (exclusive).
        end: u64,
        /// Pieces already read, in file order.
        ready: VecDeque<Bytes>,
    },
}

impl DownloadReader {
    /// Read up to `buf.len()` bytes, returning the number read (0 = EOF).
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        match self {
            DownloadReader::Local { file, remaining } => {
                use tokio::io::AsyncReadExt;
                let want = (*remaining).min(buf.len() as u64) as usize;
                if want == 0 {
                    return Ok(0);
                }
                let n = file.read(&mut buf[..want]).await?;
                *remaining -= n as u64;
                Ok(n)
            }
            DownloadReader::Remote {
                file,
                piece,
                next,
                end,
                ready,
            } => {
                // Read ahead until something is buffered, or the range ends.
                // `fill` either moves `next` forward or ends the range, so this
                // cannot spin.
                while ready.is_empty() && *next < *end {
                    fill(file, piece, next, end, ready).await?;
                }
                let Some(mut chunk) = ready.pop_front() else {
                    return Ok(0);
                };
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    // The caller asked for less than a whole piece: keep the
                    // rest at the front for the next call.
                    chunk.advance(n);
                    ready.push_front(chunk);
                }
                Ok(n)
            }
        }
    }
}

/// Read one wave of pieces from `next` into `ready`.
///
/// Two invariants keep the caller's read-ahead loop from spinning: the wave
/// either moves `next` forward, or it sets `end = next` when the file turns out
/// to be shorter than the size it was opened with.
async fn fill(
    file: &openssh_sftp_client::file::File,
    piece: &mut u32,
    next: &mut u64,
    end: &mut u64,
    ready: &mut VecDeque<Bytes>,
) -> Result<()> {
    use tokio::io::AsyncSeekExt;

    // Submit the whole wave before awaiting any reply, so the channel stays
    // busy across a round trip instead of draining after every piece.
    let mut requests = Vec::with_capacity(MAX_INFLIGHT_READS);
    let mut offset = *next;
    while requests.len() < MAX_INFLIGHT_READS && offset < *end {
        let want = (*end - offset).min(u64::from(*piece)) as u32;
        // `File::read` takes `&mut self`, so each piece reads through its own
        // clone; the clone's offset is local (see `reader_remote`).
        let mut handle = file.clone();
        handle.seek(std::io::SeekFrom::Start(offset)).await?;
        requests.push(async move {
            handle
                .read(want, BytesMut::with_capacity(want as usize))
                .await
                .map(|data| (offset, want, data))
        });
        offset += u64::from(want);
    }

    let mut read = 0u64;
    let replies = futures_util::future::try_join_all(requests)
        .await
        .map_err(|e| anyhow::anyhow!("SFTP read failed: {e}"))?;
    for (offset, want, data) in replies {
        let Some(data) = data else {
            // EOF: the file is shorter than the size we were given.
            *end = offset;
            break;
        };
        let got = data.len() as u64;
        if got == 0 {
            *end = offset;
            break;
        }
        ready.push_back(data.freeze());
        *next = offset + got;
        read += got;
        if got < u64::from(want) {
            // Short reply: the server's read limit is below `piece`. The rest
            // of this wave was requested at offsets that assumed a full piece,
            // so it is dropped, and the next wave restarts here with the piece
            // size the server just showed us.
            *piece = got as u32;
            break;
        }
    }
    if read == 0 {
        // Nothing usable came back: end the range instead of asking again.
        *end = *next;
    }
    Ok(())
}

/// Open a local file for streaming `length` bytes from `offset` onwards.
pub async fn reader_local(path: &str, offset: u64, length: u64) -> Result<DownloadReader> {
    use tokio::io::AsyncSeekExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("cannot open file: {path}"))?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    Ok(DownloadReader::Local {
        file,
        remaining: length,
    })
}

/// Size of a local file.
pub fn size_local(path: &str) -> Result<u64> {
    let meta = std::fs::metadata(path).with_context(|| format!("cannot stat file: {path}"))?;
    Ok(meta.len())
}

/// Stat a remote file and return its byte size (`0` if unknown).
async fn stat_remote(pool: &SftpPool, server: &ServerConfig, path: &str) -> Result<u64> {
    let client = pool.client(server).await?;
    let mut fs = client.fs();
    let meta = fs
        .metadata(path)
        .await
        .map_err(|e| anyhow::anyhow!("SFTP stat failed: {e}"))?;
    Ok(meta.len().unwrap_or(0))
}

/// Size of a remote file.
pub async fn size_remote(pool: &SftpPool, server: &ServerConfig, path: &str) -> Result<u64> {
    stat_remote(pool, server, path).await
}

/// Open a remote file for streaming `length` bytes from `offset` onwards.
///
/// `File` implements `AsyncSeek`, but the SFTP protocol has no seek: it only
/// records the local offset, which the following read sends as part of its
/// request. No read happens here — the first one is issued by the first
/// [`read`] call, so a download that never gets read costs nothing.
///
/// [`read`]: DownloadReader::read
pub async fn reader_remote(
    pool: &SftpPool,
    server: &ServerConfig,
    path: &str,
    offset: u64,
    length: u64,
) -> Result<DownloadReader> {
    let client = pool.client(server).await?;
    let size = stat_remote(pool, server, path).await?;
    let file = client
        .open(path)
        .await
        .map_err(|e| anyhow::anyhow!("SFTP open failed: {e}"))?;
    let end = offset.saturating_add(length).min(size);
    Ok(DownloadReader::Remote {
        file,
        piece: READ_PIECE_BYTES,
        next: offset.min(end),
        end,
        ready: VecDeque::new(),
    })
}
