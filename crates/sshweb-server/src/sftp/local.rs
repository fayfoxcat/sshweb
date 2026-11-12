//! Local filesystem operations (the server's own host).

use std::path::{Path, PathBuf};

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

/// Recover the real directory entry behind a path the client sent back.
///
/// Same problem as the remote side (`sftp/remote.rs::resolve_entry`, 已知坑
/// 101): our protocol carries paths as UTF-8 text while Unix names are
/// arbitrary bytes, so a file whose name is not valid UTF-8 is listed with
/// U+FFFD replacement characters and can never be addressed by the string that
/// comes back. If the path does not resolve, look for the entry whose **lossy**
/// rendering matches and use its raw path.
fn resolve_entry(path: &str) -> PathBuf {
    let as_is = PathBuf::from(path);
    if std::fs::symlink_metadata(&as_is).is_ok() {
        return as_is;
    }
    let (Some(parent), Some(name)) = (as_is.parent(), as_is.file_name()) else {
        return as_is;
    };
    if parent.as_os_str().is_empty() {
        return as_is;
    }
    let want = name.to_string_lossy();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return as_is;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy() == want {
            return entry.path();
        }
    }
    as_is
}

/// Remove a local file or directory (recursively).
pub fn remove_local(path: &str, is_dir: bool) -> Result<()> {
    let target = resolve_entry(path);
    if is_dir {
        std::fs::remove_dir_all(&target)
            .with_context(|| format!("cannot remove dir: {}", target.display()))
    } else {
        std::fs::remove_file(&target)
            .with_context(|| format!("cannot remove file: {}", target.display()))
    }
}

/// Rename a local file or directory.
pub fn rename_local(from: &str, to: &str) -> Result<()> {
    let source = resolve_entry(from);
    std::fs::rename(&source, to)
        .with_context(|| format!("cannot rename {} to {to}", source.display()))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "sshweb-local-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A name that is not valid UTF-8 — exactly what an old Info-ZIP `unzip`
    /// produces from a UTF-8 zip (已知坑 101). Only Unix allows creating it.
    #[cfg(unix)]
    fn create_non_utf8_name(dir: &Path, suffix: &str) -> PathBuf {
        use std::os::unix::ffi::OsStrExt;
        // 0x41 'A', then a lone 0xD5 continuation-less byte: invalid UTF-8.
        let mut name = vec![b'A', 0xD5, b'B'];
        name.extend_from_slice(suffix.as_bytes());
        let path = dir.join(std::ffi::OsStr::from_bytes(&name));
        std::fs::write(&path, b"x").unwrap();
        path
    }

    /// The listing shows the U+FFFD form; deleting with that string must still
    /// find and remove the real file instead of failing with ENOENT.
    #[cfg(unix)]
    #[test]
    fn remove_local_resolves_a_lossy_name() {
        let scratch = Scratch::new("remove");
        let real = create_non_utf8_name(&scratch.0, ".txt");
        let listed = format!(
            "{}/{}",
            scratch.0.display(),
            real.file_name().unwrap().to_string_lossy()
        );

        remove_local(&listed, false).expect("the lossy path must resolve to the real entry");
        assert!(!real.exists());
    }

    /// Same for rename — the format path of "fix the name by renaming it".
    #[cfg(unix)]
    #[test]
    fn rename_local_resolves_a_lossy_name() {
        let scratch = Scratch::new("rename");
        let real = create_non_utf8_name(&scratch.0, ".txt");
        let listed = format!(
            "{}/{}",
            scratch.0.display(),
            real.file_name().unwrap().to_string_lossy()
        );
        let target = scratch.0.join("fixed.txt");

        rename_local(&listed, target.to_str().unwrap()).unwrap();
        assert!(target.exists());
        assert!(!real.exists());
    }

    /// An existing path still takes the fast path unchanged, and a path that
    /// matches nothing is passed through (so the caller sees the real error).
    #[test]
    fn resolve_entry_passes_through_existing_and_unknown_paths() {
        let scratch = Scratch::new("resolve");
        let file = scratch.0.join("ok.txt");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(resolve_entry(file.to_str().unwrap()), file);

        let missing = scratch.0.join("missing.txt");
        assert_eq!(resolve_entry(missing.to_str().unwrap()), missing);
    }
}
