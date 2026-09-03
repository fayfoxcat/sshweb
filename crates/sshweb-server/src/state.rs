//! Shared stateful components of the server.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use dashmap::DashMap;

use crate::config::ConfigStore;
use crate::proxy::ProxyRegistry;
use crate::session::Session;
use crate::stats::SystemStats;
use crate::web::ratelimit::RateLimiter;
use crate::ServerOptions;

/// Default time a session with no attached clients is kept alive before its
/// terminal processes are reaped.
pub const SESSION_RECLAIM_SECS: u64 = 1800;

/// Shared state object for global server logic.
pub struct ServerState {
    /// Command used to launch shells in new sessions.
    shell_command: String,

    /// All currently active sessions, keyed by session name.
    store: DashMap<String, Arc<Session>>,

    /// System statistics (CPU, memory, time) for the stats endpoint.
    stats: SystemStats,

    /// Encrypted server-side settings and access-control state.
    config: Arc<ConfigStore>,

    /// Idle sessions are reclaimed after this long without any client.
    session_ttl: Duration,

    /// SOCKS5 隧道注册表(每台服务器一个入站代理端口)。
    proxies: ProxyRegistry,

    /// 登录/改密端点按来源 IP 的失败限流(安全审查 M1)。
    auth_limiter: RateLimiter,
}

/// The shell used for new local terminals when `--shell` is not given: prefer
/// the launching environment's `$SHELL` (so `sshweb -d` inherits the deploy
/// user's login shell, e.g. bash), falling back to `/bin/bash`. The previous
/// hard-coded `/bin/sh` produced a bare `sh` prompt on deployments that didn't
/// pass `--shell /bin/bash`.
fn default_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/bash".to_string())
}

impl ServerState {
    /// Create an empty server state.
    pub fn new(options: ServerOptions) -> Result<Self> {
        let shell_command = options.shell.unwrap_or_else(default_shell);
        let config = ConfigStore::new(
            options
                .config_path
                .unwrap_or_else(ConfigStore::default_path),
        )?;
        Ok(Self {
            shell_command,
            store: DashMap::new(),
            stats: SystemStats::new(),
            config: Arc::clone(&config),
            session_ttl: Duration::from_secs(options.session_ttl.unwrap_or(SESSION_RECLAIM_SECS)),
            proxies: ProxyRegistry::new(Some(config)),
            auth_limiter: RateLimiter::default(),
        })
    }

    /// Returns the shell command used for new sessions.
    pub fn shell_command(&self) -> String {
        self.shell_command.clone()
    }

    /// Returns the system statistics tracker.
    pub fn stats(&self) -> &SystemStats {
        &self.stats
    }

    /// Returns the encrypted configuration store.
    pub fn config(&self) -> Arc<ConfigStore> {
        Arc::clone(&self.config)
    }

    /// Returns the SOCKS5 tunnel registry.
    pub fn proxies(&self) -> &ProxyRegistry {
        &self.proxies
    }

    /// Returns the auth-failure rate limiter (login / change-password).
    pub fn auth_limiter(&self) -> &RateLimiter {
        &self.auth_limiter
    }

    /// Look up an active session by name.
    pub fn get(&self, name: &str) -> Option<Arc<Session>> {
        self.store.get(name).map(|e| Arc::clone(e.value()))
    }

    /// Atomically fetch the session `name` or create it via `init` and store
    /// it. Concurrent connections for the same key share one session (a
    /// refresh's overlapping connection never creates a duplicate or kills the
    /// live one).
    pub fn get_or_create(&self, name: &str, init: impl FnOnce() -> Arc<Session>) -> Arc<Session> {
        use dashmap::mapref::entry::Entry;
        match self.store.entry(name.to_string()) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                let session = init();
                entry.insert(Arc::clone(&session));
                session
            }
        }
    }

    /// Remove a session from the local store.
    pub fn unregister(&self, session: &Arc<Session>) {
        let name = session.name().to_string();
        if let Some((_, prev)) = self.store.remove(&name) {
            prev.shutdown();
        }
    }

    /// Reap sessions that have had no attached client for the idle TTL. Runs
    /// on a background sweeper; a session is only ever reclaimed while idle,
    /// so an attached client never sees its session shut down.
    pub fn reclaim_idle(&self) {
        let ttl = self.session_ttl;
        let now = Instant::now();
        let mut to_reclaim: Vec<String> = Vec::new();
        for entry in &self.store {
            let session = entry.value();
            if session.client_count() == 0 {
                if let Some(since) = session.idle_since() {
                    if now.duration_since(since) >= ttl {
                        to_reclaim.push(session.name().to_string());
                    }
                }
            }
        }
        for name in to_reclaim {
            if let Some((_, session)) = self.store.remove(&name) {
                session.shutdown();
            }
        }
    }

    /// Send a graceful shutdown signal to every session.
    pub fn shutdown(&self) {
        for entry in &self.store {
            entry.value().shutdown();
        }
        // Stop every SOCKS5 tunnel (closes their listeners + SSH connections).
        let proxies = self.proxies.clone();
        tokio::spawn(async move {
            proxies.shutdown_all().await;
        });
    }
}
