//! Encrypted, server-side configuration and browser access control.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use anyhow::{bail, Context, Result};
use argon2::Argon2;
use base64::prelude::{Engine as _, BASE64_STANDARD};
use parking_lot::{Mutex, RwLock};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};

use crate::web::protocol::ServerConfig;

const FILE_VERSION: u32 = 1;
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 12;
const KEY_BYTES: usize = 32;
const MIN_PASSWORD_LENGTH: usize = 6;
const ASSOCIATED_DATA: &[u8] = b"sshx-server-config-v1";
const COOKIE_NAME: &str = "sshweb_auth";
const STALE_AUTH: &str = "认证状态已失效，请重新登录";

/// Server-side settings persisted in the encrypted file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    /// Saved SSH server configurations.
    pub servers: Vec<StoredServerConfig>,
    /// Saved SSH keypairs (public-key authentication).
    #[serde(default)]
    pub keys: Vec<StoredKey>,
    /// SSH host key fingerprints (SHA256 base64) by `user@host:port`, recorded
    /// on first connect and verified afterwards (TOFU, 安全审查 H2). `serde`
    /// default keeps configs written before this field existed readable.
    #[serde(default)]
    pub host_keys: std::collections::HashMap<String, String>,
}

/// A persisted SSH keypair (Ed25519). The private key stays server-side inside
/// the encrypted config file and is never sent to the browser — only the
/// public parts ([`KeyInfo`]) are exposed over the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredKey {
    /// Stable identifier referenced by `ServerConfig::key_id`.
    pub id: String,
    /// User-facing name.
    pub name: String,
    /// OpenSSH public key line (`ssh-ed25519 AAAA… comment`).
    pub public_key: String,
    /// SHA256 fingerprint for display.
    pub fingerprint: String,
    /// OpenSSH-format PEM private key.
    pub private_key: String,
}

/// Public key info returned to the browser (never the private key).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyInfo {
    /// Stable identifier referenced by `ServerConfig::key_id`.
    pub id: String,
    /// User-facing name.
    pub name: String,
    /// OpenSSH public key line (`ssh-ed25519 AAAA… comment`).
    pub public_key: String,
    /// SHA256 fingerprint for display.
    pub fingerprint: String,
}

impl KeyInfo {
    fn from_key(key: &StoredKey) -> Self {
        Self {
            id: key.id.clone(),
            name: key.name.clone(),
            public_key: key.public_key.clone(),
            fingerprint: key.fingerprint.clone(),
        }
    }
}

/// Summary of the configuration store (no password needed to obtain).
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// Path of the encrypted config file.
    pub path: String,
    /// Number of configured servers.
    pub server_count: usize,
    /// Number of saved SSH keys.
    pub key_count: usize,
}

/// One configured server, safe for CLI display (no password or private key).
#[derive(Debug, Clone, Serialize)]
pub struct ServerRow {
    /// Stable identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// SSH host.
    pub host: String,
    /// SSH port.
    pub port: u16,
    /// SSH username.
    pub username: String,
    /// `"password"` or `"key"`.
    pub auth: String,
    /// ID of the saved key used when `auth == "key"`.
    pub key_id: Option<String>,
    /// Whether a password is stored in the encrypted config.
    pub has_password: bool,
    /// Whether a proxy is configured.
    pub has_proxy: bool,
    /// Number of jump hosts in the chain.
    pub jumps: usize,
}

