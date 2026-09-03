//! Minimal streaming ZIP writer + archive walkers.
//!
//! No temp file and no whole-archive buffering: entries are pushed to the
//! caller's channel in 64 KiB chunks the moment they exist. Symbolic links are
//! skipped and recursion is depth-capped (see 已知坑 15). There is no zip64
//! support: a single file or a whole archive above 4 GB, or more than 65535
//! entries, raises an error.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sftp::copy_local;

    /// Build a temporary fixture tree and return its root directory:
    ///
    /// ```text
    /// root/
    ///   a.txt         (hello)
    ///   link.txt -> a.txt   (symlink, must be skipped)
    ///   sub/b.txt    (world)
    /// ```
    fn make_fixture() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "sshweb-arch-verify-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = dir.join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"hello\n").unwrap();
        std::fs::write(root.join("sub/b.txt"), b"world\n").unwrap();
        std::os::unix::fs::symlink("a.txt", root.join("link.txt")).unwrap();
        dir
    }

    /// Produce a ZIP archive for a single path and collect its bytes.
    fn collect_archive(path: &str) -> Vec<u8> {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            archive_local_stream(&[path.to_string()], false, tx).unwrap();
            let mut out = Vec::new();
            while let Some(chunk) = rx.recv().await {
                out.extend_from_slice(&chunk.unwrap());
            }
            out
        })
    }

    #[test]
    fn archive_skips_symlinks_and_names_children() {
        let dir = make_fixture();
        let root = dir.join("root");
        let out = collect_archive(root.to_str().unwrap());
        // List member names from the central directory (the entry names live
        // at a fixed 46-byte offset inside each central-directory record).
        let eocd = &out[out.len() - 22..];
        assert_eq!(&eocd[0..4], [0x50, 0x4b, 0x05, 0x06]);
        let cd_size = u32::from_le_bytes([eocd[12], eocd[13], eocd[14], eocd[15]]) as usize;
        let cd_off = u32::from_le_bytes([eocd[16], eocd[17], eocd[18], eocd[19]]) as usize;
        let mut names = Vec::new();
        let mut i = cd_off;
        let end = cd_off + cd_size;
        while i + 46 <= end && &out[i..i + 4] == [0x50, 0x4b, 0x01, 0x02] {
            let name_len = u16::from_le_bytes([out[i + 28], out[i + 29]]) as usize;
            names.push(String::from_utf8(out[i + 46..i + 46 + name_len].to_vec()).unwrap());
            let extra = u16::from_le_bytes([out[i + 30], out[i + 31]]) as usize;
            let comment = u16::from_le_bytes([out[i + 32], out[i + 33]]) as usize;
            i += 46 + name_len + extra + comment;
        }
        let mut names = names;
        names.sort();
        // The symlink `link.txt` is skipped; files are nested under the root
        // directory name. Directory entries end with `/` (ZIP spec; some
        // extractors refuse to treat bare names as directories).
        assert_eq!(
            names,
            vec!["root/", "root/a.txt", "root/sub/", "root/sub/b.txt"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_skips_symlinks_and_recurses() {
        let dir = make_fixture();
        let root = dir.join("root");
        let dst = dir.join("copied");
        copy_local(root.to_str().unwrap(), dst.to_str().unwrap()).unwrap();
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"hello\n");
        assert_eq!(std::fs::read(dst.join("sub/b.txt")).unwrap(), b"world\n");
        assert!(!dst.join("link.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

use std::path::Path;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use openssh_sftp_client::Sftp;

use super::local::{unix_secs, walk_local};
use super::pool::SftpPool;
use crate::web::protocol::ServerConfig;

/// Chunk size pushed into the HTTP response stream by the ZIP writer.
const ARCHIVE_CHUNK: usize = 64 * 1024;
/// Chunk size for reading source files while archiving.
const ARCHIVE_READ: usize = 256 * 1024;

const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;
/// General-purpose bit: UTF-8 file names.
const ZIP_FLAG_UTF8: u16 = 0x0800;
/// General-purpose bit: sizes follow in a data descriptor. Required here: the
/// `zip` crate's writer seeks back to patch file headers, which is impossible
/// on a streaming (non-rewindable) output, so we write the descriptor form
/// ourselves.
const ZIP_FLAG_DESCRIPTOR: u16 = 0x0008;

/// Central-directory record for one archived entry.
struct ZipCentralEntry {
    name: String,
    method: u16,
    flags: u16,
    dos_time: u16,
    dos_date: u16,
    crc: u32,
    compressed: u32,
    uncompressed: u32,
    offset: u32,
    is_dir: bool,
}

/// A minimal streaming ZIP writer: local headers + data descriptors are
/// emitted entry by entry and pushed to the browser in 64 KiB chunks the
/// moment they exist. No temp file, no whole-archive buffering, no `Seek`.
struct StreamZip {
    tx: tokio::sync::mpsc::UnboundedSender<Result<Bytes, anyhow::Error>>,
    buf: Vec<u8>,
    pos: u64,
    entries: Vec<ZipCentralEntry>,
}

impl StreamZip {
    fn new(tx: tokio::sync::mpsc::UnboundedSender<Result<Bytes, anyhow::Error>>) -> Self {
        Self {
            tx,
            buf: Vec::with_capacity(ARCHIVE_CHUNK),
            pos: 0,
            entries: Vec::new(),
        }
    }

    fn emit(&mut self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.buf.extend_from_slice(data);
        self.pos += data.len() as u64;
        if self.buf.len() >= ARCHIVE_CHUNK {
            self.flush()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::take(&mut self.buf);
        // The receiver is dropped when the client aborts: stop immediately.
        if self.tx.send(Ok(Bytes::from(chunk))).is_err() {
            bail!("client aborted download");
        }
        Ok(())
    }

    fn emit_local_header(
        &mut self,
        name: &str,
        method: u16,
        dos_time: u16,
        dos_date: u16,
        with_descriptor: bool,
    ) -> Result<()> {
        if self.pos > u32::MAX as u64 {
            bail!("打包文件超过 4 GB，流式 ZIP 暂不支持（无 zip64）");
        }
        let mut header = Vec::with_capacity(30 + name.len());
        header.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        header.extend_from_slice(&20u16.to_le_bytes()); // version needed
        let flags = ZIP_FLAG_UTF8
            | if with_descriptor {
                ZIP_FLAG_DESCRIPTOR
            } else {
                0
            };
        header.extend_from_slice(&flags.to_le_bytes());
        header.extend_from_slice(&method.to_le_bytes());
        header.extend_from_slice(&dos_time.to_le_bytes());
        header.extend_from_slice(&dos_date.to_le_bytes());
        // CRC and sizes are zero here; with the descriptor flag the real
        // values follow the data (see `CurrentFile::finish`).
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&(name.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // extra length
        header.extend_from_slice(name.as_bytes());
        self.emit(&header)
    }

    fn add_dir(&mut self, name: &str, dos_time: u16, dos_date: u16) -> Result<()> {
        // ZIP spec requires directory entries to end with `/` — Windows'
        // built-in extractor relies on it to tell directories apart (a bare
        // name like `.config` is treated as a 0-byte file and extraction
        // fails or produces a bogus file).
        let name = if name.ends_with('/') {
            name.to_string()
        } else {
            format!("{name}/")
        };
        let offset = self.pos as u32;
        self.emit_local_header(&name, METHOD_STORED, dos_time, dos_date, false)?;
        self.entries.push(ZipCentralEntry {
            name,
            method: METHOD_STORED,
            flags: ZIP_FLAG_UTF8,
            dos_time,
            dos_date,
            crc: 0,
            compressed: 0,
            uncompressed: 0,
            offset,
            is_dir: true,
        });
        Ok(())
    }

    /// Begin a file entry; returns the state to feed raw chunks into.
    fn begin_file(&mut self, name: String, dos_time: u16, dos_date: u16) -> Result<CurrentFile> {
        self.emit_local_header(&name, METHOD_DEFLATE, dos_time, dos_date, true)?;
        let offset = (self.pos as u32).saturating_sub(30 + name.len() as u32);
        Ok(CurrentFile {
            name,
            method: METHOD_DEFLATE,
            dos_time,
            dos_date,
            offset,
            crc: crc32fast::Hasher::new(),
            comp: flate2::Compress::new(flate2::Compression::default(), false),
            outbuf: vec![0u8; ARCHIVE_CHUNK],
            compressed: 0,
            uncompressed: 0,
        })
    }

    /// Finish the archive: central directory + end-of-central-directory.
    fn finish(mut self) -> Result<()> {
        let central_start = self.pos;
        let mut central: Vec<Vec<u8>> = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let mut c = Vec::with_capacity(46 + entry.name.len());
            c.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
            c.extend_from_slice(&20u16.to_le_bytes()); // version made by
            c.extend_from_slice(&20u16.to_le_bytes()); // version needed
            c.extend_from_slice(&entry.flags.to_le_bytes());
            c.extend_from_slice(&entry.method.to_le_bytes());
            c.extend_from_slice(&entry.dos_time.to_le_bytes());
            c.extend_from_slice(&entry.dos_date.to_le_bytes());
            c.extend_from_slice(&entry.crc.to_le_bytes());
            c.extend_from_slice(&entry.compressed.to_le_bytes());
            c.extend_from_slice(&entry.uncompressed.to_le_bytes());
            c.extend_from_slice(&(entry.name.len() as u16).to_le_bytes());
            c.extend_from_slice(&0u16.to_le_bytes()); // extra length
            c.extend_from_slice(&0u16.to_le_bytes()); // comment length
            c.extend_from_slice(&0u16.to_le_bytes()); // disk number
            c.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            c.extend_from_slice(&(if entry.is_dir { 0x10u32 << 16 } else { 0 }).to_le_bytes());
            c.extend_from_slice(&entry.offset.to_le_bytes());
            c.extend_from_slice(entry.name.as_bytes());
            central.push(c);
        }
        for c in &central {
            self.emit(c)?;
        }
        let central_size = self.pos - central_start;
        if self.entries.len() > u16::MAX as usize
            || central_size > u32::MAX as u64
            || central_start > u32::MAX as u64
        {
            bail!("打包条目过多或过大，流式 ZIP 暂不支持（无 zip64）");
        }
        let mut eocd = Vec::with_capacity(22);
        eocd.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
        eocd.extend_from_slice(&0u16.to_le_bytes()); // disk number
        eocd.extend_from_slice(&0u16.to_le_bytes()); // central-dir disk
        eocd.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        eocd.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        eocd.extend_from_slice(&(central_size as u32).to_le_bytes());
        eocd.extend_from_slice(&(central_start as u32).to_le_bytes());
        eocd.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.emit(&eocd)?;
        self.flush()
    }
}

/// In-progress file entry: raw deflate + CRC, streamed chunk by chunk.
struct CurrentFile {
    name: String,
    method: u16,
    dos_time: u16,
    dos_date: u16,
    offset: u32,
    crc: crc32fast::Hasher,
    comp: flate2::Compress,
    outbuf: Vec<u8>,
    compressed: u64,
    uncompressed: u64,
}

impl CurrentFile {
    fn push(&mut self, zip: &mut StreamZip, data: &[u8]) -> Result<()> {
        self.crc.update(data);
        self.uncompressed += data.len() as u64;
        let mut input = data;
        while !input.is_empty() {
            let before_in = self.comp.total_in();
            let before_out = self.comp.total_out();
            let status = self
                .comp
                .compress(input, &mut self.outbuf, flate2::FlushCompress::None)
                .map_err(|err| anyhow::anyhow!("deflate failed for {}: {err}", self.name))?;
            let consumed = (self.comp.total_in() - before_in) as usize;
            let produced = (self.comp.total_out() - before_out) as usize;
            if produced > 0 {
                zip.emit(&self.outbuf[..produced])?;
                self.compressed += produced as u64;
            }
            match status {
                flate2::Status::Ok | flate2::Status::BufError => {
                    if consumed == 0 && produced == 0 {
                        bail!("deflate stalled on {}", self.name);
                    }
                    input = &input[consumed..];
                }
                flate2::Status::StreamEnd => input = &[],
            }
        }
        Ok(())
    }

    fn finish(mut self, zip: &mut StreamZip) -> Result<()> {
        loop {
            let before_out = self.comp.total_out();
            let status = self
                .comp
                .compress(&[], &mut self.outbuf, flate2::FlushCompress::Finish)
                .map_err(|err| anyhow::anyhow!("deflate finish failed for {}: {err}", self.name))?;
            let produced = (self.comp.total_out() - before_out) as usize;
            if produced > 0 {
                zip.emit(&self.outbuf[..produced])?;
                self.compressed += produced as u64;
            }
            if status == flate2::Status::StreamEnd {
                break;
            }
        }
        if self.compressed > u32::MAX as u64 || self.uncompressed > u32::MAX as u64 {
            bail!(
                "打包文件 {} 超过 4 GB，流式 ZIP 暂不支持（无 zip64）",
                self.name
            );
        }
        let crc = self.crc.finalize();
        let mut descriptor = Vec::with_capacity(16);
        descriptor.extend_from_slice(&[0x50, 0x4B, 0x07, 0x08]);
        descriptor.extend_from_slice(&crc.to_le_bytes());
        descriptor.extend_from_slice(&(self.compressed as u32).to_le_bytes());
        descriptor.extend_from_slice(&(self.uncompressed as u32).to_le_bytes());
        zip.emit(&descriptor)?;
        zip.entries.push(ZipCentralEntry {
            name: self.name,
            method: self.method,
            flags: ZIP_FLAG_UTF8 | ZIP_FLAG_DESCRIPTOR,
            dos_time: self.dos_time,
            dos_date: self.dos_date,
            crc,
            compressed: self.compressed as u32,
            uncompressed: self.uncompressed as u32,
            offset: self.offset,
            is_dir: false,
        });
        Ok(())
    }
}

/// MS-DOS date/time from Unix seconds (`None` -> 1980-01-01).
fn dos_datetime(secs: Option<u64>) -> (u16, u16) {
    let Some(secs) = secs else { return (0, 0x21) };
    let days = secs / 86_400;
    let time = secs % 86_400;
    let (h, m, s) = (time / 3600, (time % 3600) / 60, time % 60);
    let (year, month, day) = civil_from_days(days as i64);
    let year = year.clamp(1980, 2107) as u64;
    let dos_date = (((year - 1980) << 9) | ((month as u64) << 5) | day as u64) as u16;
    let dos_time = ((h << 11) | (m << 5) | (s / 2)) as u16;
    (dos_time, dos_date)
}

/// The ZIP entry name for a node: the relative path under the archive root,
/// prefixed with the root's own base name (empty-base handling matches the
/// pre-shared-walker behaviour). Both sides are normalized so a root path that
/// ends in `/` (or a rel that starts with one) never produces a `//` in the
/// entry name — some extractors (Windows, busybox unzip) reject those.
fn zip_name(base: &str, rel: &str) -> String {
    let base = base.trim_end_matches('/');
    let rel = rel.trim_start_matches('/');
    if rel.is_empty() {
        base.to_string()
    } else if base.is_empty() {
        rel.to_string()
    } else {
        format!("{base}/{rel}")
    }
}

/// Days since 1970-01-01 -> (year, month, day) (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Stream a ZIP archive of local paths: walks the filesystem, pushing 64 KiB
/// `Bytes` chunks into `tx` while it goes (off-thread; see 已知坑 11).
///
/// Symbolic links are skipped and depth is capped (see 已知坑 15). If the
/// browser disconnects, sending fails and the walk aborts. There is no
/// temp file: `tx` receives errors and the caller sends one final error.
///
/// When `flat` is true (a single folder downloaded on its own) the folder's
/// own name is not wrapped into the archive — its contents appear at the top
/// level. Multi-selection always keeps each item's name as a top-level entry.
pub fn archive_local_stream(
    paths: &[String],
    flat: bool,
    tx: tokio::sync::mpsc::UnboundedSender<Result<Bytes, anyhow::Error>>,
) -> Result<()> {
    use std::io::Read;

    let mut zip = StreamZip::new(tx);
    for path in paths {
        let base = if flat {
            String::new()
        } else {
            super::file_basename(path)
        };
        let p = Path::new(path);
        walk_local(p, 0, &mut |fs_path, meta| {
            // The ZIP name of each node is its path relative to the archive
            // root, prefixed with the root's own name (empty-root handling
            // matches the pre-shared-walker behaviour).
            let rel = fs_path.strip_prefix(p).unwrap_or(fs_path);
            let rel_str = rel.to_string_lossy();
            // In flat mode the root folder itself is not archived (only its
            // contents), so skip the root directory entry.
            if flat && rel_str.is_empty() && meta.is_dir() {
                return Ok(());
            }
            let entry_name = zip_name(&base, &rel_str);
            let (dos_time, dos_date) = dos_datetime(unix_secs(meta.modified().ok()));
            if meta.is_dir() {
                zip.add_dir(&entry_name, dos_time, dos_date)?;
            } else {
                let mut file =
                    std::fs::File::open(fs_path).with_context(|| format!("open {fs_path:?}"))?;
                let mut entry = zip.begin_file(entry_name.to_string(), dos_time, dos_date)?;
                let mut buf = vec![0u8; ARCHIVE_READ];
                loop {
                    let n = file.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    entry.push(&mut zip, &buf[..n])?;
                }
                entry.finish(&mut zip)?;
            }
            Ok(())
        })?;
    }
    zip.finish()
}

/// Streaming ZIP archive of remote SFTP paths, emitting 64 KiB chunks into
/// `tx` (SFTP reads are async, compression runs on 256 KiB blocks; no
/// whole-archive buffering, no temp file).
///
/// Symbolic links are skipped and depth is capped (see
/// [`super::remote::walk_remote`]); on mid-archival I/O errors (e.g. a dead
/// tunnel) the pool is invalidated so later operations reconnect.
pub async fn archive_remote_stream(
    pool: &SftpPool,
    server: &ServerConfig,
    paths: &[String],
    flat: bool,
    tx: tokio::sync::mpsc::UnboundedSender<Result<Bytes, anyhow::Error>>,
) -> Result<()> {
    /// Visitor streaming one remote tree into the archive.
    struct ArchiveRemoteVisitor {
        zip: StreamZip,
        root: String,
        base: String,
        flat: bool,
    }

    impl super::remote::RemoteWalker for ArchiveRemoteVisitor {
        fn visit<'a>(
            &'a mut self,
            client: &'a Sftp,
            fs_path: &'a str,
            node: &'a super::remote::RemoteNode,
        ) -> futures_util::future::BoxFuture<'a, Result<()>> {
            // The ZIP name of each node is its path relative to the archive
            // root, prefixed with the root's own name (empty-root handling
            // matches the pre-shared-walker behaviour).
            let rel = fs_path.strip_prefix(&self.root).unwrap_or(fs_path);
            // In flat mode the root folder itself is not archived.
            let is_root = rel.is_empty();
            let entry_name = zip_name(&self.base, rel);
            let (dos_time, dos_date) = dos_datetime(node.modified);
            let is_dir = node.is_dir;
            let size = node.size;
            Box::pin(async move {
                if is_dir {
                    if !(self.flat && is_root) {
                        self.zip.add_dir(&entry_name, dos_time, dos_date)?;
                    }
                } else {
                    let mut file = client.open(fs_path).await?;
                    let mut entry =
                        self.zip
                            .begin_file(entry_name.to_string(), dos_time, dos_date)?;
                    let mut remaining = size;
                    while remaining > 0 {
                        // `read` reads at most `want` bytes and advances the
                        // file's internal offset.
                        let want = remaining.min(ARCHIVE_READ as u64) as u32;
                        let buf = bytes::BytesMut::zeroed(want as usize);
                        let data = file.read(want, buf).await?;
                        let Some(data) = data else { break };
                        if data.is_empty() {
                            break;
                        }
                        entry.push(&mut self.zip, &data)?;
                        remaining -= data.len() as u64;
                    }
                    entry.finish(&mut self.zip)?;
                    let _ = file.close().await;
                }
                Ok(())
            })
        }
    }

    let client = pool.client(server).await?;
    let mut zip = StreamZip::new(tx);
    for path in paths {
        let name = if flat {
            String::new()
        } else {
            super::file_basename(path)
        };
        let mut visitor = ArchiveRemoteVisitor {
            zip,
            root: path.clone(),
            base: name,
            flat,
        };
        if let Err(err) = super::remote::walk_remote(&client, path, 0, &mut visitor).await {
            pool.invalidate().await;
            return Err(err);
        }
        zip = visitor.zip;
    }
    zip.finish()
}
