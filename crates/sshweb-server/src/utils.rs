//! Utility functions shared among server logic.

use std::fmt::Debug;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::Notify;

/// A cloneable structure that handles shutdown signals.
#[derive(Clone)]
pub struct Shutdown {
    inner: Arc<(AtomicBool, Notify)>,
}

impl Shutdown {
    /// Construct a new [`Shutdown`] object.
    pub fn new() -> Self {
        Self {
            inner: Arc::new((AtomicBool::new(false), Notify::new())),
        }
    }

    /// Send a shutdown signal to all listeners.
    pub fn shutdown(&self) {
        self.inner.0.swap(true, Ordering::Relaxed);
        self.inner.1.notify_waiters();
    }

    /// Wait for the shutdown signal, if it has not already been sent.
    pub fn wait(&'_ self) -> impl Future<Output = ()> + Send {
        let inner = self.inner.clone();
        async move {
            // Initial fast check
            if !inner.0.load(Ordering::Relaxed) {
                let notify = inner.1.notified();
                // Second check to avoid "missed wakeup" race conditions
                if !inner.0.load(Ordering::Relaxed) {
                    notify.await;
                }
            }
        }
    }
}

impl Debug for Shutdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shutdown")
            .field("is_terminated", &self.inner.0.load(Ordering::Relaxed))
            .finish()
    }
}

/// Incremental splitter for byte streams arriving in arbitrary chunks: drains
/// complete newline-terminated lines and leaves the partial tail buffered.
///
/// Used wherever line-oriented output must be parsed without assuming chunks
/// align to line boundaries (the remote stats sampler and the terminal's
/// `su`/`sudo` scanner).
#[derive(Debug, Default)]
pub struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    /// Construct an empty line buffer.
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    /// Feed bytes, invoking `on_line` with each complete line *including* its
    /// trailing newline, in order. A partial final line is kept for the next
    /// call.
    pub fn feed(&mut self, data: &[u8], mut on_line: impl FnMut(&[u8])) {
        self.buf.extend_from_slice(data);
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            on_line(&line);
        }
    }

    /// The current incomplete trailing line (no newline yet).
    pub fn leftover(&self) -> &[u8] {
        &self.buf
    }
}

/// Escape a value so it is safe to embed between single quotes in a POSIX
/// shell command (each `'` becomes `'\''`). Shared by the remote-shell `cd`
/// wrapper (`runner.rs`) and the key-install `authorized_keys` command
/// (`web/keys.rs`).
pub fn shell_quote(s: &str) -> String {
    s.replace('\'', "'\\''")
}

/// Resolve a configured/prompt working directory against a **known** home to an
/// absolute path: empty or `~` → that home; `~/…` and bare relative paths are
/// anchored at it; absolute paths pass through unchanged.
///
/// Shared by the remote SFTP browse start (`session/sftp_ops.rs`) and the local
/// start-directory resolver below, so a 「启动目录」/「家目录」 typed in the UI
/// behaves identically wherever it is used.
pub fn resolve_start_dir(dir: &str, home: &str) -> String {
    let dir = dir.trim();
    if dir.is_empty() || dir == "~" {
        return home.to_string();
    }
    let rest = if let Some(rest) = dir.strip_prefix("~/") {
        rest
    } else if dir.starts_with('/') {
        return dir.to_string();
    } else {
        // Bare relative path (e.g. `project`): anchored at the user's home.
        dir
    };
    if rest.is_empty() {
        return home.to_string();
    }
    if home.is_empty() {
        return dir.to_string();
    }
    format!("{}/{}", home.trim_end_matches('/'), rest)
}

/// The sshweb process's own home directory (`$HOME`), or `None` when
/// unset/empty.
///
/// Deliberately does **not** consult `/etc/passwd`: every shell is spawned as a
/// child of this process and inherits its environment, so the `~` a user types
/// in a local terminal expands to exactly this `$HOME` — resolving it any other
/// way could pick a start directory the shell itself would not call home.
pub fn local_home() -> Option<String> {
    std::env::var("HOME").ok().filter(|home| !home.is_empty())
}