/// A persisted SSH server configuration. The id is kept outside the WebSocket
/// configuration so it can be used by the browser as a stable list key while
/// the serialized fields remain flat and compatible with `ServerConfig`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredServerConfig {
    /// Stable browser-side identifier.
    pub id: String,
    /// SSH connection fields.
    #[serde(flatten)]
    pub config: ServerConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EncryptedFile {
    version: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

#[derive(Debug, Clone)]
struct Unlocked {
    settings: ServerSettings,
    key: [u8; KEY_BYTES],
    salt: [u8; SALT_BYTES],
}

/// Persistent encrypted configuration store and in-memory authentication state.
pub struct ConfigStore {
    path: PathBuf,
    encrypted: Mutex<Option<EncryptedFile>>,
    unlocked: RwLock<Option<Unlocked>>,
    sessions: Mutex<HashSet<String>>,
    /// Sessions that logged in with the one-time setup key and MUST change the
    /// access password before any other use (首次启动安装密钥,见 H1).
    pending_change: Mutex<HashSet<String>>,
    /// One-time setup key printed at first startup; consumed on first login.
    setup_key: Mutex<Option<String>>,
    /// Whether to mark the auth cookie `Secure` (set when serving over TLS).
    secure_cookies: AtomicBool,
}

impl ConfigStore {
    /// Open an existing encrypted configuration file, or an empty uninitialized
    /// store.
    pub fn new(path: PathBuf) -> Result<Arc<Self>> {
        let encrypted = if path.exists() {
            Some(read_encrypted_file_at(&path)?)
        } else {
            None
        };
        Ok(Arc::new(Self {
            path,
            encrypted: Mutex::new(encrypted),
            unlocked: RwLock::new(None),
            sessions: Mutex::new(HashSet::new()),
            pending_change: Mutex::new(HashSet::new()),
            setup_key: Mutex::new(None),
            secure_cookies: AtomicBool::new(false),
        }))
    }

    /// Whether the auth cookie should carry the `Secure` flag (set at server
    /// startup when serving over TLS).
    pub fn secure_cookies(&self) -> bool {
        self.secure_cookies.load(Ordering::Relaxed)
    }

    /// Set the `Secure` cookie flag (called at server startup when TLS is
    /// enabled).
    pub fn set_secure_cookies(&self, secure: bool) {
        self.secure_cookies.store(secure, Ordering::Relaxed);
    }

    /// The one-time setup key for a not-yet-configured store, generating it on
    /// first access. Returns `None` once a password has been set. The server
    /// prints this at startup so the operator can perform the first login; it
    /// is never exposed over the network.
    pub fn setup_key(&self) -> Option<String> {
        if self.is_setup() {
            return None;
        }
        let mut guard = self.setup_key.lock();
        if guard.is_none() {
            *guard = Some(BASE64_STANDARD.encode(random_array::<32>()));
        }
        guard.clone()
    }

    /// Return the default persistent configuration path. **程序运行目录**(当前
    /// 工作目录)下的固定文件,不写入 `$XDG_CONFIG_HOME` / `$HOME`:配置文件随
    /// 启动目录走,便于把整个 sshweb 目录拷走即迁移。用 `--config` 可覆盖。
    pub fn default_path() -> PathBuf {
        PathBuf::from("sshweb-config.enc")
    }

    /// Return the path used by the store.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether an access password has already been configured.
    pub fn is_setup(&self) -> bool {
        self.encrypted.lock().is_some()
    }

    /// Check whether a request carries a currently valid authentication cookie.
    pub fn is_authenticated(&self, cookie_header: Option<&str>) -> bool {
        let Some(token) = cookie_value(cookie_header, COOKIE_NAME) else {
            return false;
        };
        self.sessions.lock().contains(token)
    }

    /// Whether the given session logged in with the setup key and still must
    /// set an access password before any other use.
    pub fn is_pending_change(&self, token: &str) -> bool {
        self.pending_change.lock().contains(token)
    }

    /// Whether the request's cookie names a session that is still pending a
    /// forced password change.
    pub fn is_pending_change_session(&self, cookie_header: Option<&str>) -> bool {
        let Some(token) = cookie_value(cookie_header, COOKIE_NAME) else {
            return false;
        };
        self.is_pending_change(token)
    }

    /// Configure the first access password and create an authenticated session.
    pub fn setup(&self, password: &str, confirmation: &str) -> Result<String> {
        if self.is_setup() {
            bail!("访问密码已经设置");
        }
        validate_password(password, confirmation)?;
        let salt = random_array::<SALT_BYTES>();
        let key = derive_key(password, &salt)?;
        let unlocked = Unlocked {
            settings: ServerSettings::default(),
            key,
            salt,
        };
        self.persist_and_commit(&unlocked)?;
        *self.unlocked.write() = Some(unlocked);
        Ok(self.create_session())
    }

    /// Authenticate with the configured access password. When the store is not
    /// set up yet, the one-time setup key is the only accepted credential and
    /// the resulting session is forced to change the password first.
    pub fn login(&self, password: &str) -> Result<String> {
        if !self.is_setup() {
            // First-boot: the only credential is the one-time setup key printed
            // at startup. It is consumed on first use and the resulting session
            // is forced to set a real access password before anything else.
            let key = self
                .setup_key
                .lock()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("安装密钥已失效，请重启服务查看新密钥"))?;
            if !constant_time_eq(password.as_bytes(), key.as_bytes()) {
                bail!("安装密钥错误");
            }
            // Consume the key only on a successful login.
            *self.setup_key.lock() = None;
            let token = self.create_session();
            self.pending_change.lock().insert(token.clone());
            return Ok(token);
        }
        let (salt, key, settings) = self.decrypt_with_key(password)?;
        *self.unlocked.write() = Some(Unlocked {
            settings,
            key,
            salt,
        });
        Ok(self.create_session())
    }

    /// Decrypt the config with `password` and return the derived salt, key and
    /// parsed settings. Shared by [`ConfigStore::login`] and
    /// [`ConfigStore::decrypt`] so the ciphertext → settings pipeline lives
    /// in exactly one place.
    fn decrypt_with_key(
        &self,
        password: &str,
    ) -> Result<([u8; SALT_BYTES], [u8; KEY_BYTES], ServerSettings)> {
        let encrypted = self
            .encrypted
            .lock()
            .clone()
            .ok_or_else(|| anyhow::anyhow!("尚未设置访问密码"))?;
        let salt = decode_fixed::<SALT_BYTES>(&encrypted.salt)?;
        let nonce = decode_fixed::<NONCE_BYTES>(&encrypted.nonce)?;
        let key = derive_key(password, &salt)?;
        let ciphertext = BASE64_STANDARD
            .decode(encrypted.ciphertext)
            .context("invalid encrypted config payload")?;
        let cipher = Aes256Gcm::new_from_slice(&key).expect("AES-256 key has fixed size");
        let plaintext = cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                aes_gcm::aead::Payload {
                    msg: &ciphertext,
                    aad: ASSOCIATED_DATA,
                },
            )
            .map_err(|_| anyhow::anyhow!("访问密码错误"))?;
        let settings: ServerSettings =
            serde_json::from_slice(&plaintext).context("decrypted config payload is invalid")?;
        Ok((salt, key, settings))
    }

    /// Change the page access password for an authenticated request.
    ///
    /// The old password is verified against the in-memory key; a fresh salt and
    /// Argon2 key are derived from the new password and the stored settings are
    /// re-encrypted atomically. The current session stays logged in; all other
    /// sessions are invalidated so old cookies can't be reused.
    pub fn change_password(
        &self,
        cookie_header: Option<&str>,
        old_password: &str,
        new_password: &str,
        confirmation: &str,
    ) -> Result<()> {
        let token =
            cookie_value(cookie_header, COOKIE_NAME).ok_or_else(|| anyhow::anyhow!("需要登录"))?;
        if !self.sessions.lock().contains(token) {
            bail!("需要登录");
        }
        // A setup-key session has no config yet: the only action allowed is
        // setting the first access password (强制首次修改).
        if self.is_pending_change(token) {
            return self.complete_setup_session(token, new_password, confirmation);
        }
        self.with_state_mut(cookie_header, |state| {
            // Verify the old password against the key derived at login time.
            let old_key = derive_key(old_password, &state.salt)?;
            if old_key != state.key {
                bail!("当前访问密码错误");
            }
            validate_password(new_password, confirmation)?;
            let salt = random_array::<SALT_BYTES>();
            let key = derive_key(new_password, &salt)?;
            let unlocked = Unlocked {
                settings: state.settings.clone(),
                key,
                salt,
            };
            // Persist first: only commit the new state if the file was written.
            self.persist_and_commit(&unlocked)?;
            *state = unlocked;
            Ok(())
        })?;
        // Keep the current session, drop every other one.
        let token = token.to_string();
        self.sessions.lock().retain(|t| *t == token);
        Ok(())
    }

    /// Complete the forced first-login password change: create the encrypted
    /// config with `password`, convert the setup-key session into a normal one,
    /// and (like a regular password change) drop every other session.
    fn complete_setup_session(
        &self,
        token: &str,
        password: &str,
        confirmation: &str,
    ) -> Result<()> {
        validate_password(password, confirmation)?;
        let salt = random_array::<SALT_BYTES>();
        let key = derive_key(password, &salt)?;
        let unlocked = Unlocked {
            settings: ServerSettings::default(),
            key,
            salt,
        };
        self.persist_and_commit(&unlocked)?;
        *self.unlocked.write() = Some(unlocked);
        self.pending_change.lock().remove(token);
        self.sessions.lock().retain(|t| t == token);
        Ok(())
    }

    /// Revoke a session token.
    pub fn logout(&self, cookie_header: Option<&str>) {
        if let Some(token) = cookie_value(cookie_header, COOKIE_NAME) {
            self.sessions.lock().remove(token);
        }
        if self.sessions.lock().is_empty() {
            *self.unlocked.write() = None;
        }
    }

    /// Read settings for an authenticated request.
    pub fn settings(&self, cookie_header: Option<&str>) -> Result<ServerSettings> {
        self.with_state_read(cookie_header, |state| state.settings.clone())
    }

    /// Replace settings and persist them for an authenticated request.
    pub fn save_settings(
        &self,
        cookie_header: Option<&str>,
        settings: ServerSettings,
    ) -> Result<()> {
        self.with_state_mut(cookie_header, |state| {
            // The browser's `PUT /api/config` manages only servers; SSH keys are
            // handled by /api/keys. Preserve the stored keys unless the payload
            // explicitly carries a non-empty list (avoid wiping them on save).
            if !settings.keys.is_empty() {
                state.settings.keys = settings.keys;
            }
            state.settings.servers = settings.servers;
            self.persist(state)
        })
    }

    /// Import the browser's legacy configuration into an empty server store.
    /// One-time migration: only fills an empty store; any existing settings
    /// (including saved keys) are left untouched.
    pub fn import_settings(
        &self,
        cookie_header: Option<&str>,
        settings: ServerSettings,
    ) -> Result<ServerSettings> {
        self.with_state_mut(cookie_header, |state| {
            if state.settings.servers.is_empty() && !settings.servers.is_empty() {
                let keys = state.settings.keys.clone();
                state.settings.servers = settings.servers;
                state.settings.keys = keys;
                self.persist(state)?;
            }
            Ok(state.settings.clone())
        })
    }

    /// List the saved SSH keys (public parts only).
    pub fn list_keys(&self, cookie_header: Option<&str>) -> Result<Vec<KeyInfo>> {
        self.with_state_read(cookie_header, |state| {
            state.settings.keys.iter().map(KeyInfo::from_key).collect()
        })
    }

    /// Export the **encrypted config file bytes** (the whole `config.enc` JSON)
    /// for a full backup / migration — used by the `sshweb-server export` CLI.
    /// The private keys stay inside the ciphertext; nothing sensitive is
    /// exposed in plaintext. CLI-level operation, no auth.
    pub fn export_backup(&self) -> Result<Vec<u8>> {
        fs::read(&self.path)
            .with_context(|| format!("cannot read config file {}", self.path.display()))
    }

    /// Import a previously-exported encrypted config file (raw `config.enc`
    /// JSON bytes) — used by the `sshweb-server import` CLI. Validates the
    /// structure, writes it atomically, and refreshes the cached ciphertext.
    /// Does not touch sessions (they are reset on the next service start
    /// anyway); the operator uses the restored config's access password.
    pub fn import_backup(&self, bytes: &[u8]) -> Result<()> {
        let file: EncryptedFile =
            serde_json::from_slice(bytes).context("备份文件不是有效的加密配置文件")?;
        if file.version != FILE_VERSION {
            bail!("不支持的配置版本 {}", file.version);
        }
        write_atomic(&self.path, bytes)?;
        *self.encrypted.lock() = Some(file);
        // The new ciphertext may use a different password: drop the unlocked
        // state so the next login re-reads with the restored password.
        *self.unlocked.write() = None;
        Ok(())
    }

    /// Decrypt the config with `password` and return the settings, **without**
    /// creating an auth session or touching the unlocked state. Used by the
    /// `sshweb keys` CLI.
    pub fn decrypt(&self, password: &str) -> Result<ServerSettings> {
        let (_, _, settings) = self.decrypt_with_key(password)?;
        Ok(settings)
    }

    /// Re-encrypt the config with a new access password, preserving all
    /// settings (servers + keys). Verifies `old_password` first, derives a
    /// fresh salt + key from `new_password`, persists atomically and drops
    /// all in-memory sessions (the running service keeps the old in-memory
    /// state until it is restarted). Used by the `sshweb reset-password` CLI.
    pub fn reencrypt(&self, old_password: &str, new_password: &str) -> Result<()> {
        let settings = self.decrypt(old_password)?;
        validate_password(new_password, new_password)?;
        let salt = random_array::<SALT_BYTES>();
        let key = derive_key(new_password, &salt)?;
        let unlocked = Unlocked {
            settings,
            key,
            salt,
        };
        self.persist_and_commit(&unlocked)?;
        *self.unlocked.write() = Some(unlocked);
        self.sessions.lock().clear();
        Ok(())
    }

    /// Describe the current configuration state without requiring a password.
    /// Returns `None` when no access password is configured yet.
    pub fn status(&self) -> Option<Status> {
        self.encrypted.lock().as_ref()?;
        let (server_count, key_count) = match self.unlocked.read().as_ref() {
            Some(u) => (u.settings.servers.len(), u.settings.keys.len()),
            None => (0, 0),
        };
        Some(Status {
            path: self.path.display().to_string(),
            server_count,
            key_count,
        })
    }

    /// List the configured servers. Passwords are never returned; `password`
    /// reports only whether one is stored, `key_id` references a saved key.
    pub fn servers(&self, password: &str) -> Result<Vec<ServerRow>> {
        let settings = self.decrypt(password)?;
        Ok(settings
            .servers
            .iter()
            .map(|s| ServerRow {
                id: s.id.clone(),
                name: s.config.name.clone(),
                host: s.config.host.clone(),
                port: s.config.port,
                username: s.config.username.clone(),
                auth: s
                    .config
                    .auth_method
                    .as_deref()
                    .unwrap_or("password")
                    .to_string(),
                key_id: s.config.key_id.clone(),
                has_password: !s.config.password.is_empty(),
                has_proxy: s.config.proxy.is_some(),
                jumps: s.config.hosts.len(),
            })
            .collect())
    }

    /// Generate a new Ed25519 keypair and persist it. The private key is only
    /// ever stored inside the encrypted config file.
    pub fn create_key(&self, cookie_header: Option<&str>, name: String) -> Result<KeyInfo> {
        self.with_state_mut(cookie_header, |state| {
            let key = generate_key(name)?;
            state.settings.keys.push(key.clone());
            self.persist(state)?;
            Ok(KeyInfo::from_key(&key))
        })
    }

    /// Delete a saved SSH key. Returns `false` when the id did not exist.
    pub fn delete_key(&self, cookie_header: Option<&str>, id: &str) -> Result<bool> {
        self.with_state_mut(cookie_header, |state| {
            let before = state.settings.keys.len();
            state.settings.keys.retain(|k| k.id != id);
            if state.settings.keys.len() == before {
                return Ok(false);
            }
            self.persist(state)?;
            Ok(true)
        })
    }

    /// Rename a saved SSH key. Errors when the id does not exist.
    pub fn rename_key(
        &self,
        cookie_header: Option<&str>,
        id: &str,
        name: String,
    ) -> Result<KeyInfo> {
        self.with_state_mut(cookie_header, |state| {
            // Rename in place, then clone out of the mutable borrow so the
            // immutable `persist(state)` and the returned `KeyInfo` don't
            // overlap with the `iter_mut` borrow.
            let renamed = {
                let key = state
                    .settings
                    .keys
                    .iter_mut()
                    .find(|k| k.id == id)
                    .ok_or_else(|| anyhow::anyhow!("密钥不存在"))?;
                key.name = name;
                key.clone()
            };
            self.persist(state)?;
            Ok(KeyInfo::from_key(&renamed))
        })
    }

    /// The saved key with this id (a clone: the key lives inside the encrypted
    /// config state, only reachable behind the read lock).
    fn find_key(&self, id: &str) -> Option<StoredKey> {
        self.unlocked
            .read()
            .as_ref()
            .and_then(|state| state.settings.keys.iter().find(|k| k.id == id))
            .cloned()
    }

    /// The public key line of a saved key, if any (used to install it on a
    /// server).
    pub fn key_public_key(&self, id: &str) -> Option<String> {
        self.find_key(id).map(|k| k.public_key)
    }

    /// The OpenSSH PEM private key of a saved key, if any. Used by the
    /// WebSocket handlers to resolve `ServerConfig::key_id` into the key used
    /// to authenticate.
    pub fn resolve_private_key(&self, id: &str) -> Option<String> {
        self.find_key(id).map(|k| k.private_key)
    }

    /// The stored SSH host key fingerprint for `user@host:port`, if any (TOFU,
    /// 安全审查 H2). `None` means the target has never been connected to.
    pub fn host_key_for(&self, target: &str) -> Option<String> {
        self.unlocked
            .read()
            .as_ref()
            .and_then(|s| s.settings.host_keys.get(target).cloned())
    }

    /// Record (or replace) the SSH host key fingerprint for a target after a
    /// successful first connection (TOFU). Called from the SSH connect path,
    /// which runs under an already-authenticated session, so no cookie is
    /// required to write the in-memory state and persist it.
    pub fn record_host_key(&self, target: &str, fingerprint: String) {
        let mut guard = self.unlocked.write();
        if let Some(state) = guard.as_mut() {
            let changed = state
                .settings
                .host_keys
                .insert(target.into(), fingerprint.clone());
            if changed != Some(fingerprint) {
                let _ = self.persist(state);
            }
        }
    }

    /// Resolve `auth_method` / `key_id` into the concrete private key used to
    /// authenticate. Key-mode configs referencing a missing or deleted key
    /// error out clearly instead of silently falling back to a password.
    /// Jump hosts resolve the same way: a `key_id` selects the saved key,
    /// otherwise the password is used. Used by the WebSocket session handlers
    /// and the connection-test endpoint.
    pub fn resolve_auth(&self, server: &mut crate::web::protocol::ServerConfig) -> Result<()> {
        if server.auth_method.as_deref() == Some("key") {
            let key_id = server
                .key_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("已选择密钥认证但未指定密钥"))?;
            server.private_key = match self.resolve_private_key(&key_id) {
                Some(pem) => Some(pem),
                None => bail!("密钥不存在或已删除"),
            };
        } else {
            server.private_key = None;
            server.key_id = None;
        }
        for host in &mut server.hosts {
            let key_id = host.key_id.as_deref().filter(|id| !id.is_empty());
            host.private_key = match key_id {
                Some(id) => match self.resolve_private_key(id) {
                    Some(pem) => Some(pem),
                    None => bail!("密钥不存在或已删除"),
                },
                None => None,
            };
        }
        Ok(())
    }

    fn require_auth(&self, cookie_header: Option<&str>) -> Result<()> {
        if self.is_authenticated(cookie_header) {
            Ok(())
        } else {
            bail!("需要登录")
        }
    }

    /// Run `f` with read access to the decrypted state for an authenticated
    /// request. Collapses the repeated `require_auth` + `unlocked.read()` +
    /// `STALE_AUTH` boilerplate of the config API methods.
    fn with_state_read<T>(
        &self,
        cookie_header: Option<&str>,
        f: impl FnOnce(&Unlocked) -> T,
    ) -> Result<T> {
        self.require_auth(cookie_header)?;
        let guard = self.unlocked.read();
        let state = guard.as_ref().ok_or_else(|| anyhow::anyhow!(STALE_AUTH))?;
        Ok(f(state))
    }

    /// Run `f` with write access to the decrypted state for an authenticated
    /// request (see [`Self::with_state_read`]).
    fn with_state_mut<T>(
        &self,
        cookie_header: Option<&str>,
        f: impl FnOnce(&mut Unlocked) -> Result<T>,
    ) -> Result<T> {
        self.require_auth(cookie_header)?;
        let mut guard = self.unlocked.write();
        let state = guard.as_mut().ok_or_else(|| anyhow::anyhow!(STALE_AUTH))?;
        f(state)
    }

    fn create_session(&self) -> String {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = BASE64_STANDARD.encode(bytes);
        self.sessions.lock().insert(token.clone());
        token
    }

    /// Persist `unlocked` to disk and refresh the cached encrypted file
    /// metadata (`persist` now keeps the cache in sync, so a later `login()`
    /// never decrypts with stale salt/ciphertext). Kept as the explicit
    /// "write + commit" name for the auth flows that used it.
    fn persist_and_commit(&self, unlocked: &Unlocked) -> Result<()> {
        self.persist(unlocked)
    }

    fn persist(&self, unlocked: &Unlocked) -> Result<()> {
        let plaintext = serde_json::to_vec(&unlocked.settings)?;
        let nonce = random_array::<NONCE_BYTES>();
        let cipher = Aes256Gcm::new_from_slice(&unlocked.key).expect("AES-256 key has fixed size");
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                aes_gcm::aead::Payload {
                    msg: &plaintext,
                    aad: ASSOCIATED_DATA,
                },
            )
            .map_err(|_| anyhow::anyhow!("failed to encrypt config"))?;
        let file = EncryptedFile {
            version: FILE_VERSION,
            salt: BASE64_STANDARD.encode(unlocked.salt),
            nonce: BASE64_STANDARD.encode(nonce),
            ciphertext: BASE64_STANDARD.encode(ciphertext),
        };
        write_atomic(&self.path, &serde_json::to_vec_pretty(&file)?)?;
        // Keep the cached ciphertext in sync so a later login (which decrypts
        // the cache, not the disk) observes this change. Without this, every
        // mutation that only calls `persist` (config saves, key
        // create/delete/rename) is invisible to a fresh login on the same
        // running server until it restarts.
        *self.encrypted.lock() = Some(file);
        Ok(())
    }
}

