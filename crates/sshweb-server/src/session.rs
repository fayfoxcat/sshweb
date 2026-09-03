//! Core logic for a single sshweb session, which directly owns its terminal
//! processes.
//!
//! Each incoming WebSocket connection attaches to a [`Session`] identified by
//! a stable key (the URL path). Sessions survive connection drops so a browser
//! refresh reconnects to the same terminal processes; a session with no
//! attached clients is reclaimed after an idle timeout (see
//! `ServerState::reclaim_idle`).
//!
//! Shell lifecycle, input routing and stats live here; SFTP orchestration is
//! in [`sftp_ops`] (remote = openssh-sftp-client pool + hard timeout, local =
//! blocking `std::fs` on a background thread).

mod sftp_ops;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Result};
use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use sftp_ops::WriteOp;
use sshweb_core::{IdCounter, Sid};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::config::ConfigStore;
use crate::runner::{shell_task, ssh_task, ShellData};
use crate::sftp::SftpPool;
use crate::utils::Shutdown;
use crate::web::protocol::{ServerConfig, WsServer, WsWinsize};

/// Per-connection outbound queue capacity (坑 13: bounded, non-blocking sends).
const SUBSCRIBER_CAPACITY: usize = 512;
/// Terminal output retained per shell for replay after a reconnect. Oldest
/// bytes are dropped past this bound.
const TERMINAL_BUFFER_BYTES: usize = 1 << 20; // 1 MiB
/// Bounded capacity of the per-session file-write FIFO (安全审查 M4). A
/// malicious client spamming `sftpWriteAt`/`sftpWrite` can no longer grow
/// memory without bound; when the queue is full the write is rejected with an
/// error instead of being silently dropped (known pitfall 19).
const WRITE_QUEUE_CAP: usize = 256;
/// Server-side cap on open shells per session (安全审查 M4). The frontend has
/// its own lower `MAX_TERMINALS`; this is the hard limit so a malicious client
/// cannot spawn unbounded local PTY processes.
const MAX_SHELLS_PER_SESSION: usize = 64;

/// Split a terminal ring buffer into ≤64 KiB chunks for replay, so a single
/// 1 MiB message doesn't block the client's renderer for long.
fn split_buffer(ring: &VecDeque<u8>) -> Vec<bytes::Bytes> {
    const CHUNK: usize = 64 * 1024;
    let mut chunks = Vec::new();
    let mut iter = ring.iter();
    loop {
        let take = iter.len().min(CHUNK);
        if take == 0 {
            break;
        }
        let buf: Vec<u8> = iter.by_ref().take(take).copied().collect();
        chunks.push(bytes::Bytes::from(buf));
    }
    chunks
}

/// Per-shell in-memory state, holding the channel used to send input to the
/// shell task. Kept in an ordered vector so the frontend z-order matches the
/// order in which shells were created or brought to front.
struct Shell {
    input_tx: mpsc::Sender<ShellData>,
    winsize: WsWinsize,
    /// Remote SSH server config, or `None` for a local shell.
    server: Option<ServerConfig>,
    /// Reusable SFTP connection pool for remote shells.
    sftp: SftpPool,
    /// True for headless SFTP-only shells (no terminal tab, no runner task).
    hidden: bool,
    /// Latest remote host stats sampled by the stats collector task.
    stats: crate::stats::HostStats,
    /// Initial working directory this shell was created with (if any), used as
    /// the SFTP browse start when the same user identity is in use.
    cwd: Option<String>,
    /// User the terminal switched to via an interactive `su`/`sudo` seen in
    /// the terminal output (`su - root` + `Password:`). Detection only — the
    /// switched user can't be reused for a new connection.
    su_detected: Option<String>,
    /// Tab's base display title, persisted so a reconnecting client restores
    /// it (empty for shells the server spawned itself).
    label: String,
    /// Retained terminal output for replay on reconnect (oldest dropped over
    /// the bound).
    ring: VecDeque<u8>,
    /// PID of a local shell's child process, used to read its live cwd via
    /// `/proc/<pid>/cwd` (None for remote/headless shells).
    pid: Option<i32>,
}

