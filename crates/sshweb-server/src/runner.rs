//! Asynchronous tasks that run a single shell with process I/O.

use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use sshweb_core::Sid;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};
use tracing::{debug, trace};

use crate::session::Session;
use crate::ssh;
use crate::terminal::Terminal;
use crate::web::protocol::ServerConfig;

/// Internal message routed to shell runners.
pub enum ShellData {
    /// Sequence of input bytes from the server.
    Data(Vec<u8>),
    /// Resize the shell to a different number of rows and columns.
    Size(u32, u32),
    /// Query the shell's current working directory. The runner injects
    /// `stty -echo` then a marked OSC `printf` (echo stays off, so only a
    /// harmless `stty -echo` line is echoed by readline — the printf itself
    /// and its OSC output are invisible), parses the OSC, and replies with
    /// [`crate::session::Session::report_pwd`].
    PwdRequest,
}

/// Asynchronous task to run a single shell with process I/O.
///
/// Reads output from the terminal and stores it in the session, from where it
/// is streamed to connected browsers. Incoming input is written to the PTY.
pub async fn shell_task(
    id: Sid,
    shell: String,
    cwd: Option<String>,
    mut shell_rx: mpsc::Receiver<ShellData>,
    session: Arc<Session>,
) -> Result<()> {
    let mut term = Terminal::new(&shell, cwd.as_deref()).await?;
    term.set_winsize(24, 80)?;
    debug!(%id, "spawning new shell");
    // Record the shell's pid so the session can read its live cwd via
    // /proc/<pid>/cwd (no terminal echo, unlike injecting a `pwd` command).
    session.set_shell_pid(id, term.pid());

    let mut buf = [0u8; 4096];
    let mut finished = false;

    while !finished {
        tokio::select! {
            result = term.read(&mut buf) => {
                // The PTY returns EIO when the shell's session leader exits.
                let n = match result {
                    Err(err)
                        if err.raw_os_error() == Some(5)
                            || err.to_string().contains("Input/output error") =>
                    {
                        trace!(%id, ?err, "shell read returned EIO");
                        0
                    }
                    result => result?,
                };
                if n == 0 {
                    finished = true;
                } else {
                    session.add_data(id, vec![Bytes::copy_from_slice(&buf[..n])])?;
                }
            }
            item = shell_rx.recv() => {
                match item {
                    Some(ShellData::Data(data)) => {
                        term.write_all(&data).await?;
                    }
                    Some(ShellData::Size(rows, cols)) => {
                        term.set_winsize(rows as u16, cols as u16)?;
                    }
                    // Local shells answer pwd via /proc/<pid>/cwd in
                    // `Session::pwd_request`, never through the runner.
                    Some(ShellData::PwdRequest) => {}
                    None => finished = true, // Server closed this shell.
                }
            }
        }
    }

    trace!(%id, "shell task finished");
    session.remove_shell(id);
    Ok(())
}