/// Read, parse and version-check an encrypted config file (shared by
/// [`ConfigStore::new`] and the post-persist cache refresh).
fn read_encrypted_file_at(path: &Path) -> Result<EncryptedFile> {
    let bytes =
        fs::read(path).with_context(|| format!("cannot read config file {}", path.display()))?;
    let file: EncryptedFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid config file {}", path.display()))?;
    if file.version != FILE_VERSION {
        bail!("unsupported config file version {}", file.version);
    }
    Ok(file)
}

fn validate_password(password: &str, confirmation: &str) -> Result<()> {
    if password.chars().count() < MIN_PASSWORD_LENGTH {
        bail!("访问密码至少需要 {MIN_PASSWORD_LENGTH} 个字符");
    }
    if password != confirmation {
        bail!("两次输入的访问密码不一致");
    }
    Ok(())
}

fn derive_key(password: &str, salt: &[u8; SALT_BYTES]) -> Result<[u8; KEY_BYTES]> {
    let mut key = [0u8; KEY_BYTES];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|err| anyhow::anyhow!("failed to derive config key: {err}"))?;
    Ok(key)
}

fn random_array<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    OsRng.fill_bytes(&mut bytes);
    bytes
}

/// Generate a fresh Ed25519 keypair (32 random seed bytes → keypair), encoded
/// as an OpenSSH-format private key with a `sshweb-<id>` comment.
fn generate_key(name: String) -> Result<StoredKey> {
    let id = sshweb_core::rand_alphanumeric(16);
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let keypair = ssh_key::private::Ed25519Keypair::from_seed(&seed);
    let comment = format!("sshweb-{id}");
    let private_key =
        ssh_key::PrivateKey::new(ssh_key::private::KeypairData::Ed25519(keypair), comment)
            .context("generate ed25519 keypair")?;
    let pem = private_key
        .to_openssh(ssh_key::LineEnding::LF)
        .context("encode ed25519 private key")?;
    let public = private_key
        .public_key()
        .to_openssh()
        .context("encode ed25519 public key")?;
    let fingerprint = format!(
        "{}",
        private_key
            .public_key()
            .fingerprint(ssh_key::HashAlg::Sha256)
    );
    Ok(StoredKey {
        id: id.clone(),
        name,
        // `to_openssh` already appends the key comment.
        public_key: public,
        fingerprint,
        private_key: pem.to_string(),
    })
}