impl Shell {
    /// Construct a shell entry with the freshly-created defaults (default
    /// winsize, empty stats, no detected `su`).
    fn new(
        input_tx: mpsc::Sender<ShellData>,
        server: Option<ServerConfig>,
        sftp: SftpPool,
        hidden: bool,
        cwd: Option<String>,
        label: String,
    ) -> Self {
        Self {
            input_tx,
            winsize: WsWinsize::default(),
            server,
            sftp,
            hidden,
            stats: crate::stats::HostStats::default(),
            cwd,
            su_detected: None,
            label,
            ring: VecDeque::new(),
            pid: None,
        }
    }
}

/// Whether two server configs target the same SSH host (host / port / user);
/// used to share SFTP pools and reuse headless shells.
fn same_target(cfg: &ServerConfig, other: &ServerConfig) -> bool {
    cfg.host == other.host && cfg.port == other.port && cfg.username == other.username
}

/// Redact every SSH credential from a config before it is replayed to clients
/// over the session attach (安全审查 M6). The browser already holds the
/// passwords it submitted (its own `connections` store) and re-merges them by
/// target; sending them back would hand every session-attached client the
/// stored passwords for free.
fn redact_credentials(server: &ServerConfig) -> ServerConfig {
    let mut s = server.clone();
    s.password.clear();
    s.private_key = None;
    for host in &mut s.hosts {
        host.password.clear();
        host.private_key = None;
    }
    if let Some(proxy) = &mut s.proxy {
        proxy.password.clear();
    }
    s
}

/// Which server the nav bar stats for a shell should come from.
pub enum ShellStats {
    /// Plain `/api/stats` (the machine running sshweb-server): local shells.
    Machine,
    /// Sampled on the remote SSH host, once available.
    Remote(crate::stats::HostStats),
    /// No such shell.
    Unknown,
}

/// In-memory state for a single sshweb session.
pub struct Session {
    /// Human-readable name for this session.
    name: String,

    /// Command used to launch a new terminal shell.
    shell_command: String,

    /// Encrypted config store, used to resolve saved SSH keys.
    config: Arc<ConfigStore>,

    /// Open shells, in display order (frontmost last).
    shells: RwLock<Vec<(Sid, Shell)>>,

    /// Atomic counter to get new, unique shell IDs.
    counter: IdCounter,

    /// Broadcast subscribers — one per attached WebSocket. Sending is
    /// non-blocking so terminal output cannot stall the session task
    /// (坑 13: a slow client has its messages dropped, never blocks others).
    subscribers: RwLock<Vec<(usize, mpsc::Sender<WsServer>)>>,

    /// Monotonic id handed out to each subscriber (for `detach`).
    next_sub: AtomicUsize,

    /// Number of currently attached WebSocket clients.
    clients: AtomicUsize,

    /// Instant the client count last dropped to zero (drives idle reclamation).
    idle_since: Mutex<Option<Instant>>,

    /// The active (visible) shell tab, replayed on attach so a refresh restores
    /// it.
    active: RwLock<Option<Sid>>,

    /// blocking the WebSocket message loop. **Bounded** (`WRITE_QUEUE_CAP`):
    /// when full, new writes are rejected with an error rather than silently
    /// dropped or buffered without bound (已知坑 19 + 安全审查 M4).
    write_queue: mpsc::Sender<WriteOp>,

    /// Set when this session has been closed and removed.
    shutdown: Shutdown,
}

impl Session {
    /// Look up a shell with `f` applied under a read lock.
    fn with_shell<R>(&self, id: Sid, f: impl FnOnce(&Shell) -> R) -> Option<R> {
        self.shells
            .read()
            .iter()
            .find(|(sid, _)| *sid == id)
            .map(|(_, shell)| f(shell))
    }

    /// Look up a shell with `f` applied under a write lock.
    fn with_shell_mut<R>(&self, id: Sid, f: impl FnOnce(&mut Shell) -> R) -> Option<R> {
        self.shells
            .write()
            .iter_mut()
            .find(|(sid, _)| *sid == id)
            .map(|(_, shell)| f(shell))
    }

    /// Run a blocking local filesystem task off the async runtime
    /// (`spawn_blocking`, see 已知坑 11), mapping a panicked task to a
    /// descriptive error with `label`.
    async fn run_local<T>(
        task: impl FnOnce() -> Result<T> + Send + 'static,
        label: &str,
    ) -> Result<T>
    where
        T: Send + 'static,
    {
        tokio::task::spawn_blocking(task)
            .await
            .map_err(|e| anyhow::anyhow!("{label} panicked: {e}"))?
    }

