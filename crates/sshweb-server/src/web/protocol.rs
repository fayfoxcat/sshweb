//! Serializable types sent and received by the web server.

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use sshweb_core::Sid;

/// Deserialize a `u64` that the browser may have encoded as a CBOR **float**.
///
/// `cbor-x` encodes JavaScript numbers that do not fit in 32 bits as IEEE-754
/// float64 (`fb 41f0…` for `4294967296`), not as CBOR uint64 — see
/// `node_modules/cbor-x/encode.js`. That is invisible for small files, but the
/// byte **offset of an upload chunk** crosses 2^32 as soon as the file passes
/// 4 GiB, and the whole `sftpWriteAt` message was then rejected with
/// `invalid type: floating point …, expected integer`. Worse, the decode error
/// used to kill the WebSocket, so a large upload stalled at exactly 4 GiB and
/// its watchdog retried the same offset until it gave up (已知坑 100).
///
/// JS numbers are exact integers up to 2^53, so accepting an integral float is
/// lossless in the range that matters. Anything else (negative, fractional,
/// NaN, or beyond 2^53) stays a protocol error.
pub(crate) fn de_u64_from_number<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{Unexpected, Visitor};

    /// Largest integer a JS `number` can hold exactly.
    const MAX_EXACT: f64 = 9_007_199_254_740_992.0; // 2^53

    struct NumberVisitor;

    impl Visitor<'_> for NumberVisitor {
        type Value = u64;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a non-negative integer (or an integral CBOR float)")
        }

        fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<u64, E> {
            Ok(value)
        }

        fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<u64, E> {
            u64::try_from(value).map_err(|_| E::invalid_value(Unexpected::Signed(value), &self))
        }

        fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<u64, E> {
            if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= MAX_EXACT {
                Ok(value as u64)
            } else {
                Err(E::invalid_value(Unexpected::Float(value), &self))
            }
        }
    }

    deserializer.deserialize_any(NumberVisitor)
}

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
    /// A chunked upload finished **and was verified**: the server stat'ed
    /// `path` and its on-disk size matches the total the client reported (the
    /// echoed size). Only this message means "the file is complete" — an
    /// acked final chunk does not (a lost chunk, or a client that counted
    /// acks instead of bytes, can leave the file short without any write
    /// failing). See `sftp_upload_finish`.
    SftpUploadOk(Sid, String, u64),
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
    /// Progress of an in-flight `SftpCopy` (remote copy): the shell, the
    /// source path, and cumulative bytes copied so far. Sent throttled so the
    /// frontend can show progress for large copies without flooding the
    /// output queue.
    SftpCopyProgress(Sid, String, u64),
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
    /// The offset is carried as a JS number, so it may arrive as a CBOR float
    /// once a file passes 4 GiB (`de_u64_from_number`, 已知坑 100).
    SftpWriteAt(
        Sid,
        String,
        #[serde(deserialize_with = "de_u64_from_number")] u64,
        Bytes,
    ),
    /// Every chunk of a chunked upload was written: `total` is the file size
    /// the client uploaded. The server verifies the on-disk size before
    /// acknowledging with [`WsServer::SftpUploadOk`], and deletes the partial
    /// file instead when it does not match. Same float caveat as `SftpWriteAt`.
    SftpUploadDone(
        Sid,
        String,
        #[serde(deserialize_with = "de_u64_from_number")] u64,
    ),
    /// Give up on an in-flight chunked upload: the server deletes the partial
    /// file. Ordered through the same FIFO as the chunks, so the delete can
    /// never overtake a chunk still queued for that path (which would recreate
    /// the partial file right after it was removed).
    SftpUploadAbort(Sid, String),
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
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
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

/// 「本机」条目的设置:服务器面板里那一行硬编码的本机(终端与文件浏览器都跑在
/// sshweb 自己这台机器上)。持久化在加密配置的 `ServerSettings.local`,由服务端
/// 在**创建本地 shell / 打开本机 SFTP** 时读取——前端对本机始终发 `null`
/// (`serverTargetKey(null) === "local"`),所以这里不需要任何新的 WS 消息。
///
/// 只有这四项:本机没有主机、端口、用户名、认证方式可配。
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LocalSettings {
    /// Commands typed into a new local terminal once it starts (one per line,
    /// executed in order as if typed) — same semantics as
    /// [`ServerConfig::startup`].
    #[serde(default)]
    pub startup: String,
    /// Terminal encoding for local shell output/input (transcoded exactly like
    /// a remote `ServerConfig::encoding`). Defaults to UTF-8.
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// 家目录:新建本地终端与「本机文件系统」的默认起始目录。留空 = 保持原行为
    /// (终端用 sshweb 进程的工作目录,文件浏览用 `current_dir()`)。支持 `~` 与
    /// 相对路径(以 `$HOME` 为基准,与 [`ServerConfig::startup_dir`] 的规则一致);
    /// 路径不存在时忽略并记一条警告(见 `utils::resolve_local_start_dir`)。
    #[serde(default)]
    pub home: String,
    /// SOCKS5 直连代理偏好(监听本机端口、入站连接直接连目标,不走 SSH)。
    /// 缺省 = 不开启。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socks5_tunnel: Option<Socks5Tunnel>,
}

