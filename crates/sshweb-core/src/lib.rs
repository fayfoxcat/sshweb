//! The core crate for shared code used in the sshweb application.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt::Display;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::{Deserialize, Serialize};

/// Generate a cryptographically-secure, random alphanumeric value.
pub fn rand_alphanumeric(len: usize) -> String {
    use rand::{distributions::Alphanumeric, thread_rng, Rng};
    thread_rng()
        .sample_iter(Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

/// Unique identifier for a shell within the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sid(pub u32);

impl Display for Sid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A counter for generating unique identifiers.
#[derive(Debug)]
pub struct IdCounter {
    next_sid: AtomicU32,
}

impl Default for IdCounter {
    fn default() -> Self {
        Self {
            next_sid: AtomicU32::new(1),
        }
    }
}

impl IdCounter {
    /// Returns the next unique shell ID.
    pub fn next_sid(&self) -> Sid {
        Sid(self.next_sid.fetch_add(1, Ordering::Relaxed))
    }
}
