//! SFTP orchestration for a [`Session`]: remote shells go through the SFTP
//! pool (openssh-sftp-client + hard timeout), local shells run blocking
//! `std::fs` operations on a background thread (see 已知坑 11).

use std::future::Future;

use anyhow::Result;
use bytes::Bytes;
use sshweb_core::Sid;

use super::Session;
use crate::sftp;
use crate::sftp::SftpPool;
use crate::web::protocol::{ServerConfig, WsServer};

/// An ordered-file write operation (chunked uploads + whole-file saves),
/// applied strictly in FIFO order by the single worker spawned with the
/// session — see `Session::new`. **Unbounded**: a bounded channel with
/// `try_send` silently dropped chunks, producing incomplete files the client
/// believed to be fully uploaded (已知坑 19).
pub(crate) enum WriteOp {
    /// Chunked upload write (`offset == 0` truncates/creates).
    WriteAt(Sid, String, u64, Bytes),
    /// Whole-file write (editor save / new-file creation).
    Write(Sid, String, Bytes),
}

impl WriteOp {
    /// The affected path (used for per-path error reports).
    pub(crate) fn path(&self) -> &str {
        match self {
            WriteOp::WriteAt(_, path, ..) | WriteOp::Write(_, path, _) => path,
        }
    }
}

impl Session {
    /// Open the file manager for a shell, resolving the initial path for the
    /// terminal's server. The SFTP identity is always the **configured** user
    /// (startup snippets apply to the terminal only and are ignored here): if
    /// the configured identity can open a real SFTP session, browsing starts
    /// at the terminal's known directory (or its login home); otherwise the
    /// user is told and the view falls back to "/".
    ///
    /// The connect, probe (login dir + user) and SFTP-permission check all
    /// happen over a **single** SSH connection (`SftpPool::client_probe`), so
    /// the first open costs one handshake instead of three.
    ///
    /// The **browse target** is a headless SFTP shell deduplicated per server
    /// (host/port/user, [`Session::connect_sftp_shell`]), so all terminals of
    /// one server share a single SFTP view — the file list stays bound to the
    /// server, not to whichever terminal was active when it was opened. The
    /// returned `SftpOpenResult` announces that headless sid; the initial
    /// directory still follows this terminal's known cwd (or its login home)
    /// on the first open, and the frontend keeps it thereafter.
    pub async fn sftp_open(&self, id: Sid) {
        let Some((server, pool)) = self.shell_target(id) else {
            // Local shell: browse the server's own filesystem.
            let user = std::env::var("USER").unwrap_or_else(|_| "unknown".into());
            let cwd = Self::run_local(
                || {
                    Ok(std::env::current_dir()
                        .ok()
                        .and_then(|p| p.to_str().map(|s| s.to_string()))
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "/".into()))
                },
                "cwd",
            )
            .await
            .unwrap_or_else(|_| "/".into());
            self.send(WsServer::SftpOpenResult(id, cwd, user, None));
            return;
        };

