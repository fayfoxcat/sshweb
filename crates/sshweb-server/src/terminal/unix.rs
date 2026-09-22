use std::convert::Infallible;
use std::env;
use std::ffi::{CStr, CString};
use std::os::fd::{AsRawFd, RawFd};
use std::pin::Pin;
use std::task::{Context, Poll};

use anyhow::Result;
use close_fds::CloseFdsBuilder;
use nix::errno::Errno;
use nix::libc::{login_tty, TIOCSWINSZ};
use nix::pty::{self, Winsize};
use nix::sys::signal::{kill, Signal::SIGKILL};
use nix::sys::wait::waitpid;
use nix::unistd::{execvp, fork, ForkResult, Pid};
use pin_project::{pin_project, pinned_drop};
use tokio::fs::File;
use tokio::io::{self, AsyncRead, AsyncWrite};
use tracing::{instrument, trace};

/// An object that stores the state for a terminal session.
#[pin_project(PinnedDrop)]
pub struct Terminal {
    child: Pid,
    #[pin]
    master_read: File,
    #[pin]
    master_write: File,
}

impl Terminal {
    /// Create a new terminal, with attached PTY.
    ///
    /// `locale` is the `(variable, value)` the shell should be started with so
    /// that its environment selects a UTF-8 locale (已知坑 97), or `None` to
    /// leave the inherited environment alone. It is resolved by the caller, in
    /// the parent process — see `utils::local_shell_locale_fix`.
    #[instrument]
    pub async fn new(
        shell: &str,
        cwd: Option<&str>,
        locale: Option<(&'static str, &str)>,
    ) -> Result<Terminal> {
        let result = pty::openpty(None, None)?;

        // Logged in the parent: the child must not allocate or take the env lock
        // beyond the `set_var` calls it has always made (已知坑 97).
        if let Some((name, value)) = locale {
            tracing::debug!(%name, %value, "forcing a UTF-8 locale for the local shell");
        }

        // The slave file descriptor was created by openpty() and is forked here.
        let child = Self::fork_child(shell, cwd, result.slave.as_raw_fd(), locale)?;

        // We need to clone the file object to prevent livelocks in Tokio, when multiple
        // reads and writes happen concurrently on the same file descriptor. This is a
        // current limitation of how the `tokio::fs::File` struct is implemented, due to
        // its blocking I/O on a separate thread.
        let master_read = File::from(std::fs::File::from(result.master));
        let master_write = master_read.try_clone().await?;

        trace!(%child, "creating new terminal");

        Ok(Self {
            child,
            master_read,
            master_write,
        })
    }

    /// The PID of the forked child process (the shell). Used to read the
    /// shell's live working directory via `/proc/<pid>/cwd` — exact and with
    /// no terminal echo (unlike injecting a `pwd` command).
    pub fn pid(&self) -> i32 {
        self.child.as_raw()
    }

    /// Entry point for the child process, which spawns a shell.
    fn fork_child(
        shell: &str,
        cwd: Option<&str>,
        slave_port: RawFd,
        locale: Option<(&'static str, &str)>,
    ) -> Result<Pid> {
        let shell = CString::new(shell.to_owned())?;

        // Safety: This does not use any async-signal-unsafe operations in the child
        // branch, such as memory allocation.
        match unsafe { fork() }? {
            ForkResult::Parent { child } => Ok(child),
            ForkResult::Child => match Self::execv_child(&shell, cwd, slave_port, locale) {
                Ok(infallible) => match infallible {},
                Err(_) => std::process::exit(1),
            },
        }
    }

    fn execv_child(
        shell: &CStr,
        cwd: Option<&str>,
        slave_port: RawFd,
        locale: Option<(&'static str, &str)>,
    ) -> Result<Infallible, Errno> {
        // Safety: The slave file descriptor was created by openpty().
        Errno::result(unsafe { login_tty(slave_port) })?;
        // Safety: This is called immediately before an execv(), and there are no other
        // threads in this process to interact with its file descriptor table.
        unsafe { CloseFdsBuilder::new().closefrom(3) };

        // Change to the requested working directory before starting the shell.
        if let Some(cwd) = cwd {
            if !cwd.is_empty() {
                let cwd = std::ffi::CString::new(cwd).map_err(|_| Errno::EINVAL)?;
                Errno::result(unsafe { nix::libc::chdir(cwd.as_ptr()) })?;
            }
        }

        // Set terminal environment variables appropriately. Note that these
        // `set_var` calls allocate and take std's environment lock inside a
        // forked child of a multi-threaded process; that is only tolerable
        // because the child `execvp`s immediately and never touches the
        // environment again. The textbook alternative (build the whole envp in
        // the parent and call `execvpe`) is a separate change and is not needed
        // for correctness here.
        env::set_var("TERM", "xterm-256color");
        env::set_var("COLORTERM", "truecolor");
        env::set_var("TERM_PROGRAM", "sshweb");
        env::remove_var("TERM_PROGRAM_VERSION");

        // 已知坑 97: a local shell is the only shell in this product that does
        // not come from a login program — `ssh`/`su`/a desktop session get
        // their locale from PAM's `pam_env`, while this bare `execvp` inherits
        // sshweb's own environment. On a server started from an init script
        // that means `LANG=C` (or nothing at all = glibc's C locale), and
        // coreutils then escapes every non-ASCII filename as
        // `''$'\345\205\254\345\205\261'` instead of printing 中文. The value
        // is resolved by the parent before the fork and names the variable it
        // has to replace (`LC_ALL`/`LC_CTYPE`/`LANG`), because setting `LANG`
        // alone does nothing when a higher-precedence variable is in effect.
        if let Some((name, value)) = locale {
            env::set_var(name, value);
        }

        // Start the process as a *login* shell, so `/etc/profile` and
        // `/etc/profile.d/*.sh` are read exactly like `ssh host` does. Without
        // `-l` the shell only reads its own rc file, and anything injected into
        // the PATH by profile.d (a database client, a language toolchain) is
        // missing — the local twin of 已知坑 76. `-l` is understood by
        // bash/dash/zsh/ksh/fish alike; the dash-prefixed `argv[0]` convention
        // is equivalent for bash/dash but fish rejects it, so `-l` is what we
        // pass.
        execvp(shell, &[shell, c"-l"])
    }

    /// Get the window size of the TTY. Only used by unit tests (production
    /// resizing only ever sets the size), so it stays out of the shipped crate.
    #[cfg(test)]
    pub fn get_winsize(&self) -> Result<(u16, u16)> {
        use nix::libc::TIOCGWINSZ;
        nix::ioctl_read_bad!(ioctl_get_winsize, TIOCGWINSZ, Winsize);
        let mut winsize = make_winsize(0, 0);
        // Safety: The master file descriptor was created by openpty().
        unsafe { ioctl_get_winsize(self.master_read.as_raw_fd(), &mut winsize) }?;
        Ok((winsize.ws_row, winsize.ws_col))
    }

    /// Set the window size of the TTY.
    pub fn set_winsize(&mut self, rows: u16, cols: u16) -> Result<()> {
        nix::ioctl_write_ptr_bad!(ioctl_set_winsize, TIOCSWINSZ, Winsize);
        let winsize = make_winsize(rows, cols);
        // Safety: The master file descriptor was created by openpty().
        unsafe { ioctl_set_winsize(self.master_read.as_raw_fd(), &winsize) }?;
        Ok(())
    }
}

// Redirect terminal reads to the read file object.
impl AsyncRead for Terminal {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().master_read.poll_read(cx, buf)
    }
}

// Redirect terminal writes to the write file object.
impl AsyncWrite for Terminal {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.project().master_write.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project().master_write.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.project().master_write.poll_shutdown(cx)
    }
}

#[pinned_drop]
impl PinnedDrop for Terminal {
    fn drop(self: Pin<&mut Self>) {
        let this = self.project();
        let child = *this.child;
        trace!(%child, "dropping terminal");

        // Kill the child process on closure so that it doesn't keep running.
        kill(child, SIGKILL).ok();

        // Reap the zombie process in a background thread.
        std::thread::spawn(move || {
            waitpid(child, None).ok();
        });
    }
}

fn make_winsize(rows: u16, cols: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0, // ignored
        ws_ypixel: 0, // ignored
    }
}
