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