fn decode_fixed<const N: usize>(encoded: &str) -> Result<[u8; N]> {
    let bytes = BASE64_STANDARD.decode(encoded)?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid fixed-size config field"))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("cannot create config directory {}", parent.display()))?;
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, bytes).with_context(|| format!("cannot write {}", temp.display()))?;
    set_private_permissions(&temp)?;
    fs::rename(&temp, path)
        .with_context(|| format!("cannot replace config file {}", path.display()))?;
    set_private_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Extract a single cookie value by name from a `Cookie` header.
fn cookie_value<'a>(header: Option<&'a str>, name: &str) -> Option<&'a str> {
    header?.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

/// Constant-time byte comparison (used for the setup key, which is not hashed).
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Name of the authentication cookie used by the HTTP and WebSocket handlers.
pub fn cookie_name() -> &'static str {
    COOKIE_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_parser_reads_auth_cookie() {
        assert_eq!(
            cookie_value(Some("theme=dark; sshweb_auth=abc"), COOKIE_NAME),
            Some("abc")
        );
    }

    #[test]
    fn password_validation_requires_confirmation_and_length() {
        assert!(validate_password("abcde", "abcde").is_err());
        assert!(validate_password("long-enough", "different").is_err());
        assert!(validate_password("long-enough", "long-enough").is_ok());
        assert!(validate_password("123456", "123456").is_ok());
    }

    #[test]
    fn change_password_reencrypts_and_keeps_current_session() {
        let dir = std::env::temp_dir().join(format!(
            "sshweb-config-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("config.enc");
        let store = ConfigStore::new(path.clone()).unwrap();
        let token = store.setup("123456", "123456").unwrap();
        let cookie = format!("{COOKIE_NAME}={token}");

        // Wrong old password is rejected.
        assert!(store
            .change_password(Some(&cookie), "wrong", "abcdef", "abcdef")
            .is_err());
        // Confirmation mismatch is rejected.
        assert!(store
            .change_password(Some(&cookie), "123456", "abcdef", "abcde")
            .is_err());
        // Unauthenticated request is rejected.
        assert!(store
            .change_password(Some("sshweb_auth=other"), "123456", "abcdef", "abcdef")
            .is_err());

        store
            .change_password(Some(&cookie), "123456", "abcdef", "abcdef")
            .unwrap();

        // The old password no longer authenticates; the new one does, and the
        // in-memory settings were preserved (same settings, new key).
        assert!(store.login("123456").is_err());
        let token2 = store.login("abcdef").unwrap();
        assert!(token2 != token);

        // The persisted file re-opens with the new password only.
        let store2 = ConfigStore::new(path).unwrap();
        assert!(store2.login("123456").is_err());
        assert!(store2.login("abcdef").is_ok());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setup_key_login_forces_password_change() {
        let dir = std::env::temp_dir().join(format!(
            "sshweb-setupkey-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = dir.join("config.enc");
        let store = ConfigStore::new(path.clone()).unwrap();

        // Not set up: the setup key is the only credential.
        let key = store.setup_key().expect("setup key on fresh store");
        assert!(!key.is_empty());
        assert!(store.is_setup() == false);
        // Wrong key is rejected.
        assert!(store.login("wrong-key").is_err());
        // Correct key logs in and the session is forced to change the password.
        let token = store.login(&key).unwrap();
        assert!(store.is_pending_change(&token));
        let cookie = format!("{COOKIE_NAME}={token}");
        // A pending session cannot change the password as a normal session
        // (old password is ignored) but CAN set the first password.
        store
            .change_password(Some(&cookie), "", "newpass123", "newpass123")
            .unwrap();
        // After the forced change the config is set up and the session is normal.
        assert!(store.is_setup());
        assert!(!store.is_pending_change(&token));
        // The setup key is consumed; the new password now authenticates.
        assert!(store.login(&key).is_err());
        let token2 = store.login("newpass123").unwrap();
        assert!(!store.is_pending_change(&token2));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
