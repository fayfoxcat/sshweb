//! Shell history for the 「命令片段」 sidebar's history mode.
//!
//! The server reads the shell's history **file** on whichever host the shell
//! runs on — `std::fs` for a local shell, SFTP for a remote one — and answers
//! with [`WsServer::CommandHistory`]. Nothing is injected into the terminal:
//! a `history` command would echo into the scrollback, pollute the command
//! timestamp overlay and depend on the shell's own settings; reading the file
//! is silent and works identically for both shell kinds.
//!
//! Known limits (documented in 开发文档 §五 坑 106):
//! - only the *default* file names are probed; a custom `HISTFILE` is
//!   invisible.
//! - bash writes a command when the shell exits (or after `history -a`), so the
//!   command just typed is not in the file yet — the frontend merges this list
//!   with the commands it saw submitted in the current page (see
//!   `snippets.ts`).
//! - bash stores a multi-line command as separate lines unless `lithist` is
//!   set; zsh uses `: <ts>:<dur>;cmd` with `\`-continued lines, which is
//!   unwrapped here.

use anyhow::Result;
use sshweb_core::Sid;

use super::Session;
use crate::web::protocol::WsServer;

/// Only this much of the end of a history file is parsed. History files grow
/// forever and only the recent entries are useful for the sidebar; a big grep
/// in someone's `~/.bash_history` must not allocate hundreds of MB.
const MAX_HISTORY_BYTES: u64 = 2 << 20;

/// Entries kept after deduplication. The sidebar filters client-side, so a
/// generous cap costs nothing and a huge one only slows the CBOR message down.
const MAX_HISTORY_ENTRIES: usize = 800;

/// History file names probed in order, relative to the login home. bash and
/// zsh are the two shells that actually write one by default; `.history` is
/// what ksh/tcsh and various `HISTFILE` setups use.
const HISTORY_FILES: [&str; 3] = [".bash_history", ".zsh_history", ".history"];

impl Session {
    /// Read the shell's history and answer with
    /// [`WsServer::CommandHistory`]. Never fails: every problem is reported
    /// through the reply's `notice` field so the sidebar always gets an
    /// answer (an unanswered request would leave its button spinning).
    pub async fn command_history(&self, id: Sid) {
        let (entries, notice) = match self.history_files(id).await {
            Ok((entries, notice)) => (entries, notice),
            Err(err) => (Vec::new(), Some(format!("读取历史命令失败：{err:#}"))),
        };
        self.send(WsServer::CommandHistory(id, entries, notice));
    }

    /// Locate and parse the first history file that exists for this shell.
    async fn history_files(&self, id: Sid) -> Result<(Vec<String>, Option<String>)> {
        match self.shell_target(id) {
            Some((server, pool)) => {
                let (client, home, _user) = pool
                    .client_probe(&server)
                    .await
                    .map_err(|err| err.into_error())?;
                for name in HISTORY_FILES {
                    let path = join_home(&home, name);
                    let mut fs = client.fs();
                    if fs.symlink_metadata(&path).await.is_err() {
                        continue;
                    }
                    let mut file = client
                        .open(&path)
                        .await
                        .map_err(|err| anyhow::anyhow!("打开 {path} 失败：{err}"))?;
                    let text = read_tail_remote(&mut file).await?;
                    return Ok((parse_history(&text), None));
                }
                Ok((Vec::new(), None))
            }
            None => {
                let Some(home) = crate::utils::local_home() else {
                    return Ok((Vec::new(), None));
                };
                for name in HISTORY_FILES {
                    let path = join_home(&home, name);
                    if !std::path::Path::new(&path).is_file() {
                        continue;
                    }
                    let text = Self::run_local(
                        {
                            let path = path.clone();
                            move || read_tail_local(&path)
                        },
                        "history",
                    )
                    .await?;
                    return Ok((parse_history(&text), None));
                }
                Ok((Vec::new(), None))
            }
        }
    }
}

/// `<home>/<name>`, tolerating a home with a trailing slash.
fn join_home(home: &str, name: &str) -> String {
    if home.is_empty() {
        return name.to_string();
    }
    format!("{}/{}", home.trim_end_matches('/'), name)
}

/// Read at most the last [`MAX_HISTORY_BYTES`] of a local history file
/// (off-thread: the caller runs this inside `spawn_blocking`).
fn read_tail_local(path: &str) -> Result<String> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file =
        std::fs::File::open(path).map_err(|err| anyhow::anyhow!("打开 {path} 失败：{err}"))?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let skipped = len.saturating_sub(MAX_HISTORY_BYTES);
    if skipped > 0 {
        file.seek(SeekFrom::Start(skipped))?;
    }
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    Ok(drop_partial_first_line(&buf, skipped > 0))
}

