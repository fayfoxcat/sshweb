//! Remote SFTP operations (openssh-sftp-client over the shared russh channel
//! pool). All operations take an [`openssh_sftp_client::Sftp`] (an `Arc` in
//! the pool) and create a fresh [`Fs`] per call, matching the crate's
//! `&mut self` API. Symbolic links are skipped and recursion is depth-capped
//! in the walkers (see 已知坑 15/17).

use std::path::Path;

use anyhow::{Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use openssh_sftp_client::{fs::Fs, Sftp};

use super::pool::{with_remote, SftpPool};
use super::MAX_LIST_ENTRIES;
use crate::web::protocol::{ServerConfig, SftpEntry};

/// Combine the permission bits and the file-type bits of a [`MetaData`] into
/// the `mode` field of an [`SftpEntry`] (which includes the type part, like
/// the local listing).
fn entry_mode(md: &openssh_sftp_client::metadata::MetaData) -> u32 {
    let perms = md.permissions().map(|p| p.as_raw().bits()).unwrap_or(0);
    let ft = md.file_type().map(|t| t.as_raw() as u32).unwrap_or(0);
    perms | ft
}

/// List a directory for a remote SSH shell.
pub async fn list_remote(
    pool: &SftpPool,
    server: &ServerConfig,
    path: &str,
) -> Result<(Vec<SftpEntry>, bool)> {
    let sftp = pool.client(server).await?;
    let mut fs = sftp.fs();
    let mut entries = Vec::new();
    let mut truncated = false;
    let dir = match fs.open_dir(path).await {
        Ok(dir) => dir,
        Err(err) => {
            pool.invalidate().await;
            return Err(err).context("SFTP opendir failed");
        }
    };
    let rd = dir.read_dir();
    futures_util::pin_mut!(rd);
    while let Some(item) = rd.next().await {
        if entries.len() >= MAX_LIST_ENTRIES {
            truncated = true;
            break;
        }
        let entry = match item {
            Ok(entry) => entry,
            Err(err) => {
                pool.invalidate().await;
                return Err(anyhow::Error::from(err)).context("SFTP readdir failed");
            }
        };
        if entry.filename() == Path::new(".") || entry.filename() == Path::new("..") {
            continue;
        }
        let name = entry.filename().to_string_lossy().into_owned();
        let md = entry.metadata();
        let is_link = md.file_type().map(|t| t.is_symlink()).unwrap_or(false);
        let mut mode = entry_mode(&md);
        let mut size = md.len().unwrap_or(0);
        let mut modified = md.modified().map(|t| t.into_raw() as u64);
        // Symlinks are shown as their target (like the local listing): flag
        // them (`is_link`) but resolve type/size/time via a follow-up stat so
        // they render as a normal entry.
        if is_link {
            let target = format!("{path}/{name}");
            if let Ok(tmd) = fs.metadata(&target).await {
                mode = entry_mode(&tmd);
                size = tmd.len().unwrap_or(size);
                modified = tmd.modified().map(|t| t.into_raw() as u64);
            }
        }
        let is_dir = md.file_type().map(|t| t.is_dir()).unwrap_or(false);
        entries.push(SftpEntry {
            name,
            is_dir,
            is_link,
            size,
            modified,
            // SFTP v3 exposes no creation time.
            created: None,
            mode,
        });
    }
    Ok((entries, truncated))
}

/// Read a file from a remote SSH shell into memory.
pub async fn read_remote(pool: &SftpPool, server: &ServerConfig, path: &str) -> Result<Bytes> {
    with_remote(pool, server, "SFTP read failed", |sftp| async move {
        let mut fs = sftp.fs();
        let data = fs.read(path).await?;
        Ok::<Bytes, openssh_sftp_client::error::Error>(Bytes::from(data))
    })
    .await
}

/// Write a file on a remote SSH shell.
pub async fn write_remote(
    pool: &SftpPool,
    server: &ServerConfig,
    path: &str,
    data: &[u8],
) -> Result<()> {
    with_remote(pool, server, "SFTP write failed", |sftp| async move {
        let mut fs = sftp.fs();
        fs.write(path, data).await
    })
    .await
}

/// Write bytes at an offset of a remote file, creating it on offset 0.
///
/// Used for chunked uploads; `offset == 0` truncates any existing file and
/// ensures the parent directory chain exists (so folder uploads work without
/// separate mkdir calls).
pub async fn write_at_remote(
    pool: &SftpPool,
    server: &ServerConfig,
    path: &str,
    offset: u64,
    data: &[u8],
) -> Result<()> {
    with_remote(pool, server, "SFTP write-at failed", |sftp| async move {
        // The annotation pins the mixed anyhow/openssh-sftp error types so
        // `?` unifies on `anyhow::Error`.
        let result: Result<()> = async {
            let mut fs = sftp.fs();
            if offset == 0 {
                ensure_parent_remote(&mut fs, path).await?;
            }
            let mut opts = sftp.options();
            opts.write(true).create(true).truncate(offset == 0);
            let mut file = opts.open(path).await?;
            use tokio::io::AsyncSeekExt;
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(openssh_sftp_client::error::Error::IOError)?;
            file.write_all(data).await?;
            file.close().await?;
            Ok(())
        }
        .await;
        result
    })
    .await
}

/// Ensure the parent directory chain of `path` exists on the remote server.
///
/// Creates each missing ancestor with a single `mkdir` per level, so chunked
/// uploads into not-yet-existing sub-directories (folder uploads) succeed.
/// The existence check tolerates a concurrent creator (an already-existing
/// directory is treated as success, not an error).
async fn ensure_parent_remote(fs: &mut Fs, path: &str) -> Result<()> {
    use std::path::Component;
    let parent = Path::new(path).parent();
    let Some(parent) = parent else { return Ok(()) };

    let mut current = String::new();
    for comp in parent.components() {
        match comp {
            Component::RootDir => current = "/".to_string(),
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if current.is_empty() || current == "/" {
                    current = format!("/{name}");
                } else {
                    current = format!("{current}/{name}");
                }
                if let Ok(meta) = fs.metadata(&current).await {
                    if !meta.file_type().map(|t| t.is_dir()).unwrap_or(true) {
                        tracing::warn!(dir = %current, "upload parent is not a directory");
                        return Err(anyhow::anyhow!(
                            "上传目标 {current} 已存在但不是目录（可能是同名文件），请先处理"
                        ));
                    }
                    tracing::debug!(dir = %current, "dir already exists");
                } else {
                    match fs.create_dir(&current).await {
                        Ok(()) => {
                            tracing::debug!(dir = %current, "created dir for upload");
                        }
                        // Another request may have created it between the
                        // check and the mkdir (parallel folder drops).
                        Err(_) if fs.metadata(&current).await.is_ok() => {
                            tracing::debug!(dir = %current, "dir appeared concurrently");
                        }
                        Err(err) => {
                            tracing::warn!(dir = %current, ?err, "mkdir for upload failed");
                            return Err(anyhow::anyhow!("SFTP mkdir failed: {current}: {err}"));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Create a directory on a remote SSH shell.
pub async fn mkdir_remote(pool: &SftpPool, server: &ServerConfig, path: &str) -> Result<()> {
    with_remote(pool, server, "SFTP mkdir failed", |sftp| async move {
        let mut fs = sftp.fs();
        fs.create_dir(path).await
    })
    .await
}

/// Remove a file or directory on a remote SSH shell.
pub async fn remove_remote(
    pool: &SftpPool,
    server: &ServerConfig,
    path: &str,
    is_dir: bool,
) -> Result<()> {
    with_remote(pool, server, "SFTP remove failed", |sftp| async move {
        let result: Result<()> = async {
            if is_dir {
                // Recursive delete: an SFTP `rmdir` only removes empty
                // directories, so walk the tree depth-first (post-order) —
                // delete children before the directory itself. Symlinks are
                // skipped (never followed) and recursion is depth-capped,
                // mirroring the archive/copy walkers (see 已知坑 15/17).
                remove_tree(&sftp, path, 0).await
            } else {
                let mut fs = sftp.fs();
                fs.remove_file(path)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))
            }
        }
        .await;
        result
    })
    .await
}

/// Recursively delete a remote directory tree: every entry under `path`,
/// then `path` itself. Symlinks are removed (not followed); depth is capped
/// so a cyclic or pathologically deep tree can't run forever.
///
/// Children are deleted **in parallel** (`join_all`): openssh-sftp-client
/// processes concurrent requests on its internal queue, so a wide directory
/// deletes far faster than one round-trip per entry (a 400-file dir exceeded
/// the 30s hard timeout when serial). `Sftp` is `Send + Sync`, each child
/// future owns its own `Fs`, so the concurrent futures are safe.
async fn remove_tree(sftp: &Sftp, path: &str, depth: u32) -> Result<()> {
    use futures_util::StreamExt;
    if depth > 32 {
        return Ok(());
    }
    let mut fs = sftp.fs();
    let meta = fs
        .symlink_metadata(path)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if !meta.file_type().map(|t| t.is_dir()).unwrap_or(false) {
        // A file (or a symlink to one): remove it directly.
        fs.remove_file(path)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        return Ok(());
    }
    // Directory: descend first (deleting each child), then remove the dir.
    let dir = fs
        .open_dir(path)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let rd = dir.read_dir();
    futures_util::pin_mut!(rd);
    let mut children = Vec::new();
    while let Some(item) = rd.next().await {
        let entry = item.map_err(|e| anyhow::anyhow!("{e}"))?;
        let name = entry.filename().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        children.push(format!("{path}/{name}"));
    }
    // Delete children in parallel; each owns its own Fs. `remove_tree` borrows
    // `child`, so wrap each in a future that owns the path string.
    futures_util::future::join_all(children.into_iter().map(|child| {
        let path = child.clone();
        Box::pin(async move { remove_tree(sftp, &path, depth + 1).await })
    }))
    .await
    .into_iter()
    .collect::<Result<Vec<_>>>()?;
    fs.remove_dir(path)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

/// Rename a file or directory on a remote SSH shell.
pub async fn rename_remote(
    pool: &SftpPool,
    server: &ServerConfig,
    from: &str,
    to: &str,
) -> Result<()> {
    with_remote(pool, server, "SFTP rename failed", |sftp| async move {
        let mut fs = sftp.fs();
        fs.rename(from, to).await
    })
    .await
}

/// Whether `path` is a symbolic link (mode type bits `MODE_LINK`).
pub(crate) async fn remote_is_link(client: &Sftp, path: &str) -> Result<bool> {
    let mut fs = client.fs();
    let meta = fs.symlink_metadata(path).await?;
    Ok(meta.file_type().map(|t| t.is_symlink()).unwrap_or(false))
}

/// Metadata snapshot for one remote SFTP node, gathered once by the walker
/// and handed to visitors so file/dir handling doesn't re-stat.
pub(crate) struct RemoteNode {
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
}

/// Visitor for [`walk_remote`]: one `visit` call per node, awaited before the
/// walker descends. Implementations own whatever per-node state they need (a
/// ZIP writer, a copy destination…). Must be `Send` so the walk runs inside a
/// `tokio::spawn` task (see 已知坑 12).
pub(crate) trait RemoteWalker: Send {
    fn visit<'a>(
        &'a mut self,
        client: &'a Sftp,
        fs_path: &'a str,
        node: &'a RemoteNode,
    ) -> futures_util::future::BoxFuture<'a, Result<()>>;
}

/// Recursively visit every entry under `fs_path` (depth-capped, symlinks
/// skipped — see 已知坑 15/17). `visitor.visit` runs for each visited node,
/// then directories are descended into depth-first.
pub(crate) async fn walk_remote(
    client: &Sftp,
    fs_path: &str,
    depth: u32,
    visitor: &mut dyn RemoteWalker,
) -> Result<()> {
    if depth > 32 || remote_is_link(client, fs_path).await? {
        return Ok(());
    }
    let mut fs = client.fs();
    let meta = fs.symlink_metadata(fs_path).await?;
    let node = RemoteNode {
        is_dir: meta.file_type().map(|t| t.is_dir()).unwrap_or(false),
        size: meta.len().unwrap_or(0),
        modified: meta.modified().map(|t| t.into_raw() as u64),
    };
    visitor.visit(client, fs_path, &node).await?;
    if node.is_dir {
        let dir = fs.open_dir(fs_path).await?;
        let rd = dir.read_dir();
        futures_util::pin_mut!(rd);
        while let Some(item) = rd.next().await {
            let entry = item?;
            let name = entry.filename().to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            Box::pin(walk_remote(
                client,
                &format!("{fs_path}/{name}"),
                depth + 1,
                visitor,
            ))
            .await?;
        }
    }
    Ok(())
}

/// Copy a file or directory (recursively) on a remote SSH shell.
///
/// Symbolic links are skipped and recursion is depth-limited (see
/// [`walk_remote`]), so directory cycles can't hang the copy.
pub async fn copy_remote(
    pool: &SftpPool,
    server: &ServerConfig,
    from: &str,
    to: &str,
) -> Result<()> {
    struct CopyVisitor<'a> {
        src: &'a str,
        dst: &'a str,
    }
    impl RemoteWalker for CopyVisitor<'_> {
        fn visit<'a>(
            &'a mut self,
            client: &'a Sftp,
            fs_path: &'a str,
            node: &'a RemoteNode,
        ) -> futures_util::future::BoxFuture<'a, Result<()>> {
            // `dst` is the target directory when copying a tree root, or the
            // target file when copying a single file; children map 1:1 by path.
            let dst = if fs_path == self.src {
                self.dst.to_string()
            } else {
                format!(
                    "{}/{}",
                    self.dst,
                    fs_path.strip_prefix(self.src).unwrap_or(fs_path)
                )
            };
            Box::pin(async move {
                let mut fs = client.fs();
                if node.is_dir {
                    fs.create_dir(&dst).await?;
                } else {
                    let data = fs.read(fs_path).await?;
                    fs.write(&dst, &data).await?;
                }
                Ok(())
            })
        }
    }

    let client = pool.client(server).await?;
    let mut visitor = CopyVisitor { src: from, dst: to };
    if let Err(err) = walk_remote(&client, from, 0, &mut visitor).await {
        pool.invalidate().await;
        return Err(err);
    }
    Ok(())
}