/// The start directory for a **local** terminal and the local file browser
/// (「本机」设置的家目录, see 已知坑 81/82).
///
/// - An explicit `dir` (the file manager's "open a terminal here") always wins
///   and is passed through verbatim: it names a directory the user just listed,
///   and must keep the same semantics as a remote shell's start directory.
/// - Otherwise the configured `home` is expanded like a 「启动目录」 (empty /
///   `~` → `$HOME`, relative paths anchored at `$HOME`) and used only if it
///   actually **is a directory**.
/// - Anything else resolves to `None`, meaning "no start directory at all" —
///   the previous behavior (terminal: the process working directory; browser:
///   `current_dir()`, falling back to `/`).
///
/// The `is_dir()` check is not cosmetic: `terminal/unix.rs`'s `chdir` failure
/// makes `execv_child` return `Err`, the child exits 1, the PTY read returns
/// EIO and the tab disappears with **no** message. A typo in the configured
/// home would therefore look like "the terminal is broken", so it is ignored
/// with a log line instead (已知坑 82).
pub fn resolve_local_start_dir(explicit: Option<&str>, home: &str) -> Option<String> {
    if let Some(dir) = explicit.map(str::trim).filter(|dir| !dir.is_empty()) {
        return Some(dir.to_string());
    }
    let home = home.trim();
    if home.is_empty() {
        return None;
    }
    let resolved = resolve_start_dir(home, &local_home().unwrap_or_default());
    if resolved.is_empty() {
        return None;
    }
    if std::path::Path::new(&resolved).is_dir() {
        return Some(resolved);
    }
    tracing::warn!(
        dir = %resolved,
        "configured local home is not a directory; using the default start directory"
    );
    None
}

#[cfg(test)]
mod tests {
    use super::{resolve_local_start_dir, resolve_start_dir};

    #[test]
    fn resolve_start_dir_expands_against_home() {
        assert_eq!(resolve_start_dir("", "/home/me"), "/home/me");
        assert_eq!(resolve_start_dir("~", "/home/me"), "/home/me");
        assert_eq!(
            resolve_start_dir("~/project", "/home/me"),
            "/home/me/project"
        );
        assert_eq!(resolve_start_dir("project", "/home/me"), "/home/me/project");
        // Absolute paths are used verbatim.
        assert_eq!(resolve_start_dir("/data/app", "/home/me"), "/data/app");
        // Trailing slashes on the home are not doubled.
        assert_eq!(resolve_start_dir("~/x", "/home/me/"), "/home/me/x");
    }

    #[test]
    fn resolve_local_start_dir_prefers_the_explicit_dir() {
        // An explicit directory wins and is passed through untouched — the
        // caller already resolved it against a real listing.
        assert_eq!(
            resolve_local_start_dir(Some("/data/app"), "/nonexistent"),
            Some("/data/app".to_string())
        );
        assert_eq!(
            resolve_local_start_dir(Some("  /data/app  "), ""),
            Some("/data/app".to_string())
        );
    }

    #[test]
    fn resolve_local_start_dir_ignores_a_missing_configured_home() {
        // Empty config → no start directory (the previous behavior).
        assert_eq!(resolve_local_start_dir(None, ""), None);
        assert_eq!(resolve_local_start_dir(None, "   "), None);
        // A typo must not reach `chdir` (已知坑 82): the terminal would vanish
        // with no message. Absolute and `$HOME`-anchored forms are both checked,
        // so this holds whether or not `$HOME` is set in the test environment.
        assert_eq!(
            resolve_local_start_dir(None, "/definitely/not/a/real/dir"),
            None
        );
        assert_eq!(
            resolve_local_start_dir(None, "definitely-not-a-real-dir-xyz"),
            None
        );
    }

    #[test]
    fn resolve_local_start_dir_accepts_an_existing_dir() {
        let tmp = std::env::temp_dir();
        let expected = tmp.to_string_lossy().into_owned();
        assert_eq!(
            resolve_local_start_dir(None, &expected),
            Some(expected.clone())
        );
        // `~` expands to `$HOME` (via `local_home()`), so it is accepted exactly
        // when `$HOME` exists as a directory.
        if let Some(home) = super::local_home() {
            if std::path::Path::new(&home).is_dir() {
                assert_eq!(resolve_local_start_dir(None, "~"), Some(home.clone()));
                assert_eq!(
                    resolve_local_start_dir(None, "~/."),
                    Some(crate::utils::resolve_start_dir("~/.", &home))
                );
            }
        }
    }
}
