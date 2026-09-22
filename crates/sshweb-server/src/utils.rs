//! Utility functions shared among server logic.

use std::fmt::Debug;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::fs;
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

/// The locale a local shell is forced to when the ambient locale is not UTF-8
/// (已知坑 97).
///
/// `C.UTF-8` is the one UTF-8 locale that is otherwise byte-identical to `C`,
/// which is what makes it the right **fallback** — `en_US.UTF-8` would also
/// change collation, month names and message language on a box that never asked
/// for it. Built into glibc ≥2.35 and shipped by Debian/Ubuntu well before
/// that.
pub const FALLBACK_UTF8_LOCALE: &str = "C.UTF-8";

/// Locale files consulted, in order, for a UTF-8 locale to hand to a local
/// shell: the file `pam_env` reads for every real login on Debian/Ubuntu, then
/// the RHEL/systemd equivalent.
const LOCALE_FILES: [&str; 2] = ["/etc/default/locale", "/etc/locale.conf"];

/// Whether a locale **name** selects the UTF-8 codeset — `C.UTF-8`,
/// `zh_CN.utf8`, `en_US.UTF-8@euro`, or a bare `UTF-8`.
///
/// Only the codeset matters (the part after the last `.`, minus any
/// `@modifier`): `en_US` with no codeset at all is the C locale as far as ctype
/// is concerned, and a name we do not recognize has to count as "not UTF-8" so
/// that it still gets replaced.
pub fn is_utf8_locale(locale: &str) -> bool {
    let locale = locale.trim();
    let codeset = locale.rsplit_once('.').map_or(locale, |(_, cs)| cs);
    let codeset = codeset.split('@').next().unwrap_or(codeset);
    matches!(
        codeset.to_ascii_lowercase().replace('_', "-").as_str(),
        "utf-8" | "utf8"
    )
}

/// Find a UTF-8 locale name in the contents of a locale file
/// (`/etc/default/locale`, `/etc/locale.conf`).
///
/// The format is `KEY=VALUE` lines with `#` comments and an optional `export `
/// prefix, and `update-locale` writes the value **quoted**
/// (`LANG="zh_CN.UTF-8"`). Only `LC_ALL`/`LC_CTYPE`/`LANG` select a locale —
/// `LANGUAGE` is a gettext setting and must be ignored.
///
/// The result is deliberately *not* the file's effective locale but any UTF-8
/// name it offers: a non-UTF-8 `LC_ALL` must not shadow a UTF-8 `LANG`, and the
/// point is to give the shell the name a login on this box would use (which
/// keeps collation and messages localized) rather than dropping to `C.UTF-8`.
/// Later lines win over earlier ones for the same key, and the keys are then
/// resolved in glibc's precedence order.
pub fn utf8_locale_in_file(contents: &str) -> Option<String> {
    let mut found: [Option<String>; 3] = [None, None, None];
    for line in contents.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        if line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let index = match key.trim() {
            "LC_ALL" => 0,
            "LC_CTYPE" => 1,
            "LANG" => 2,
            _ => continue,
        };
        // An inline `#` is *not* stripped: a locale name cannot contain one, so
        // stripping would only add a way to mis-parse the value.
        let value = value.trim().trim_matches(['"', '\'']).trim();
        if is_utf8_locale(value) {
            found[index] = Some(value.to_string());
        }
    }
    let [lc_all, lc_ctype, lang] = found;
    lc_all.or(lc_ctype).or(lang)
}

/// The locale to force on a local shell, or `None` when nothing needs doing.
///
/// `ambient` is the locale currently in effect for the child (see
/// [`locale_var`]); an already-UTF-8 value is left completely alone —
/// overwriting a working `zh_CN.UTF-8` with `C.UTF-8` would silently change
/// collation, month names and message language. Otherwise the box's own locale
/// file wins (that is what PAM hands a real login), falling back to
/// [`FALLBACK_UTF8_LOCALE`].
pub fn locale_fix(ambient: Option<&str>, file_locale: Option<&str>) -> Option<String> {
    if ambient.is_some_and(is_utf8_locale) {
        return None;
    }
    Some(file_locale.map_or_else(|| FALLBACK_UTF8_LOCALE.to_string(), str::to_string))
}

/// Which variable to upgrade: glibc resolves the `LC_CTYPE` category as
/// `LC_ALL` → `LC_CTYPE` → `LANG`, so only the **highest-precedence non-empty**
/// one has to change — setting `LANG` while `LC_CTYPE=C` is set would do
/// nothing at all.
///
/// An empty value counts as unset (glibc ignores it: `env -i LC_ALL=
/// LANG=zh_CN.UTF-8 locale` reports `LC_CTYPE="zh_CN.UTF-8"`), so `probe` is
/// expected to filter those out; with none set, `LANG` is the one to use.
pub fn locale_var(probe: impl Fn(&str) -> Option<String>) -> &'static str {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find(|name| probe(name).is_some_and(|value| !value.is_empty()))
        .unwrap_or("LANG")
}

