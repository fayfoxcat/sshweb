//! Serializable types sent and received by the web server.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sshweb_core::Sid;

/// Real-time message conveying the size of a terminal.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WsWinsize {
    /// The number of rows in the window.
    pub rows: u16,
    /// The number of columns in the terminal.
    pub cols: u16,
}

impl Default for WsWinsize {
    fn default() -> Self {
        WsWinsize { rows: 24, cols: 80 }
    }
}

/// A real-time message sent from the server over WebSocket.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum WsServer {
    /// Initial server message, with the session name.
    Hello(String),
    /// Notification when the set of open shells has changed.
    Shells(Vec<(Sid, WsWinsize)>),
    /// Terminal output chunks for a shell.
    Chunks(Sid, Vec<Bytes>),
    /// SFTP directory listing for a request. The last element is true when
    /// the listing was capped (too many entries) and is incomplete.
    SftpList(Sid, String, Vec<SftpEntry>, bool),
    /// SFTP operation succeeded with no payload.
    SftpOk(Sid, String),
    /// A chunked-upload write (`SftpWriteAt`) succeeded: echoes the byte
    /// offset that was written, so the client can deduplicate retried chunks
    /// and resume exactly where the server acknowledged (a bare `SftpOk`
    /// cannot tell a retry's ack from the original's).
    SftpWriteOk(Sid, String, u64),
    /// SFTP file read data (editor reads).
    SftpData(Sid, String, Bytes),
    /// Alert the client of an application error.
    Error(String),
    /// Announces the sid of a headless SFTP shell (no terminal tab).
    SftpShell(Sid),
    /// SFTP browsing result for an `SftpOpen` / `SftpConnect` probe: the sid to
    /// browse, the initial directory, the effective user, and an optional
    /// notice explaining a fallback. An empty user means the probe failed and
    /// defaults were used.
    SftpOpenResult(Sid, String, String, Option<String>),
    /// Per-shell display labels (sid → tab title), replayed on attach so a
    /// reconnecting client restores tab titles. Visible shells only; carries
    /// no credentials.
    ShellsMeta(Vec<(Sid, String)>),
    /// Per-shell remote server configs (visible shells only), replayed on
    /// attach so a reconnecting client restores the stats / ssh-in-dir /
    /// file-follow identities. Passwords were already browser-originated; the
    /// server-internal private key is never serialized.
    ShellsConfig(Vec<(Sid, ServerConfig)>),
    /// Replay of the headless (SFTP-only) shells and their server configs, so
    /// the file manager's targets survive a refresh.
    HeadlessShells(Vec<(Sid, ServerConfig)>),
    /// Which shell is the active tab, replayed on attach.
    ActiveShell(Sid),
    /// Reply to a `PwdRequest`: the shell's current working directory (or an
    /// empty string when it could not be determined).
    Pwd(Sid, String),
}

/// A real-time message sent from the client over WebSocket.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub enum WsClient {
    /// Create a new shell. The coordinates are ignored in tabbed mode; a
    /// server config connects to a remote SSH host, or `None` for a local
    /// shell. `cwd` optionally sets the shell's initial working directory; the
    /// optional `label` is the tab's base display title (persisted server-side
    /// so it survives a browser refresh).
    Create(
        i32,
        i32,
        Option<ServerConfig>,
        Option<String>,
        Option<String>,
    ),
    /// Connect a headless SFTP session for a server (no terminal tab).
    SftpConnect(ServerConfig),
    /// Open SFTP for a terminal shell: resolve the terminal's current directory
    /// and user, optionally upgrading to that user (if `su`/`sudo` is active).
    SftpOpen(Sid),
    /// Set the active (visible) terminal tab.
    SetActive(Sid),
    /// Reorder the open shells (tab display order). The list must be a
    /// permutation of the current sids; the server applies it and broadcasts
    /// the new `Shells` order, so all clients (and a later replay) see the
    /// same order.
    ReorderShells(Vec<Sid>),
    /// Close a specific shell.
    Close(Sid),
    /// Resize a shell to the given dimensions.
    Resize(Sid, WsWinsize),
    /// Add user data to a given shell.
    Data(Sid, Bytes),
    /// List a directory for a shell (local path or remote SFTP).
    SftpList(Sid, String),
    /// Read a file from a shell, returning its content.
    SftpRead(Sid, String),
    /// Write a file to a shell (create or overwrite).
    SftpWrite(Sid, String, Bytes),
    /// Write bytes at an offset (chunked upload). Offset 0 truncates the file.
    SftpWriteAt(Sid, String, u64, Bytes),
    /// Create a directory in a shell.
    SftpMkdir(Sid, String),
    /// Delete a file or directory in a shell.
    SftpRemove(Sid, String, bool),
    /// Rename (move) a file or directory in a shell.
    SftpRename(Sid, String, String),
    /// Copy a file or directory (recursively) in a shell.
    SftpCopy(Sid, String, String),
    /// Ask a shell for its current working directory (used for uploading a
    /// dragged file to the terminal's `pwd`). The runner answers from its
    /// prompt-parsed cwd and replies with [`WsServer::Pwd`].
    PwdRequest(Sid),
}