    /// Find the sid of a shell whose server targets the same host as `server`
    /// (host / port / user), optionally restricted to hidden (headless) shells.
    fn find_shell(&self, server: &ServerConfig, hidden_only: bool) -> Option<Sid> {
        self.shells
            .read()
            .iter()
            .find(|(_, shell)| {
                (!hidden_only || shell.hidden)
                    && shell
                        .server
                        .as_ref()
                        .is_some_and(|cfg| same_target(cfg, server))
            })
            .map(|(id, _)| *id)
    }

    /// Reuse the SFTP pool of an existing shell targeting the same server
    /// (host / port / user), so N terminals sharing a host share one
    /// SSH/SFTP connection (probe included). Falls back to a fresh pool.
    fn pool_for_shared(&self, server: &ServerConfig) -> SftpPool {
        self.find_shell(server, false)
            .and_then(|id| self.with_shell(id, |shell| shell.sftp.clone()))
            .unwrap_or_else(|| SftpPool::new(Arc::clone(&self.config)))
    }

    /// Allocate a new sid, register a shell entry (fresh input channel, shared
    /// or fresh SFTP pool) and return its sid plus the input receiver.
    fn insert_shell(
        &self,
        server: Option<ServerConfig>,
        hidden: bool,
        cwd: Option<String>,
        label: String,
    ) -> (Sid, mpsc::Receiver<ShellData>) {
        let id = self.counter.next_sid();
        let (input_tx, input_rx) = mpsc::channel(16);
        let sftp = match &server {
            Some(cfg) => self.pool_for_shared(cfg),
            None => SftpPool::default(),
        };
        self.shells
            .write()
            .push((id, Shell::new(input_tx, server, sftp, hidden, cwd, label)));
        (id, input_rx)
    }