/// Asynchronous task that connects to a remote SSH server and proxies a
/// remote shell session between the browser and the SSH channel.
///
/// If the server declares a non-UTF-8 terminal encoding, remote output is
/// transcoded to UTF-8 and inbound input back to that encoding.
pub async fn ssh_task(
    id: Sid,
    server: ServerConfig,
    cwd: Option<String>,
    mut shell_rx: mpsc::Receiver<ShellData>,
    session: Arc<Session>,
) -> Result<()> {
    let handle = ssh::connect(&server, Some(&session.config())).await?;

    let channel = handle.channel_open_session().await?;
    channel
        .request_pty(true, "xterm-256color", 80, 24, 0, 0, &[])
        .await?;
    // Start the shell in the requested directory, if any.
    match cwd {
        Some(cwd) if !cwd.is_empty() => {
            let cmd = format!(
                "cd '{}' && exec ${{SHELL:-/bin/sh}}",
                crate::utils::shell_quote(&cwd)
            );
            channel
                .exec(true, cmd)
                .await
                .map_err(|e| anyhow::anyhow!("failed to start shell in directory: {e}"))?;
        }
        _ => {
            channel.request_shell(true).await?;
        }
    }

    // Second channel on the same connection: sample the remote host's
    // CPU / memory / network / time once per second (nav bar stats). Read in a
    // dedicated task so the shell's I/O loop is never blocked by it; it dies
    // with the connection when the shell task (and handle) is dropped.
    match crate::stats::stats_channel(&handle).await {
        Ok(channel) => {
            tokio::spawn(read_remote_stats(id, channel, Arc::clone(&session)));
        }
        Err(err) => {
            debug!(%id, ?err, "remote stats channel unavailable");
        }
    }

    let (mut read_half, write_half) = channel.split();

    // Startup snippet injection: one line at a time, paced so interactive
    // commands (`su - root` + password) work. A whole-snippet write fails
    // because the shell's readline consumes the buffered lines, leaving the
    // `su` password prompt with an empty tty buffer.
    let mut inject_lines: std::collections::VecDeque<String> =
        startup_snippet_lines(&server.startup)
            .unwrap_or_default()
            .into();
    let mut inject_deadline = tokio::time::Instant::now();
    let mut last_was_switch = false;
    let mut injecting = !inject_lines.is_empty();

    // Encoding transcoding (if not UTF-8).
    let encoding = encoding_rs::Encoding::for_label(server.encoding.as_bytes());
    let needs_transcode = encoding.map(|e| e != encoding_rs::UTF_8).unwrap_or(false);
    let mut decoder = encoding.map(|e| e.new_decoder());
    let mut encoder = encoding.map(|e| e.new_encoder());
    tracing::debug!(%id, enc = %server.encoding, transcode = needs_transcode, "terminal encoding");

    // Zero-intrusion `su` detection: watch the terminal output for an
    // interactive `su`/`sudo` command echo followed by a `Password:` prompt.
    // Used only to tell the user (the switched identity is NOT reused for a
    // new connection — see `Session::shell_detected_su`).
    let mut su_pending: Option<String> = None;
    let mut su_line_buf = crate::utils::LineBuffer::new();
    // Parse the current working directory from the shell's prompt line (e.g.
    // `linx:/home/elss_company#`), updated as the user `cd`s — zero terminal
    // pollution (no injection).
    let mut prompt_buf = crate::utils::LineBuffer::new();
    let mut last_cwd: Option<String> = None;

    let mut finished = false;
    while !finished {
        tokio::select! {
            _ = tokio::time::sleep_until(inject_deadline), if injecting => {
                if let Some(line) = inject_lines.pop_front() {
                    let body = format!("{line}\r");
                    if let Err(err) = write_half.data(body.as_bytes()).await {
                        debug!(%id, ?err, "failed to send startup snippet line");
                    }
                    // After a user-switch command wait briefly, then send the
                    // password; after any other line wait a short beat so the
                    // previous command is fully processed.
                    let is_switch = user_switch_line(&line).is_some();
                    let gap = if is_switch {
                        std::time::Duration::from_millis(250)
                    } else if last_was_switch {
                        std::time::Duration::from_millis(1000)
                    } else {
                        std::time::Duration::from_millis(350)
                    };
                    last_was_switch = is_switch;
                    inject_deadline = tokio::time::Instant::now() + gap;
                } else {
                    injecting = false;
                }
            }
            msg = read_half.wait() => {
                match crate::ssh::channel_event(msg) {
                    crate::ssh::ChannelEvent::Data(data) => {
                        if needs_transcode {
                            let decoder = decoder.as_mut().unwrap();
                            let mut text = String::new();
                            let _ = decoder.decode_to_string(&data, &mut text, false);
                            scan_su_output(id, &text, &mut su_pending, &mut su_line_buf, &session);
                            if let Some(cwd) = feed_prompt_cwd(&mut prompt_buf, &text) {
                                last_cwd = Some(cwd);
                            }
                            session.add_data(id, vec![Bytes::from(text.into_bytes())])?;
                        } else {
                            let text = String::from_utf8_lossy(&data).into_owned();
                            if let Some(cwd) = feed_prompt_cwd(&mut prompt_buf, &text) {
                                last_cwd = Some(cwd);
                            }
                            session.add_data(id, vec![data])?;
                            scan_su_output(id, &text, &mut su_pending, &mut su_line_buf, &session);
                        }
                    }
                    crate::ssh::ChannelEvent::Eof | crate::ssh::ChannelEvent::Closed => {
                        finished = true;
                    }
                    _ => {}
                }
            }
            item = shell_rx.recv() => {
                match item {
                    Some(ShellData::Data(data)) => {
                        if needs_transcode {
                            let encoder = encoder.as_mut().unwrap();
                            let mut out = Vec::new();
                            let _ = encoder.encode_from_utf8_without_replacement(
                                std::str::from_utf8(&data).unwrap_or(""),
                                &mut out,
                                false,
                            );
                            write_half.data(&out[..]).await?;
                        } else {
                            write_half.data(&data[..]).await?;
                        }
                    }
                    Some(ShellData::Size(rows, cols)) => {
                        write_half.window_change(cols, rows, 0, 0).await?;
                    }
                    Some(ShellData::PwdRequest) => {
                        // Answer from the last prompt-parsed cwd (no injection,
                        // zero terminal noise). Empty when no prompt was seen
                        // yet (fall back to the SFTP home on the client).
                        session.report_pwd(id, last_cwd.clone().unwrap_or_default());
                    }
                    None => {
                        write_half.eof().await?;
                        finished = true;
                    }
                }
            }
        }
    }

    trace!(%id, "ssh task finished");
    session.remove_shell(id);
    Ok(())
}

