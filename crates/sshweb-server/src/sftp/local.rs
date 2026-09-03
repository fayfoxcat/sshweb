//! Local filesystem operations (the server's own host).

use std::path::Path;

use anyhow::{Context, Result};
use bytes::Bytes;

use super::MAX_LIST_ENTRIES;
use crate::web::protocol::SftpEntry;

/// List a local directory on the server.
pub fn list_local(path: &str) -> Result<(Vec<SftpEntry>, bool)> {
    let mut entries = Vec::new();
    let mut truncated = false;
    let rd = std::fs::read_dir(path).with_context(|| format!("cannot list directory: {path}"))?;
    for item in rd {
        if entries.len() >= MAX_LIST_ENTRIES {
            truncated = true;
            break;
        }
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        // `DirEntry::metadata()` does not follow symlinks, so we can flag
        // links (`is_link`) while still displaying the **target's** kind,
        // permissions and size (follow-up stat below).
        let link_meta = item.metadata()?;
        let is_link = link_meta.file_type().is_symlink();
        let md = if is_link {
            std::fs::metadata(item.path())?
        } else {
            link_meta
        };
        entries.push(SftpEntry {
            name,
            is_dir: md.is_dir(),
            is_link,
            size: md.len(),
            modified: unix_secs(md.modified().ok()),
            created: unix_secs(md.created().ok()),
            mode: entry_mode(&md),
        });
    }
    Ok((entries, truncated))
}

/// The full Unix mode (including type bits) of a local file, or 0 elsewhere.
#[cfg(unix)]
fn entry_mode(md: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    md.permissions().mode()
}

#[cfg(not(unix))]
fn entry_mode(_md: &std::fs::Metadata) -> u32 {
    0
}

/// Convert an optional [`std::time::SystemTime`] into unix seconds.
pub(crate) fn unix_secs(time: Option<std::time::SystemTime>) -> Option<u64> {
    time.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

/// Ensure the parent directory of `path` exists, creating it (and any missing
/// ancestors) if needed. `what` is the failure context label (log-level only).
fn ensure_parent_dir(path: &str, what: &str) -> Result<()> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| format!("{what}: {parent:?}"))?;
        }
    }
    Ok(())
}

/// Read a local file on the server.
pub fn read_local(path: &str) -> Result<Bytes> {
    let data = std::fs::read(path).with_context(|| format!("cannot read file: {path}"))?;
    Ok(Bytes::from(data))
}

/// Write a local file on the server.
pub fn write_local(path: &str, data: &[u8]) -> Result<()> {
    ensure_parent_dir(path, "cannot create parent dir")?;
    std::fs::write(path, data).with_context(|| format!("cannot write file: {path}"))?;
    Ok(())
}

/// Write bytes at an offset of a local file, creating it on offset 0.
///
/// Used for chunked uploads; `offset == 0` truncates any existing file.
pub fn write_at_local(path: &str, offset: u64, data: &[u8]) -> Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    ensure_parent_dir(path, "cannot create parent dir")?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .open(path)
        .with_context(|| format!("cannot open file for write-at: {path}"))?;
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)
        .with_context(|| format!("cannot write file at offset: {path}"))?;
    Ok(())
}

/// Create a local directory.
pub fn mkdir_local(path: &str) -> Result<()> {
    std::fs::create_dir_all(path).with_context(|| format!("cannot create dir: {path}"))?;
    Ok(())
}

/// Remove a local file or directory (recursively).
pub fn remove_local(path: &str, is_dir: bool) -> Result<()> {
    if is_dir {
        std::fs::remove_dir_all(path).with_context(|| format!("cannot remove dir: {path}"))
    } else {
        std::fs::remove_file(path).with_context(|| format!("cannot remove file: {path}"))
    }
}

/// Rename a local file or directory.
pub fn rename_local(from: &str, to: &str) -> Result<()> {
    std::fs::rename(from, to).with_context(|| format!("cannot rename {from} to {to}"))
}

/// Copy a local file or directory (recursively).
///
/// Symbolic links are skipped and recursion is depth-limited, mirroring the
/// archive walkers to avoid cycles.
pub fn copy_local(from: &str, to: &str) -> Result<()> {
    let src = Path::new(from);
    let dst = Path::new(to);
    walk_local(src, 0, &mut |path, meta| {
        // `dst` is the target directory when copying a tree root, or the
        // target file when copying a single file; children map 1:1 by path.
        let target = if path == src {
            dst.to_path_buf()
        } else {
            dst.join(path.strip_prefix(src).unwrap_or(path))
        };
        if meta.is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("cannot create dir for copy: {target:?}"))?;
        } else {
            // Ensure the destination directory exists before copying a file.
            ensure_parent_dir(&target.to_string_lossy(), "cannot create dir for copy")?;
            std::fs::copy(path, &target)
                .with_context(|| format!("cannot copy {path:?} to {target:?}"))?;
        }
        Ok(())
    })
}

/// Recursively visit every entry under `root`, skipping symbolic links and
/// capping depth at 32 (see 已知坑 15/17). `on_node` is invoked for each
/// visited file or directory with its path and metadata; the root itself is
/// visited at `depth` 0.
///
/// Shared by the recursive copy and the streaming ZIP archive walker so both
/// apply the same symlink/depth rules.
pub(crate) fn walk_local<F>(root: &Path, depth: u32, on_node: &mut F) -> Result<()>
where
    F: FnMut(&Path, &std::fs::Metadata) -> Result<()>,
{
    if depth > 32 {
        return Ok(());
    }
    let meta = std::fs::symlink_metadata(root).with_context(|| format!("cannot stat {root:?}"))?;
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    on_node(root, &meta)?;
    if meta.is_dir() {
        for entry in std::fs::read_dir(root).with_context(|| format!("cannot read dir {root:?}"))? {
            let entry = entry?;
            walk_local(&entry.path(), depth + 1, on_node)?;
        }
    }
    Ok(())
}
