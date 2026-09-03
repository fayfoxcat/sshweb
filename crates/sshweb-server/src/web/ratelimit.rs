//! Per-IP sliding-window rate limiting for the authentication endpoints.
//!
//! The page access password is the single credential guarding a host shell, so
//! online brute-force must be throttled (安全审查 M1). Failures are recorded
//! per source IP; once too many accumulate inside a window, further attempts
//! are rejected until the window slides clear. A successful login clears the
//! IP's history.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Max failed auth attempts per IP inside the window before blocking.
pub const AUTH_MAX_FAILURES: usize = 10;
/// The sliding window for auth failures (15 minutes).
pub const AUTH_WINDOW: Duration = Duration::from_secs(15 * 60);

/// A sliding-window failure counter keyed by client IP.
pub struct RateLimiter {
    inner: Mutex<HashMap<IpAddr, Vec<Instant>>>,
    max_failures: usize,
    window: Duration,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(AUTH_MAX_FAILURES, AUTH_WINDOW)
    }
}

impl RateLimiter {
    pub fn new(max_failures: usize, window: Duration) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            max_failures,
            window,
        }
    }

    /// Whether the IP has exceeded the failure budget inside the window.
    pub fn is_blocked(&self, ip: IpAddr) -> bool {
        let mut map = self.inner.lock();
        prune(&mut map, self.window);
        map.get(&ip)
            .map(|failures| failures.len() >= self.max_failures)
            .unwrap_or(false)
    }

    /// Record a failed attempt for the IP.
    pub fn record_failure(&self, ip: IpAddr) {
        let mut map = self.inner.lock();
        prune(&mut map, self.window);
        map.entry(ip).or_default().push(Instant::now());
    }

    /// Clear the IP's failure history (on a successful login).
    pub fn clear(&self, ip: IpAddr) {
        self.inner.lock().remove(&ip);
    }
}

/// Drop entries older than the window, and empty buckets.
fn prune(map: &mut HashMap<IpAddr, Vec<Instant>>, window: Duration) {
    let cutoff = Instant::now() - window;
    map.retain(|_, failures| {
        failures.retain(|t| *t > cutoff);
        !failures.is_empty()
    });
}