/// Split a startup snippet into the lines to type, skipping blanks and
/// comment-only lines. Returns `None` when nothing should be typed.
fn startup_snippet_lines(snippet: &str) -> Option<Vec<String>> {
    let lines: Vec<String> = snippet
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_string())
        .collect();
    if lines.is_empty() {
        None
    } else {
        Some(lines)
    }
}

/// Match a user-switch command line: `su`, `su -`, `su <user>`, `su - <user>`,
/// `sudo -i`, `sudo -iu <user>`, `sudo su` (plus the token parser's additional
/// `sudo -s` / `sudo -u <user>` forms). Used to pace the interactive password
/// line during startup injection (the snippet itself only affects the terminal
/// — SFTP never reads it).
///
/// The switch command must be the **first** token of the line — a startup line
/// like `echo su` must not trigger a password line. The shared token parser
/// [`switch_user_from_tokens`] additionally drives the output scanner, where
/// the echoed switch word can appear anywhere in the line.
fn user_switch_line(line: &str) -> Option<&str> {
    let tokens: Vec<&str> = line.trim().split_whitespace().collect();
    if matches!(tokens.first().copied(), Some("su") | Some("sudo")) {
        switch_user_from_tokens(&tokens)
    } else {
        None
    }
}

/// Read the remote stats channel, publishing one HostStats per second into the
/// session. Terminates once the channel closes (SSH connection gone) or the
/// shell is removed.
async fn read_remote_stats(
    id: Sid,
    channel: russh::Channel<russh::client::Msg>,
    session: Arc<Session>,
) {
    let (mut read_half, _write_half) = channel.split();
    let mut decoder = crate::stats::RemoteStatsDecoder::default();
    loop {
        match crate::ssh::channel_event(read_half.wait().await) {
            crate::ssh::ChannelEvent::Data(data) => {
                if decoder.decode(&data) {
                    session.set_remote_stats(id, decoder.latest().clone());
                    if !session.has_shell(id) {
                        debug!(%id, "remote stats stopped: shell removed");
                        break;
                    }
                }
            }
            crate::ssh::ChannelEvent::Eof | crate::ssh::ChannelEvent::Closed => {
                debug!(%id, "remote stats channel closed");
                break;
            }
            _ => {}
        }
    }
}

/// Feed terminal output through the su/sudo detection state machine. Matches
/// the echoed `su - <user>` command line followed by a password prompt.
fn scan_su_output(
    id: Sid,
    text: &str,
    pending: &mut Option<String>,
    line_buf: &mut crate::utils::LineBuffer,
    session: &Arc<Session>,
) {
    line_buf.feed(text.as_bytes(), |line| {
        let line = String::from_utf8_lossy(line);
        check_su_line(id, &line, pending, session);
    });
}

/// Examine one terminal output line for su/sudo signals.
fn check_su_line(id: Sid, line: &str, pending: &mut Option<String>, session: &Arc<Session>) {
    let clean = strip_ansi(line);
    let lower = clean.to_lowercase();
    if lower.contains("incorrect password") || lower.contains("authentication failure") {
        // A failed switch: forget the pending target.
        *pending = None;
        return;
    }
    if lower.contains("password") && lower.contains(':') {
        if let Some(user) = pending.take() {
            session.mark_su_detected(id, user);
        }
        return;
    }
    let tokens: Vec<&str> = clean.split_whitespace().collect();
    if let Some(user) = switch_user_from_tokens(&tokens) {
        *pending = Some(user.to_string());
    }
}

/// Extract the target user from a `su`/`sudo` command line (tokens). The
/// prompt (`user@host:~$`) is usually glued to the echo of the typed command
/// without a newline, so the switch word is located anywhere in the line.
fn switch_user_from_tokens<'a>(tokens: &[&'a str]) -> Option<&'a str> {
    let idx = tokens.iter().position(|t| matches!(*t, "su" | "sudo"))?;
    let parts = &tokens[idx..];
    let first = parts[0];
    if first == "su" {
        // `su`, `su -`, `su - <user>`, `su <user>`
        let rest = &parts[1..];
        if rest.is_empty() {
            return Some("root");
        }
        if rest == ["-"] {
            return Some("root");
        }
        if rest[0] == "-" {
            return rest.get(1).copied();
        }
        return rest.first().copied();
    }
    if first == "sudo" {
        let rest = &parts[1..];
        if rest.is_empty() {
            return None;
        }
        if rest == ["-i"] || rest == ["su"] || rest == ["-s"] {
            return Some("root");
        }
        if rest[0] == "-iu" {
            return rest.get(1).copied();
        }
        if rest[0] == "-u" {
            return rest.get(1).copied();
        }
        return None;
    }
    None
}

