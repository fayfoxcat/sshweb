//! Streaming file readers over local disk or SFTP, used to back HTTP Range
//! downloads without buffering the whole file.

use anyhow::{Context, Result};
use bytes::Buf;

use super::pool::SftpPool;
use crate::web::protocol::ServerConfig;

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
    /// Reading from a remote SFTP file.
    Remote {
        /// Open SFTP file handle (keeps the connection alive).
        file: openssh_sftp_client::file::File,
        /// Bytes read ahead that didn't fit the caller's last buffer (an SFTP
        /// read may return more bytes than asked for).
        pending: bytes::BytesMut,
        /// Number of bytes left to send in this range.
        remaining: u64,
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
                pending,
                remaining,
            } => {
                // Serve bytes read ahead on a previous call first.
                if !pending.is_empty() {
                    let n = pending.len().min(buf.len());
                    buf[..n].copy_from_slice(&pending[..n]);
                    pending.advance(n);
                    *remaining -= n as u64;
                    return Ok(n);
                }
                let want = (*remaining).min(buf.len() as u64) as u32;
                if want == 0 {
                    return Ok(0);
                }
                let data = file
                    .read(want, bytes::BytesMut::with_capacity(want as usize))
                    .await?;
                let Some(data) = data else { return Ok(0) };
                if data.is_empty() {
                    *remaining = 0;
                    return Ok(0);
                }
                // An SFTP read may return more bytes than asked for; copy what
                // fits the caller's buffer and keep the rest for next time.
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                if data.len() > n {
                    *pending = bytes::BytesMut::from(&data[n..]);
                }
                *remaining -= n as u64;
                Ok(n)
            }
        }
    }
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

/// Open a local file for streaming from `offset` onwards.
pub async fn reader_local(path: &str, offset: u64) -> Result<DownloadReader> {
    use tokio::io::AsyncSeekExt;
    let meta = tokio::fs::metadata(path)
        .await
        .with_context(|| format!("cannot stat file: {path}"))?;
    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("cannot open file: {path}"))?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    Ok(DownloadReader::Local {
        file,
        remaining: meta.len().saturating_sub(offset),
    })
}

/// Open a remote file for streaming from `offset` onwards.
///
/// `File` implements `AsyncSeek`; the SFTP protocol has no seek, so
/// `start_seek` only records the local offset, which the following `read`
/// uses as its request offset.
pub async fn reader_remote(
    pool: &SftpPool,
    server: &ServerConfig,
    path: &str,
    offset: u64,
) -> Result<DownloadReader> {
    let client = pool.client(server).await?;
    let size = stat_remote(pool, server, path).await?;
    let mut file = client
        .open(path)
        .await
        .map_err(|e| anyhow::anyhow!("SFTP open failed: {e}"))?;
    use tokio::io::AsyncSeekExt;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    Ok(DownloadReader::Remote {
        file,
        pending: bytes::BytesMut::new(),
        remaining: size.saturating_sub(offset),
    })
}