impl Default for LocalSettings {
    /// 手写而非 `derive`:`#[serde(default = "default_encoding")]`
    /// 在缺字段时给出 `"utf-8"`,而 derive 的 `Default` 只会给
    /// `""`——新建配置走的是 `ServerSettings::default()`,
    /// 两份默认值分叉会让前端编码下拉框显示空。
    fn default() -> Self {
        Self {
            startup: String::new(),
            encoding: default_encoding(),
            home: String::new(),
            socks5_tunnel: None,
        }
    }
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
    /// 启动目录(初始目录):新建终端在此目录启动、首次打开 SFTP 文件也在此浏览。
    /// 留空或 `~` = 该 SSH 用户的主目录;支持绝对路径或相对路径(相对路径以该
    /// 用户主目录为基准,如 `project` 即主目录下的 `project`)。
    #[serde(default)]
    pub startup_dir: String,
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

#[cfg(test)]
mod tests {
    use ciborium::value::Value;

    use super::*;

    /// Build the CBOR a browser sends for `sftpWriteAt` with a given offset
    /// value (so a test can choose float vs integer encoding).
    fn write_at_messages(offset: Value) -> Vec<u8> {
        let msg = Value::Map(vec![(
            Value::Text("sftpWriteAt".into()),
            Value::Array(vec![
                Value::Integer(1.into()),
                Value::Text("/tmp/f".into()),
                offset,
                Value::Bytes(vec![0x41]),
            ]),
        )]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&msg, &mut buf).unwrap();
        buf
    }

    fn decode(bytes: &[u8]) -> Result<WsClient, ciborium::de::Error<std::io::Error>> {
        ciborium::de::from_reader(bytes)
    }

    #[test]
    fn write_at_accepts_an_integer_offset() {
        let bytes = write_at_messages(Value::Integer(4_194_304u64.into()));
        match decode(&bytes).unwrap() {
            WsClient::SftpWriteAt(sid, path, offset, data) => {
                assert_eq!(sid, Sid(1));
                assert_eq!(path, "/tmp/f");
                assert_eq!(offset, 4_194_304);
                assert_eq!(data.as_ref(), &[0x41]);
            }
            other => panic!("unexpected message: {other:?}"),
        }
    }

    /// `cbor-x` turns every JS number >= 2^32 into a float64, so a chunked
    /// upload past 4 GiB sends `offset` as a float. Rejecting that killed the
    /// WebSocket and stalled every large upload (已知坑 100).
    #[test]
    fn write_at_accepts_a_float_encoded_offset() {
        let bytes = write_at_messages(Value::Float(4_294_967_296.0));
        match decode(&bytes).unwrap() {
            WsClient::SftpWriteAt(_, _, offset, _) => assert_eq!(offset, 4_294_967_296),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    /// The same encoding reaches `sftpUploadDone.total` when a file is exactly
    /// a multiple of 4 GiB.
    #[test]
    fn upload_done_accepts_a_float_encoded_total() {
        let msg = Value::Map(vec![(
            Value::Text("sftpUploadDone".into()),
            Value::Array(vec![
                Value::Integer(2.into()),
                Value::Text("/tmp/f".into()),
                Value::Float(4_294_967_296.0),
            ]),
        )]);
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&msg, &mut buf).unwrap();
        match decode(&buf).unwrap() {
            WsClient::SftpUploadDone(_, _, total) => assert_eq!(total, 4_294_967_296),
            other => panic!("unexpected message: {other:?}"),
        }
    }

    /// A float is only tolerated when it is an exact non-negative integer:
    /// fractional or out-of-range values stay a protocol error.
    #[test]
    fn offset_still_rejects_non_integral_floats() {
        for value in [1.5, -1.0, f64::NAN, 1e300] {
            let bytes = write_at_messages(Value::Float(value));
            assert!(decode(&bytes).is_err(), "float {value} must be rejected");
        }
    }
}