/// A single entry in an SFTP directory listing.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SftpEntry {
    /// File or directory name.
    pub name: String,
    /// True if this entry is a directory. A symlink to a directory reports
    /// `true` so the browser can still descend into it.
    pub is_dir: bool,
    /// True for symlink entries (for highlighting). The displayed
    /// type/size/mode always describe the **target**: symlinks show as
    /// their target kind, this flag only marks them visually.
    pub is_link: bool,
    /// File size in bytes, if known.
    pub size: u64,
    /// Last modification time (Unix seconds), if known.
    pub modified: Option<u64>,
    /// Creation time (Unix seconds), if known.
    pub created: Option<u64>,
    /// Unix permission bits including the file-type part (0o040000 directory,
    /// 0o120000 symlink, 0o100000 regular file). 0 when unknown. Never
    /// follows symlinks: `/bin` on disk shows up as a `l` entry.
    pub mode: u32,
}

/// SOCKS5 隧道偏好（入站代理到远程内网）:持久化在服务器配置中,运行时监听由
/// `ProxyRegistry`（proxy.rs）管理。
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Socks5Tunnel {
    /// 本地监听端口偏好(0 = 自动分配,从 10801 起)。
    #[serde(default)]
    pub port: u16,
    /// SOCKS5 认证用户名(可选;空 = 无认证,仅 no-auth)。
    #[serde(default)]
    pub username: String,
    /// SOCKS5 认证密码(可选;`username` 非空时生效)。
    #[serde(default)]
    pub password: String,
}

/// Remote SSH server connection parameters.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    /// Human-readable name of the server.
    pub name: String,
    /// SSH host address.
    pub host: String,
    /// SSH port.
    pub port: u16,
    /// SSH username.
    pub username: String,
    /// SSH password.
    pub password: String,
    /// Terminal encoding used to decode remote output. Defaults to UTF-8.
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// Ordered chain of SSH jump hosts, or empty for a direct connection.
    #[serde(default)]
    pub hosts: Vec<JumpHost>,
    /// Proxy settings for reaching the target, if any.
    #[serde(default)]
    pub proxy: Option<ProxyConfig>,
    /// Preferred MAC (message authentication code) algorithms, in order.
    /// Empty means use the library defaults.
    #[serde(default)]
    pub macs: Vec<String>,
    /// Commands typed into the shell once the terminal starts (one per line,
    /// executed in order as if typed). Used for e.g. `export` or `cd` setup.
    #[serde(default)]
    pub startup: String,
    /// Authentication method: `"password"` (default) or `"key"`. When `"key"`
    /// the connection authenticates with the saved server-side key referenced
    /// by [`Self::key_id`].
    #[serde(default)]
    pub auth_method: Option<String>,
    /// ID of a saved server-side SSH key (see `config.rs::StoredKey`) used for
    /// public-key authentication.
    #[serde(default)]
    pub key_id: Option<String>,
    /// Resolved private key (OpenSSH PEM) for public-key authentication. The
    /// server fills this from the encrypted config when a `key_id` is used; it
    /// is never sent to the browser and never persisted.
    #[serde(default, skip_serializing)]
    pub private_key: Option<String>,
    /// SOCKS5 隧道偏好(入站代理到远程内网,见 [`Socks5Tunnel`])。缺省 = 不开启。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks5_tunnel: Option<Socks5Tunnel>,
}

impl ServerConfig {
    /// Stable identity of a server: `user@host:port`. Mirrors the frontend's
    /// `serverTargetKey`; used to bind the file-manager view and the SOCKS5
    /// tunnel registry (proxy.rs), and for human-readable target labels
    /// (keys.rs).
    pub fn target_key(&self) -> String {
        format!("{}@{}:{}", self.username, self.host, self.port)
    }
}

fn default_encoding() -> String {
    "utf-8".to_string()
}

/// A single SSH jump host in a chain (ProxyJump).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct JumpHost {
    /// Jump host address.
    pub host: String,
    /// Jump host port.
    pub port: u16,
    /// Jump host username.
    pub username: String,
    /// Jump host password (used when `key_id` is empty/absent).
    pub password: String,
    /// ID of a saved server-side SSH key (see `config.rs::StoredKey`) used for
    /// public-key authentication. Empty/absent means password authentication.
    #[serde(default)]
    pub key_id: Option<String>,
    /// Resolved private key (OpenSSH PEM) for public-key authentication. The
    /// server fills this from the encrypted config when a `key_id` is used; it
    /// is never sent to the browser and never persisted.
    #[serde(default, skip_serializing)]
    pub private_key: Option<String>,
}

/// Proxy configuration for reaching the SSH target.
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfig {
    /// Proxy type: "http" or "socks5".
    pub kind: String,
    /// Proxy host.
    pub host: String,
    /// Proxy port.
    pub port: u16,
    /// Proxy username, if any.
    pub username: String,
    /// Proxy password, if any.
    pub password: String,
}