/// Strip ANSI escape sequences from a terminal output line.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c2 in chars.by_ref() {
                        if c2.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for c2 in chars.by_ref() {
                        if c2 == '\x07' || c2 == '\x1b' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Feed terminal output into the prompt-cwd parser: each complete line (and
/// the partial tail, where the cursor sits after a prompt like `…~$ `) is
/// checked for a shell prompt carrying the current directory. Returns the
/// parsed cwd when the line looks like a prompt (no ANSI, ends in `$`/`#`).
fn feed_prompt_cwd(buf: &mut crate::utils::LineBuffer, text: &str) -> Option<String> {
    buf.feed(text.as_bytes(), |_line| {});
    // The prompt is the last (usually incomplete) line; check both the
    // complete lines fed above and the leftover partial in the buffer.
    let leftover = buf.leftover();
    let clean = strip_ansi(&String::from_utf8_lossy(leftover))
        .trim()
        .to_string();
    extract_prompt_cwd(&clean)
}

/// Extract the working directory from a cleaned prompt line.
///
/// Accepts `[user@]host:path` followed by `$`/`#` (optionally with a trailing
/// space), where `path` is absolute, `~`, or `~/…` (prompts show `~` for the
/// home dir). Also accepts a bare absolute-path prompt like `/home/foo# `.
/// Returns `None` for output lines that aren't prompts.
fn extract_prompt_cwd(line: &str) -> Option<String> {
    let line = line.trim_end();
    if line.len() < 2 {
        return None;
    }
    let last = line.as_bytes()[line.len() - 1];
    if last != b'$' && last != b'#' {
        return None;
    }
    let body = &line[..line.len() - 1];
    let Some(colon) = body.rfind(':') else {
        // No colon: the whole body could be a bare path (`/x#` or `~#`).
        return if body.starts_with('/') || body == "~" || body.starts_with("~/") {
            if body.contains(' ') {
                None
            } else {
                Some(body.to_string())
            }
        } else {
            None
        };
    };
    let path = &body[colon + 1..];
    if (path.starts_with('/') || path == "~" || path.starts_with("~/")) && !path.contains(' ') {
        Some(path.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{switch_user_from_tokens, user_switch_line};

    #[test]
    fn user_switch_line_matches_command_forms() {
        // Every form the injection pacer must recognize.
        assert_eq!(user_switch_line("su"), Some("root"));
        assert_eq!(user_switch_line("su -"), Some("root"));
        assert_eq!(user_switch_line("su alice"), Some("alice"));
        assert_eq!(user_switch_line("su - alice"), Some("alice"));
        assert_eq!(user_switch_line("sudo -i"), Some("root"));
        assert_eq!(user_switch_line("sudo -iu alice"), Some("alice"));
        assert_eq!(user_switch_line("sudo su"), Some("root"));
        // Union additions from the token parser (previously unmatched).
        assert_eq!(user_switch_line("sudo -s"), Some("root"));
        assert_eq!(user_switch_line("sudo -u bob"), Some("bob"));
    }

    #[test]
    fn user_switch_line_requires_leading_command() {
        // A line whose first word is not su/sudo must never trigger a password
        // line (the merged parser must not treat `echo su` as a switch).
        assert_eq!(user_switch_line("echo su"), None);
        assert_eq!(user_switch_line("ls -l"), None);
        assert_eq!(user_switch_line(""), None);
        assert_eq!(user_switch_line("   "), None);
    }

    #[test]
    fn switch_user_from_tokens_finds_word_anywhere() {
        // The output scanner locates the switch word anywhere in the line.
        fn tokens(s: &str) -> Vec<&str> {
            s.split_whitespace().collect()
        }
        assert_eq!(switch_user_from_tokens(&tokens("su")), Some("root"));
        assert_eq!(switch_user_from_tokens(&tokens("su -")), Some("root"));
        assert_eq!(switch_user_from_tokens(&tokens("su alice")), Some("alice"));
        assert_eq!(
            switch_user_from_tokens(&tokens("su - alice")),
            Some("alice")
        );
        assert_eq!(switch_user_from_tokens(&tokens("sudo -i")), Some("root"));
        assert_eq!(
            switch_user_from_tokens(&tokens("sudo -iu alice")),
            Some("alice")
        );
        assert_eq!(switch_user_from_tokens(&tokens("sudo su")), Some("root"));
        assert_eq!(switch_user_from_tokens(&tokens("sudo -s")), Some("root"));
        assert_eq!(switch_user_from_tokens(&tokens("sudo -u bob")), Some("bob"));
        // Echoed prompt glued to the command, e.g. `alice@host:~$ su alice`.
        assert_eq!(
            switch_user_from_tokens(&tokens("alice@host:~$ su alice")),
            Some("alice")
        );
        assert_eq!(switch_user_from_tokens(&tokens("ls")), None);
    }
}