        // Configured identity: connect, probe and prove SFTP permission in one
        // connection, then open at the terminal's known directory or its
        // login home. The browse sid is the per-server headless shell (all
        // terminals of this server share it); `id` still names the terminal
        // whose known cwd seeds the initial directory.
        let browse_sid = self.connect_sftp_shell(server.clone());
        self.probe_and_report(browse_sid, id, &server, &pool, true, true)
            .await;
    }

    /// Probe a headless SFTP shell's login directory and user over its own
    /// pool connection (created lazily; cached for subsequent opens), then
    /// announce the browse start to the client. Failures are reported through
    /// the `notice` field so the client always gets a final result toast
    /// (success is signalled by a clean probe, i.e. no notice).
    pub async fn sftp_probe(&self, id: Sid) {
        let Some((server, pool)) = self.shell_target(id) else {
            return;
        };
        self.probe_and_report(id, id, &server, &pool, false, false)
            .await;
    }

    /// Connect + probe an SFTP target and announce the browse result, sharing
    /// the connect/probe/permission handshake and the failure reporting
    /// between [`Self::sftp_open`] (terminal shells) and [`Self::sftp_probe`]
    /// (headless shells).
    ///
    /// `browse_sid` is the sid announced to the client (the per-server headless
    /// shell for terminal opens); `term_id` is the terminal whose known cwd and
    /// detected `su` user seed the initial directory when `follow_terminal` is
    /// set (headless probes pass both as the same id). `headless_retry` opens a
    /// headless retry shell when the server is unreachable, so the client has a
    /// sid to browse once it comes back up.
    async fn probe_and_report(
        &self,
        browse_sid: Sid,
        term_id: Sid,
        server: &ServerConfig,
        pool: &SftpPool,
        follow_terminal: bool,
        headless_retry: bool,
    ) {
        let (shell_cwd, detected_su) = if follow_terminal {
            (self.shell_cwd(term_id), self.shell_detected_su(term_id))
        } else {
            (None, None)
        };
        match Self::remote_timeout(pool, pool.client_probe(server)).await {
            Ok((_client, home, user)) => {
                let (dir, notice) = if follow_terminal {
                    let dir = shell_cwd
                        .as_deref()
                        .filter(|s| !s.is_empty())
                        .map(|s| s.to_string())
                        .unwrap_or(home);
                    // The terminal switched to another user (su/sudo), but that
                    // identity cannot be reused for SFTP: tell the user instead
                    // of silently browsing as the configured user.
                    let notice = detected_su
                        .filter(|u| !u.is_empty() && u != &user)
                        .map(|u| {
                            format!("当前终端用户（{u}）没有可用的 SFTP 权限，已按配置用户目录打开")
                        });
                    (dir, notice)
                } else {
                    (home, None)
                };
                self.send(WsServer::SftpOpenResult(browse_sid, dir, user, notice));
            }
            Err(err) => self.send_probe_failure(browse_sid, err, server, headless_retry),
        }
    }

    /// Report a failed SFTP connect+probe through the `sftpOpenResult` notice.
    /// `browse_sid` is the sid announced to the client (the per-server headless
    /// shell for terminal opens). `NoSftp` means SSH worked but the SFTP
    /// subsystem is unavailable; `Connect` means the server could not be
    /// reached — optionally opening a headless retry shell first, as
    /// `sftp_open` does, so the client has a sid to browse once it comes back
    /// up.
    fn send_probe_failure(
        &self,
        browse_sid: Sid,
        err: sftp::SftpConnectError,
        server: &ServerConfig,
        headless_retry: bool,
    ) {
        match err {
            sftp::SftpConnectError::NoSftp { user, .. } => {
                // SSH worked but the SFTP subsystem is denied / missing.
                self.send(WsServer::SftpOpenResult(
                    browse_sid,
                    "/".into(),
                    String::new(),
                    Some(format!(
                        "配置用户（{user}）没有可用的 SFTP 权限，请检查 sshd 的 SFTP 子系统"
                    )),
                ));
            }
            sftp::SftpConnectError::Connect(err) => {
                tracing::warn!(%browse_sid, ?err, "remote sftp probe failed");
                // Failed to reach the server at all: headless retry at root.
                let browse_sid = if headless_retry {
                    self.connect_sftp_shell(server.clone())
                } else {
                    browse_sid
                };
                self.send(WsServer::SftpOpenResult(
                    browse_sid,
                    "/".into(),
                    String::new(),
                    Some("无法连接该服务器的 SFTP 服务，请稍后重试".to_string()),
                ));
            }
        }
    }

    /// List a directory for a shell. Remote shells use SFTP; local shells use
    /// the server filesystem.
    pub async fn sftp_list(&self, id: Sid, path: String) -> Result<()> {
        let path_for_send = path.clone();
        let (entries, truncated) = self
            .run_path_ret(
                id,
                &path_for_send,
                "list task",
                |pool, server, rp| async move { sftp::list_remote(&pool, &server, &rp).await },
                |lp| sftp::list_local(&lp),
            )
            .await?;
        self.send(WsServer::SftpList(id, path_for_send, entries, truncated));
        Ok(())
    }

    /// Read a file for a shell.
    pub async fn sftp_read(&self, id: Sid, path: String) -> Result<()> {
        let path_for_send = path.clone();
        let data = self.read_file(id, &path).await?;
        self.send(WsServer::SftpData(id, path_for_send, data));
        Ok(())
    }

    /// Read a whole file for a shell (remote SFTP or local fs).
    async fn read_file(&self, id: Sid, path: &str) -> Result<Bytes> {
        self.run_path_ret(
            id,
            path,
            "read task",
            |pool, server, rp| async move { sftp::read_remote(&pool, &server, &rp).await },
            |lp| sftp::read_local(&lp),
        )
        .await
    }

    /// Size of a file for a shell (used by the HTTP Range download endpoint).
    pub async fn download_size(&self, id: Sid, path: &str) -> Result<u64> {
        self.run_path_ret(
            id,
            path,
            "stat task",
            |pool, server, rp| async move { sftp::size_remote(&pool, &server, &rp).await },
            |lp| sftp::size_local(&lp),
        )
        .await
    }

    /// Open a streaming reader for a file at `offset` (HTTP Range download).
    pub async fn download_reader(
        &self,
        id: Sid,
        path: &str,
        offset: u64,
    ) -> Result<sftp::DownloadReader> {
        let path = path.to_owned();
        match self.shell_target(id) {
            Some((server, pool)) => {
                Self::remote_timeout(&pool, sftp::reader_remote(&pool, &server, &path, offset))
                    .await
            }
            None => sftp::reader_local(&path, offset).await,
        }
    }

    /// Run a remote SFTP operation under a hard timeout.
    ///
    /// Without a timeout, a single stalled SFTP request (e.g. a dead-but-not-
    /// detected SSH tunnel over a jump host) makes every later operation for
    /// the shell queue behind it forever — the file manager appears frozen.
    /// On timeout the cached connection is dropped so the next operation
    /// reconnects fresh.
    async fn remote_timeout<T, E>(
        pool: &sftp::SftpPool,
        fut: impl std::future::Future<Output = std::result::Result<T, E>>,
    ) -> std::result::Result<T, E>
    where
        E: From<anyhow::Error>,
    {
        // A single SFTP request (including a jump-host round trip) should not
        // make the whole file manager appear frozen for two minutes. The
        // transport connect timeout is 25s; 30s leaves a small margin while
        // still recovering promptly from a dead tunnel or stalled server.
        const SFTP_OP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
        match tokio::time::timeout(SFTP_OP_TIMEOUT, fut).await {
            Ok(result) => result,
            Err(_) => {
                pool.invalidate().await;
                tracing::warn!("remote SFTP operation timed out");
                Err(anyhow::anyhow!("SFTP 操作超时（{SFTP_OP_TIMEOUT:?}），已重新连接").into())
            }
        }
    }

    /// Dispatch an SFTP operation to the remote pool (with a hard timeout) or
    /// to a blocking local task, depending on whether the shell is remote or
    /// local, and return its result.
    ///
    /// `remote` runs against the shell's pool / server config; `local` runs on
    /// a background thread (已知坑 11). This collapses the repeated
    /// `match shell_target` + `remote_timeout`/`run_local` boilerplate across
    /// every file operation.
    async fn run_target<T, R, Fut, L>(
        &self,
        id: Sid,
        remote: R,
        local: L,
        label: &'static str,
    ) -> Result<T>
    where
        T: Send + 'static,
        R: FnOnce(SftpPool, ServerConfig) -> Fut + Send,
        Fut: Future<Output = Result<T>> + Send,
        L: FnOnce() -> Result<T> + Send + 'static,
    {
        match self.shell_target(id) {
            Some((server, pool)) => {
                let fut = remote(pool.clone(), server);
                Self::remote_timeout(&pool, fut).await
            }
            None => Self::run_local(local, label).await,
        }
    }

    /// [`Self::run_target`] followed by an `SftpOk` acknowledgement — the
    /// common shape of the write/mkdir/remove/rename/copy operations.
    async fn run_and_ack<T, R, Fut, L>(
        &self,
        id: Sid,
        remote: R,
        local: L,
        label: &'static str,
        ack_path: String,
        ack_offset: Option<u64>,
    ) -> Result<()>
    where
        T: Send + 'static,
        R: FnOnce(SftpPool, ServerConfig) -> Fut + Send,
        Fut: Future<Output = Result<T>> + Send,
        L: FnOnce() -> Result<T> + Send + 'static,
    {
        self.run_target(id, remote, local, label).await?;
        match ack_offset {
            Some(offset) => self.send(WsServer::SftpWriteOk(id, ack_path, offset)),
            None => self.send(WsServer::SftpOk(id, ack_path)),
        }
        Ok(())
    }

    /// Single-path wrapper over [`Self::run_and_ack`]: clones `path` once
    /// internally for the remote/local closures and the acknowledgement, so
    /// the write/mkdir/remove/write-at helpers don't repeat the clone dance.
    async fn run_path_op<T, R, Fut, L>(
        &self,
        id: Sid,
        path: String,
        label: &'static str,
        remote: R,
        local: L,
        ack_offset: Option<u64>,
    ) -> Result<()>
    where
        T: Send + 'static,
        R: FnOnce(SftpPool, ServerConfig, String) -> Fut + Send,
        Fut: Future<Output = Result<T>> + Send,
        L: FnOnce(String) -> Result<T> + Send + 'static,
    {
        let ack_path = path.clone();
        let rp = path.clone();
        let lp = path;
        self.run_and_ack(
            id,
            move |pool, server| remote(pool, server, rp),
            move || local(lp),
            label,
            ack_path,
            ack_offset,
        )
        .await
    }

    /// Two-path wrapper over [`Self::run_and_ack`] for rename/copy, cloning
    /// both paths once internally.
    async fn run_pair_op<T, R, Fut, L>(
        &self,
        id: Sid,
        from: String,
        to: String,
        label: &'static str,
        remote: R,
        local: L,
    ) -> Result<()>
    where
        T: Send + 'static,
        R: FnOnce(SftpPool, ServerConfig, String, String) -> Fut + Send,
        Fut: Future<Output = Result<T>> + Send,
        L: FnOnce(String, String) -> Result<T> + Send + 'static,
    {
        let ack_path = from.clone();
        let rf = from.clone();
        let lf = from;
        let rt = to.clone();
        let lt = to;
        self.run_and_ack(
            id,
            move |pool, server| remote(pool, server, rf, rt),
            move || local(lf, lt),
            label,
            ack_path,
            None,
        )
        .await
    }

    /// Single-path wrapper over [`Self::run_target`] that returns the value
    /// instead of sending an `SftpOk` — the shape of the list/read/stat
    /// operations that send their own reply. Clones `path` once internally for
    /// the remote/local closures, removing the repeated `rp`/`lp` dance.
    async fn run_path_ret<T, R, Fut, L>(
        &self,
        id: Sid,
        path: &str,
        label: &'static str,
        remote: R,
        local: L,
    ) -> Result<T>
    where
        T: Send + 'static,
        R: FnOnce(SftpPool, ServerConfig, String) -> Fut + Send,
        Fut: Future<Output = Result<T>> + Send,
        L: FnOnce(String) -> Result<T> + Send + 'static,
    {
        let rp = path.to_owned();
        let lp = path.to_owned();
        self.run_target(
            id,
            move |pool, server| remote(pool, server, rp),
            move || local(lp),
            label,
        )
        .await
    }

    /// Enqueue a chunked-upload write-at operation (FIFO ordered, see
    /// [`WriteOp`] and 已知坑 19). Bounded: when the FIFO is full the write is
    /// rejected with an error instead of being buffered without bound (安全审查
    /// M4) — a legitimate client paces one chunk at a time and never fills it.
    pub fn enqueue_write_at(&self, id: Sid, path: String, offset: u64, data: Bytes) {
        let path_for_err = path.clone();
        if self
            .write_queue
            .try_send(WriteOp::WriteAt(id, path, offset, data))
            .is_err()
            && !self.write_queue.is_closed()
        {
            self.error(format!("写入队列已满，请稍后再试（{path_for_err}）"));
        }
    }

    /// Enqueue a whole-file write (editor save, new-file creation), sharing
    /// the same FIFO as chunked uploads so save-vs-upload order is preserved.
    pub fn enqueue_write(&self, id: Sid, path: String, data: Bytes) {
        let path_for_err = path.clone();
        if self
            .write_queue
            .try_send(WriteOp::Write(id, path, data))
            .is_err()
            && !self.write_queue.is_closed()
        {
            self.error(format!("写入队列已满，请稍后再试（{path_for_err}）"));
        }
    }

    /// Apply a single write-at operation and notify the client on success.
    pub(crate) async fn sftp_write_at_impl(
        &self,
        id: Sid,
        path: String,
        offset: u64,
        data: Bytes,
    ) -> Result<()> {
        let ld = data.clone();
        self.run_path_op(
            id,
            path,
            "write task",
            move |pool, server, rp| async move {
                sftp::write_at_remote(&pool, &server, &rp, offset, &data).await
            },
            move |lp| sftp::write_at_local(&lp, offset, &ld),
            Some(offset),
        )
        .await
    }

    /// Apply a whole-file write and notify the client on success.
    pub(crate) async fn sftp_write(&self, id: Sid, path: String, data: Bytes) -> Result<()> {
        let ld = data.clone();
        self.run_path_op(
            id,
            path,
            "write task",
            move |pool, server, rp| async move {
                sftp::write_remote(&pool, &server, &rp, &data).await
            },
            move |lp| sftp::write_local(&lp, &ld),
            None,
        )
        .await
    }

    /// Create a directory for a shell.
    pub async fn sftp_mkdir(&self, id: Sid, path: String) -> Result<()> {
        self.run_path_op(
            id,
            path,
            "mkdir task",
            move |pool, server, rp| async move { sftp::mkdir_remote(&pool, &server, &rp).await },
            move |lp| sftp::mkdir_local(&lp),
            None,
        )
        .await
    }

    /// Remove a file or directory for a shell.
    pub async fn sftp_remove(&self, id: Sid, path: String, is_dir: bool) -> Result<()> {
        self.run_path_op(
            id,
            path,
            "remove task",
            move |pool, server, rp| async move {
                sftp::remove_remote(&pool, &server, &rp, is_dir).await
            },
            move |lp| sftp::remove_local(&lp, is_dir),
            None,
        )
        .await
    }

    /// Rename a file or directory for a shell.
    pub async fn sftp_rename(&self, id: Sid, from: String, to: String) -> Result<()> {
        self.run_pair_op(
            id,
            from,
            to,
            "rename task",
            move |pool, server, rf, rt| async move {
                sftp::rename_remote(&pool, &server, &rf, &rt).await
            },
            move |lf, lt| sftp::rename_local(&lf, &lt),
        )
        .await
    }

    /// Copy a file or directory for a shell.
    pub async fn sftp_copy(&self, id: Sid, from: String, to: String) -> Result<()> {
        self.run_pair_op(
            id,
            from,
            to,
            "copy task",
            move |pool, server, rf, rt| async move {
                sftp::copy_remote(&pool, &server, &rf, &rt).await
            },
            move |lf, lt| sftp::copy_local(&lf, &lt),
        )
        .await
    }

    /// Stream a ZIP archive for a shell straight to an HTTP response body.
    ///
    /// The archive is generated lazily on a background task and pushed out in
    /// 64 KiB chunks as it is being created — no temp file, no whole-archive
    /// buffering in memory. Local filesystem walking runs inside
    /// `spawn_blocking`; remote SFTP reads are chunked (256 KiB) so the runtime
    /// never blocks on a huge read. `flat` unwraps a single folder's own name
    /// (its contents become the top-level entries).
    pub fn sftp_archive_stream(
        &self,
        id: Sid,
        paths: Vec<String>,
        flat: bool,
    ) -> Result<impl futures_util::Stream<Item = Result<Bytes, anyhow::Error>>, anyhow::Error> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        match self.shell_target(id) {
            Some((server, pool)) => {
                let err_tx = tx.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        sftp::archive_remote_stream(&pool, &server, &paths, flat, tx).await
                    {
                        let _ = err_tx.send(Err(err));
                    }
                });
            }
            None => {
                let task_tx = tx.clone();
                let err_tx = tx.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(err) = sftp::archive_local_stream(&paths, flat, task_tx) {
                        let _ = err_tx.send(Err(err));
                    }
                });
            }
        }
        Ok(tokio_stream::wrappers::UnboundedReceiverStream::new(rx))
    }
}