/// The `(variable, value)` a **local** shell must be given so that its
/// environment selects a UTF-8 locale, or `None` when it already does
/// (已知坑 97).
///
/// A local PTY shell is the product's only shell that does not come from a
/// login program: `ssh`/`su`/a desktop session get their locale from PAM
/// (`pam_env.so … envfile=/etc/default/locale`), while
/// `terminal/unix.rs::execv_child` is a bare `fork` + `execvp` that inherits
/// this process's environment. A server started from an init script therefore
/// hands its shells `LANG=C` — or nothing at all, which is glibc's C locale —
/// and every non-ASCII filename shows up as coreutils' shell-escape quoting
/// (`''$'\345\205\254\345\205\261'`) instead of 中文. The variable name travels
/// with the value on purpose: setting `LANG` alone is a no-op whenever `LC_ALL`
/// or `LC_CTYPE` is the one in effect.
///
/// Also called from the parent process, before `fork`: the child must not read
/// files or allocate.
pub async fn local_shell_locale_fix() -> Option<(&'static str, String)> {
    let var = locale_var(|name| std::env::var(name).ok());
    let ambient = std::env::var(var).ok().filter(|value| !value.is_empty());
    if ambient.as_deref().is_some_and(is_utf8_locale) {
        return None; // Already fine: no file I/O on the common path.
    }
    let mut file_locale = None;
    for path in LOCALE_FILES {
        // A missing file is the norm on the "other" distribution, so failures
        // are not logged.
        if let Ok(contents) = fs::read_to_string(path).await {
            if let Some(value) = utf8_locale_in_file(&contents) {
                file_locale = Some(value);
                break;
            }
        }
    }
    Some((var, locale_fix(ambient.as_deref(), file_locale.as_deref())?))
}

#[cfg(test)]
mod tests {
    use super::{
        is_utf8_locale, locale_fix, locale_var, resolve_local_start_dir, resolve_start_dir,
        utf8_locale_in_file,
    };

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

    #[test]
    fn is_utf8_locale_keys_on_the_codeset() {
        for locale in [
            "C.UTF-8",
            "C.utf8",
            "c.utf8",
            "zh_CN.UTF-8",
            "zh_CN.utf8",
            "en_US.UTF-8@euro",
            "UTF-8",
            "  utf-8  ",
        ] {
            assert!(is_utf8_locale(locale), "{locale} should be UTF-8");
        }
        for locale in [
            "",
            " ",
            "C",
            "POSIX",
            "en_US",
            "zh_CN.GBK",
            "en_US.ISO-8859-1",
            "en_US.utf-8x",
        ] {
            assert!(!is_utf8_locale(locale), "{locale} should not be UTF-8");
        }
    }

    #[test]
    fn utf8_locale_in_file_parses_the_files_pam_env_reads() {
        // `update-locale` quotes the value; comments and blank lines are skipped.
        assert_eq!(
            utf8_locale_in_file("# File generated by update-locale\n\nLANG=\"zh_CN.UTF-8\"\n"),
            Some("zh_CN.UTF-8".to_string())
        );
        // Single quotes, no quotes, CRLF, and an `export` prefix all appear in
        // the wild (`pam_env` accepts the last one even though `update-locale`
        // never writes it).
        assert_eq!(
            utf8_locale_in_file("LANG='en_US.UTF-8'\r\n"),
            Some("en_US.UTF-8".to_string())
        );
        assert_eq!(
            utf8_locale_in_file("export LANG=C.UTF-8\n"),
            Some("C.UTF-8".to_string())
        );
        // A non-UTF-8 `LC_ALL` must not shadow a UTF-8 `LANG`: the point is to
        // find *a* usable name, not the file's effective locale.
        assert_eq!(
            utf8_locale_in_file("LC_ALL=C\nLANG=zh_CN.UTF-8\n"),
            Some("zh_CN.UTF-8".to_string())
        );
        // Later lines win for the same key, and `LC_ALL` outranks `LANG`.
        assert_eq!(
            utf8_locale_in_file("LANG=en_US.UTF-8\nLANG=C.UTF-8\n"),
            Some("C.UTF-8".to_string())
        );
        assert_eq!(
            utf8_locale_in_file("LANG=en_US.UTF-8\nLC_ALL=C.utf8\n"),
            Some("C.utf8".to_string())
        );
        // `LANGUAGE` is gettext, not a locale selector.
        assert_eq!(utf8_locale_in_file("LANGUAGE=zh_CN\n"), None);
        // Nothing usable.
        assert_eq!(utf8_locale_in_file("LANG=C\nLC_ALL=POSIX\n"), None);
        assert_eq!(utf8_locale_in_file("LANG=\n"), None);
        assert_eq!(utf8_locale_in_file(""), None);
    }

    #[test]
    fn locale_fix_leaves_a_working_locale_alone() {
        // Already UTF-8 → nothing to do, whatever the box's file says.
        assert_eq!(locale_fix(Some("zh_CN.UTF-8"), Some("C")), None);
        assert_eq!(locale_fix(Some("C.UTF-8"), None), None);
        // Not UTF-8, or not set at all (glibc's C locale) → fix it, preferring
        // the box's own name over the fallback.
        assert_eq!(
            locale_fix(Some("C"), Some("zh_CN.UTF-8")),
            Some("zh_CN.UTF-8".to_string())
        );
        assert_eq!(locale_fix(Some("POSIX"), None), Some("C.UTF-8".to_string()));
        assert_eq!(locale_fix(None, None), Some("C.UTF-8".to_string()));
        assert_eq!(
            locale_fix(None, Some("en_US.UTF-8")),
            Some("en_US.UTF-8".to_string())
        );
    }

    #[test]
    fn locale_var_follows_glibc_precedence() {
        fn probe<'a>(values: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
            move |name| {
                values
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        }

        // The highest-precedence non-empty variable is the one worth changing.
        assert_eq!(
            locale_var(probe(&[("LC_ALL", "C"), ("LC_CTYPE", "C"), ("LANG", "C")])),
            "LC_ALL"
        );
        assert_eq!(
            locale_var(probe(&[("LC_CTYPE", "C"), ("LANG", "C.UTF-8")])),
            "LC_CTYPE"
        );
        // An empty value counts as unset (glibc ignores it).
        assert_eq!(
            locale_var(probe(&[("LC_ALL", ""), ("LANG", "zh_CN.UTF-8")])),
            "LANG"
        );
        // Nothing set at all → `LANG`.
        assert_eq!(locale_var(probe(&[])), "LANG");
    }
}