/// Read at most the last [`MAX_HISTORY_BYTES`] of a remote history file over
/// the shell's SFTP connection.
async fn read_tail_remote(file: &mut openssh_sftp_client::file::File) -> Result<String> {
    use std::io::SeekFrom;

    use tokio::io::AsyncSeekExt;

    let size = file
        .metadata()
        .await
        .map(|meta| meta.len().unwrap_or(0))
        .unwrap_or(0);
    let skipped = size.saturating_sub(MAX_HISTORY_BYTES);
    if skipped > 0 {
        file.seek(SeekFrom::Start(skipped)).await?;
    }
    let mut buf = Vec::new();
    loop {
        let want = (MAX_HISTORY_BYTES as usize - buf.len()).min(256 * 1024);
        if want == 0 {
            break;
        }
        let Some(data) = file
            .read(want as u32, bytes::BytesMut::with_capacity(want))
            .await?
        else {
            break; // EOF
        };
        if data.is_empty() {
            break;
        }
        buf.extend_from_slice(&data);
    }
    Ok(drop_partial_first_line(&buf, skipped > 0))
}

/// Decode the tail bytes as text, dropping the first (probably half) line when
/// the read started mid-file.
fn drop_partial_first_line(buf: &[u8], truncated: bool) -> String {
    let text = String::from_utf8_lossy(buf);
    if !truncated {
        return text.into_owned();
    }
    match text.find('\n') {
        Some(at) => text[at + 1..].to_string(),
        None => String::new(),
    }
}

/// Parse history file contents into sidebar entries: **most recent first**,
/// deduplicated, empty/comment lines dropped.
///
/// Two formats are handled:
/// - bash / sh: one command per line. `#<timestamp>` lines are bash's
///   `HISTTIMEFORMAT` markers, not commands.
/// - zsh: optionally `: <timestamp>:<duration>;<command>`, with `\`-continued
///   lines for multi-line commands.
pub fn parse_history(text: &str) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;

    for raw in text.lines() {
        // zsh continuation: the entry is not finished yet.
        let line = raw.trim_end_matches('\r');
        if let Some(mut acc) = pending.take() {
            let continued = line.ends_with('\\');
            let piece = line.trim_end_matches('\\');
            acc.push('\n');
            acc.push_str(piece);
            if continued {
                pending = Some(acc);
            } else {
                entries.push(acc);
            }
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let body = strip_zsh_prefix(line);
        if body.is_empty() {
            continue;
        }
        if body.ends_with('\\') {
            pending = Some(body.trim_end_matches('\\').to_string());
        } else {
            entries.push(body.to_string());
        }
    }
    if let Some(last) = pending {
        entries.push(last);
    }

    // Most recent first, then dedupe keeping the newest occurrence.
    let mut seen = std::collections::HashSet::new();
    entries
        .into_iter()
        .rev()
        .map(|entry| entry.trim().to_string())
        .filter(|entry| !entry.is_empty())
        .filter(|entry| seen.insert(entry.clone()))
        .take(MAX_HISTORY_ENTRIES)
        .collect()
}

/// Strip zsh's extended-history prefix (`: 1699999999:12;ls -la`).
fn strip_zsh_prefix(line: &str) -> &str {
    let Some(rest) = line.strip_prefix(": ") else {
        return line;
    };
    match rest.split_once(';') {
        Some((meta, cmd)) if meta.chars().all(|c| c.is_ascii_digit() || c == ':') => cmd,
        _ => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_bash_history_newest_first() {
        let text = "ls -la\ncd /srv/app\n\ngit status\n";
        // Dedupe + reverse: the newest line is first, empties dropped.
        assert_eq!(
            parse_history(text),
            vec!["git status", "cd /srv/app", "ls -la"]
        );
    }

    #[test]
    fn drops_bash_timestamp_markers() {
        let text = "#1699999999\nls\n#1700000000\ndf -h\n";
        assert_eq!(parse_history(text), vec!["df -h", "ls"]);
    }

    #[test]
    fn deduplicates_keeping_the_newest() {
        // The same command run twice appears once, and it keeps its newest
        // position (the order a human reads the list in).
        let text = "a\nb\na\nc\n";
        assert_eq!(parse_history(text), vec!["c", "a", "b"]);
    }

    #[test]
    fn parses_zsh_extended_history() {
        let text = ": 1699999999:0;ls -la\n: 1700000000:3;cd /srv\n";
        assert_eq!(parse_history(text), vec!["cd /srv", "ls -la"]);
    }

    #[test]
    fn joins_zsh_continued_lines() {
        // zsh escapes the newline inside a multi-line command with a
        // trailing backslash; the entry is one command, not two.
        let text = ": 1700000000:0;echo a \\\nb\n";
        assert_eq!(parse_history(text), vec!["echo a \nb"]);
    }

    #[test]
    fn a_line_that_is_not_a_zsh_prefix_is_kept_verbatim() {
        // A command that merely starts with `: ` must not lose its head.
        assert_eq!(
            parse_history(": not a timestamp\n"),
            vec![": not a timestamp"]
        );
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(parse_history("").is_empty());
        assert!(parse_history("\n\n  \n").is_empty());
    }

    #[test]
    fn join_home_tolerates_trailing_slash() {
        assert_eq!(
            join_home("/home/me", ".bash_history"),
            "/home/me/.bash_history"
        );
        assert_eq!(
            join_home("/home/me/", ".zsh_history"),
            "/home/me/.zsh_history"
        );
    }

    #[test]
    fn drop_partial_first_line_cuts_the_half_line() {
        let text = drop_partial_first_line(b"alf\nls -la\n", true);
        assert_eq!(text, "ls -la\n");
        // Not truncated: keep everything.
        assert_eq!(drop_partial_first_line(b"ls -la\n", false), "ls -la\n");
    }
}