    /// Construct a new (empty) session bound to the stable key `name`. The
    /// caller is responsible for spawning the initial shell (only for freshly
    /// created sessions) and attaching clients.
    pub fn new(name: String, shell_command: String, config: Arc<ConfigStore>) -> Arc<Session> {
        let (write_tx, mut write_rx) = mpsc::channel::<WriteOp>(WRITE_QUEUE_CAP);
        let session = Arc::new(Session {
            name,
            shell_command,
            config,
            shells: RwLock::new(Vec::new()),
            counter: IdCounter::default(),
            subscribers: RwLock::new(Vec::new()),
            next_sub: AtomicUsize::new(0),
            clients: AtomicUsize::new(0),
            // Start idle at creation: a session that never receives a client
            // (e.g. the upgrade failed right after creation) is still reaped.
            idle_since: Mutex::new(Some(Instant::now())),
            active: RwLock::new(None),
            write_queue: write_tx,
            shutdown: Shutdown::new(),
        });

        // Serialize file writes so chunks of the same file apply in order.
        // Also exit when the session is shut down (idle reclamation) — the
        // worker holds an `Arc<Session>`, so without the shutdown break it
        // would keep the reclaimed session (and its task) alive forever.
        {
            let worker = Arc::clone(&session);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = worker.terminated() => break,
                        op = write_rx.recv() => {
                            let Some(op) = op else { break };
                            let path = op.path().to_string();
                            let result = match op {
                                WriteOp::WriteAt(id, path, offset, data) => {
                                    worker.sftp_write_at_impl(id, path, offset, data).await
                                }
                                WriteOp::Write(id, path, data) => {
                                    worker.sftp_write(id, path, data).await
                                }
                            };
                            if let Err(err) = result {
                                worker.error(format!("写入失败（{path}）：{err:#}"));
                            }
                        }
                    }
                }
            });
        }

        session
    }

    /// Returns the name of this session.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the encrypted configuration store (host-key TOFU etc.).
    pub fn config(&self) -> Arc<ConfigStore> {
        Arc::clone(&self.config)
    }

    /// Attach a new WebSocket client, returning a subscriber id (for
    /// [`Self::detach`]) and the client's message receiver.
    ///
    /// Registers the subscriber first, then replays the session's current
    /// state in order — `Hello`, `Shells`, `ShellsMeta`, buffered output — so
    /// a reconnecting client sees exactly the live session. Registration is
    /// synchronous with the replay (no awaits), so a runner's output cannot be
    /// interleaved and lost.
    pub fn attach(&self) -> (usize, mpsc::Receiver<WsServer>) {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
        let sub_id = self.next_sub.fetch_add(1, Ordering::Relaxed);
        self.subscribers.write().push((sub_id, tx));

        self.clients.fetch_add(1, Ordering::Relaxed);
        *self.idle_since.lock() = None;

        // Replay (order matters; see the frontend's dispatch): the session key,
        // the shell list (which also creates per-shell writers/locks on the
        // client), the per-shell server configs and tab labels, the headless
        // SFTP targets, then the active tab (the last state message — the
        // client fires `onReplayComplete` on it, so everything needed to
        // restore editor tabs / the file view must already be in place),
        // then the buffered output.
        self.send(WsServer::Hello(self.name.clone()));
        self.sync_shells();
        self.send(WsServer::ShellsConfig(self.shells_config()));
        self.send(WsServer::ShellsMeta(self.shells_meta()));
        self.send(WsServer::HeadlessShells(self.headless_shells()));
        if let Some(active) = self.active() {
            self.send(WsServer::ActiveShell(active));
        }

        let buffered: Vec<(Sid, Vec<Bytes>)> = {
            let shells = self.shells.read();
            shells
                .iter()
                .filter(|(_, s)| !s.hidden)
                .filter(|(_, s)| !s.ring.is_empty())
                .map(|(id, s)| (*id, split_buffer(&s.ring)))
                .collect()
        };
        for (id, chunks) in buffered {
            self.send(WsServer::Chunks(id, chunks));
        }

        (sub_id, rx)
    }

    /// Detach a WebSocket client. When the last client leaves, record the idle
    /// timestamp so the sweeper can reclaim the session later.
    pub fn detach(&self, sub_id: usize) {
        self.subscribers.write().retain(|(id, _)| *id != sub_id);
        let remaining = self.clients.fetch_sub(1, Ordering::Relaxed) - 1;
        if remaining == 0 {
            *self.idle_since.lock() = Some(Instant::now());
        }
    }

    /// Number of currently attached WebSocket clients.
    pub fn client_count(&self) -> usize {
        self.clients.load(Ordering::Relaxed)
    }

    /// When the client count last dropped to zero (None while attached).
    pub fn idle_since(&self) -> Option<Instant> {
        *self.idle_since.lock()
    }

    /// Send a message to every attached WebSocket client.
    ///
    /// Do not wait for a slow WebSocket client from session operations.
    pub(crate) fn send(&self, msg: WsServer) {
        let subscribers = self.subscribers.read();
        if subscribers.is_empty() {
            return;
        }
        for (_, tx) in subscribers.iter() {
            if let Err(err) = tx.try_send(msg.clone()) {
                tracing::debug!(?err, "output channel is full or closed");
            }
        }
    }

    /// Send an error message to every connected WebSocket client.
    pub fn error(&self, message: String) {
        self.send(WsServer::Error(message));
    }

    /// Returns the ordered list of visible (non-hidden) shells and their
    /// window sizes. Headless SFTP shells are excluded from the tab bar.
    pub fn list_shells(&self) -> Vec<(Sid, WsWinsize)> {
        self.shells
            .read()
            .iter()
            .filter(|(_, shell)| !shell.hidden)
            .map(|(id, shell)| (*id, shell.winsize))
            .collect()
    }

    /// Per-shell display labels for visible shells, in display order.
    pub fn shells_meta(&self) -> Vec<(Sid, String)> {
        self.shells
            .read()
            .iter()
            .filter(|(_, shell)| !shell.hidden)
            .map(|(id, shell)| (*id, shell.label.clone()))
            .collect()
    }

    /// SSH credentials are redacted (安全审查 M6); the browser re-merges the
    /// passwords it submitted from its own store.
    pub fn shells_config(&self) -> Vec<(Sid, ServerConfig)> {
        self.shells
            .read()
            .iter()
            .filter(|(_, shell)| !shell.hidden)
            .filter_map(|(id, shell)| {
                shell
                    .server
                    .as_ref()
                    .map(|cfg| (*id, redact_credentials(cfg)))
            })
            .collect()
    }

    /// Server configs of headless SFTP shells, replayed on attach so the file
    /// manager's targets survive a refresh. SSH credentials are redacted.
    pub fn headless_shells(&self) -> Vec<(Sid, ServerConfig)> {
        self.shells
            .read()
            .iter()
            .filter(|(_, shell)| shell.hidden)
            .filter_map(|(id, shell)| {
                shell
                    .server
                    .as_ref()
                    .map(|cfg| (*id, redact_credentials(cfg)))
            })
            .collect()
    }

    /// Set the active (visible) terminal tab.
    pub fn set_active(&self, id: Sid) {
        *self.active.write() = Some(id);
    }

    /// Apply a new tab display order. `ids` must be a permutation of the
    /// current shell ids; the shells vector is reordered and the new order
    /// broadcast, so every client (and a later reconnect replay) sees the same
    /// tab order.
    pub fn reorder_shells(&self, ids: Vec<Sid>) -> Result<()> {
        {
            let mut shells = self.shells.write();
            let current: Vec<Sid> = shells.iter().map(|(sid, _)| *sid).collect();
            if ids.len() != current.len() || current.iter().any(|sid| !ids.contains(sid)) {
                bail!("cannot reorder shells: invalid shell list");
            }
            shells.sort_by_key(|(sid, _)| ids.iter().position(|id| id == sid).unwrap());
        }
        self.sync_shells();
        Ok(())
    }

    /// The active (visible) terminal tab, if any.
    pub fn active(&self) -> Option<Sid> {
        *self.active.read()
    }

    /// Broadcast the current shell list to every client.
    fn sync_shells(&self) {
        self.send(WsServer::Shells(self.list_shells()));
    }

    /// Resolve `auth_method` / `key_id` into the concrete private key used to
    /// authenticate. Key-mode servers referencing a missing or deleted key
    /// error out clearly instead of silently falling back to a password.
    pub fn resolve_auth(&self, server: &mut ServerConfig) -> Result<()> {
        self.config.resolve_auth(server)
    }

    /// Spawn a new shell. The position arguments are ignored in tabbed mode; a
    /// server config connects to a remote SSH host, or `None` for a local
    /// shell. `cwd` optionally sets the shell's initial working directory and
    /// `label` its tab's base display title.
    pub fn create_shell(
        self: &Arc<Self>,
        _x: i32,
        _y: i32,
        mut server: Option<ServerConfig>,
        cwd: Option<String>,
        label: String,
    ) -> Result<()> {
        if let Some(cfg) = &mut server {
            self.resolve_auth(cfg)?;
        }
        // Hard server-side cap so a malicious client cannot spawn unbounded
        // local PTY processes (安全审查 M4; the frontend's lower MAX_TERMINALS
        // is only a UI guard).
        if self.shells.read().len() >= MAX_SHELLS_PER_SESSION {
            bail!("终端数量已达上限（{MAX_SHELLS_PER_SESSION}）");
        }
        let (id, input_rx) = self.insert_shell(server.clone(), false, cwd.clone(), label);
        // The first shell becomes the active tab (later switches are driven by
        // the client's `setActive`).
        if self.active().is_none() {
            self.set_active(id);
        }
        self.sync_shells();

        let session = Arc::clone(self);
        let shell_command = self.shell_command.clone();
        let server_label = server.as_ref().map(|cfg| cfg.name.clone());
        tokio::spawn(async move {
            let result = match server {
                Some(server) => ssh_task(id, server, cwd, input_rx, Arc::clone(&session)).await,
                None => shell_task(id, shell_command, cwd, input_rx, Arc::clone(&session)).await,
            };
            if let Err(err) = result {
                debug!(%id, ?err, "shell task exited with error");
                // Report the final connect result: name the server for remote
                // shells so the failure toast is clearly about that server.
                let message = match &server_label {
                    Some(name) => format!("连接 {name} 失败：{err:#}"),
                    None => format!("终端 {id} 已退出：{err:#}"),
                };
                session.error(message);
                session.remove_shell(id);
            }
        });
        Ok(())
    }

    /// The directory this shell was created in (if any).
    fn shell_cwd(&self, id: Sid) -> Option<String> {
        self.with_shell(id, |shell| shell.cwd.clone()).flatten()
    }

    /// Record that the terminal's user was seen switching (interactive
    /// `su`/`sudo` + `Password:` prompt) to `user`.
    pub(crate) fn mark_su_detected(&self, id: Sid, user: String) {
        self.with_shell_mut(id, |shell| {
            if shell.su_detected.as_deref() != Some(user.as_str()) {
                tracing::info!(%id, user = %user, "terminal switched user (su/sudo)");
                shell.su_detected = Some(user);
            }
        });
    }

    /// The user the terminal was observed switching to via `su`/`sudo` (if
    /// any).
    fn shell_detected_su(&self, id: Sid) -> Option<String> {
        self.with_shell(id, |shell| shell.su_detected.clone())
            .flatten()
    }

    /// Open a headless SFTP connection to a server (no terminal tab, no
    /// runner task). The connection is lazy: the first SFTP operation opens
    /// it. Returns an existing headless shell if one is already open for the
    /// same target, and announces the sid to the client.
    pub fn connect_sftp_shell(&self, server: ServerConfig) -> Sid {
        // Reuse an existing headless shell for the same target.
        if let Some(id) = self.find_shell(&server, true) {
            self.send(WsServer::SftpShell(id));
            return id;
        }

        let (id, _input_rx) = self.insert_shell(Some(server), true, None, String::new());
        self.send(WsServer::SftpShell(id));
        id
    }

    /// Remove a shell from the session.
    ///
    /// Dropping the input channel signals the shell task to shut down. Returns
    /// `true` if the shell was actually removed, `false` if it did not exist.
    pub fn remove_shell(&self, id: Sid) -> bool {
        let mut shells = self.shells.write();
        let before = shells.len();
        shells.retain(|(sid, _)| *sid != id);
        let removed = shells.len() != before;
        if removed {
            drop(shells);
            self.sync_shells();
        }
        removed
    }

    /// Close a specific shell, requested by the client.
    pub fn close_shell(&self, id: Sid) -> Result<()> {
        if !self.remove_shell(id) {
            bail!("cannot close shell with id={id}, does not exist");
        }
        Ok(())
    }

    /// Resize a shell to the given dimensions, updating its PTY.
    pub fn resize_shell(&self, id: Sid, size: WsWinsize) -> Result<()> {
        if self
            .with_shell_mut(id, |shell| shell.winsize = size)
            .is_none()
        {
            bail!("cannot resize shell with id={id}, does not exist");
        }
        self.send_input_size(id, size);
        Ok(())
    }

    /// Send a resize notification to a shell's PTY.
    fn send_input_size(&self, id: Sid, size: WsWinsize) {
        self.with_shell(id, |shell| {
            shell
                .input_tx
                .try_send(ShellData::Size(u32::from(size.rows), u32::from(size.cols)))
                .ok();
        });
    }

    /// Forward terminal input from the client to a shell's PTY.
    pub fn send_input(&self, id: Sid, data: Vec<u8>) {
        if self
            .with_shell(id, |shell| {
                shell.input_tx.try_send(ShellData::Data(data)).ok();
            })
            .is_none()
        {
            warn!(%id, "received data for non-existing shell");
        }
    }

    /// Record a local shell's child PID so [`Self::pwd_request`] can read its
    /// live working directory from `/proc/<pid>/cwd` (called by the local
    /// shell task right after spawning).
    pub fn set_shell_pid(&self, id: Sid, pid: i32) {
        self.with_shell_mut(id, |shell| shell.pid = Some(pid));
    }

    /// Ask a shell for its current working directory.
    ///
    /// **Local** shells report their live cwd by reading `/proc/<pid>/cwd`
    /// (exact, no terminal echo). **Remote** shells cannot be queried across
    /// the SSH connection, so the runner parses the shell's **prompt** (see
    /// `runner.rs::feed_prompt_cwd`) — zero injection, no terminal noise — and
    /// replies via [`Self::report_pwd`] with the *real* directory the user is
    /// `cd`'d into.
    pub fn pwd_request(self: &Arc<Self>, id: Sid) {
        let server = self.with_shell(id, |shell| shell.server.clone()).flatten();
        if server.is_some() {
            // Remote shell: ask the runner to reply with the prompt-parsed cwd.
            self.with_shell(id, |shell| {
                shell.input_tx.try_send(ShellData::PwdRequest).ok();
            });
            return;
        }
        // Local shell: read /proc/<pid>/cwd for the live directory.
        let pid = self.with_shell(id, |shell| shell.pid).flatten();
        let path = pid.and_then(|pid| {
            std::fs::read_link(format!("/proc/{pid}/cwd"))
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        });
        self.send(WsServer::Pwd(id, path.unwrap_or_default()));
    }

    /// Announce a shell's current working directory (parsed from the prompt by
    /// the runner). A `~`/`~/…` path (the prompt shows `~` for the home dir)
    /// or an empty value is resolved to the SFTP login home.
    pub fn report_pwd(self: &Arc<Self>, id: Sid, path: String) {
        let needs_home = path.is_empty() || path == "~" || path.starts_with("~/");
        if !needs_home {
            self.send(WsServer::Pwd(id, path));
            return;
        }
        let session = Arc::clone(self);
        tokio::spawn(async move {
            let home = match session.shell_target(id) {
                Some((server, pool)) => pool
                    .client_probe(&server)
                    .await
                    .map(|(_, home, _)| home)
                    .unwrap_or_default(),
                None => String::new(),
            };
            let resolved = if home.is_empty() {
                String::new()
            } else if path == "~" || path.is_empty() {
                home
            } else {
                format!("{home}/{}", &path["~/".len()..])
            };
            session.send(WsServer::Pwd(id, resolved));
        });
    }

    /// Store new terminal output produced by a shell task, streaming it to the
    /// attached clients and retaining it (bounded ring) for replay when a
    /// client reconnects.
    pub fn add_data(&self, id: Sid, chunks: Vec<bytes::Bytes>) -> Result<()> {
        let mut found = false;
        {
            let mut shells = self.shells.write();
            if let Some((_, shell)) = shells.iter_mut().find(|(sid, _)| *sid == id) {
                found = true;
                for chunk in &chunks {
                    shell.ring.extend(chunk.iter().copied());
                }
                let over = shell.ring.len().saturating_sub(TERMINAL_BUFFER_BYTES);
                if over > 0 {
                    shell.ring.drain(..over);
                }
            }
        }
        if !found {
            bail!("cannot add data to shell with id={id}, does not exist");
        }
        self.send(WsServer::Chunks(id, chunks));
        Ok(())
    }

    /// Send a termination signal to exit this session.
    pub fn shutdown(&self) {
        self.shutdown.shutdown();
        // Drop the shell input channels so their tasks exit their select loops.
        self.shells.write().clear();
    }

    /// Resolves when the session has received a shutdown signal.
    pub async fn terminated(&self) {
        self.shutdown.wait().await
    }

    /// Look up the server config (and SFTP pool) for a shell, if any.
    fn shell_target(&self, id: Sid) -> Option<(ServerConfig, SftpPool)> {
        self.with_shell(id, |shell| {
            shell.server.clone().map(|s| (s, shell.sftp.clone()))
        })
        .flatten()
    }

    /// Whether a shell with this id is still registered.
    pub fn has_shell(&self, id: Sid) -> bool {
        self.with_shell(id, |_| ()).is_some()
    }

    /// Publish the latest remote host stats sampled for a shell.
    pub fn set_remote_stats(&self, id: Sid, stats: crate::stats::HostStats) {
        self.with_shell_mut(id, |shell| shell.stats = stats);
    }

    /// Decide which server the nav bar stats should come from for a shell:
    /// local shells report the machine running sshweb-server; remote shells
    /// return the values sampled on the SSH host (empty until the first
    /// sample arrives).
    pub fn shell_stats(&self, id: Sid) -> ShellStats {
        // Single read-lock acquisition: `then` keeps the Option layering so a
        // remote shell yields `Some(stats)` and a local one `None`.
        match self.with_shell(id, |shell| {
            shell.server.is_some().then(|| shell.stats.clone())
        }) {
            Some(Some(stats)) => ShellStats::Remote(stats),
            Some(None) => ShellStats::Machine,
            None => ShellStats::Unknown,
        }
    }
}
