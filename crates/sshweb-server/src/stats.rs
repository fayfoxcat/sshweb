//! System statistics for the server (CPU, memory, network, time).
//!
//! CPU usage and network rates are derived from `/proc/stat` and
//! `/proc/net/dev` deltas between successive samples; memory usage is read
//! from `/proc/meminfo`. On unsupported platforms these report zero.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use serde::Serialize;

/// Tracks the previous samples to compute rates over an interval.
#[derive(Debug)]
pub struct SystemStats {
    last_busy: AtomicU64,
    last_total: AtomicU64,
    last_rx: AtomicU64,
    last_tx: AtomicU64,
    last_time: AtomicU64,
}

impl Default for SystemStats {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemStats {
    /// Create a new stats tracker, initializing the baseline sample.
    pub fn new() -> Self {
        let (busy, total) = cpu_counters();
        let (rx, tx) = network_counters();
        Self {
            last_busy: AtomicU64::new(busy),
            last_total: AtomicU64::new(total),
            last_rx: AtomicU64::new(rx),
            last_tx: AtomicU64::new(tx),
            last_time: AtomicU64::new(now_ms()),
        }
    }

    /// Sample the current CPU usage percentage (0.0 to 100.0).
    pub fn cpu_usage(&self) -> f64 {
        let (busy, total) = cpu_counters();
        let last_busy = self.last_busy.swap(busy, Ordering::Relaxed);
        let last_total = self.last_total.swap(total, Ordering::Relaxed);

        let busy_delta = busy.saturating_sub(last_busy);
        let total_delta = total.saturating_sub(last_total);
        if total_delta == 0 {
            0.0
        } else {
            (busy_delta as f64 / total_delta as f64) * 100.0
        }
    }

    /// Sample the current memory usage percentage (0.0 to 100.0).
    pub fn memory_usage(&self) -> f64 {
        memory_usage()
    }

    /// Sample the current (download, upload) rates in bytes/second.
    ///
    /// Both rates are computed from a single counter sample and one shared
    /// timestamp, so the two measurements never disturb each other (each
    /// `net_rate` call used to swap the timestamp, inflating the second rate).
    pub fn net_rates(&self) -> (f64, f64) {
        let (rx, tx) = network_counters();
        let last_rx = self.last_rx.swap(rx, Ordering::Relaxed);
        let last_tx = self.last_tx.swap(tx, Ordering::Relaxed);
        let now = now_ms();
        let last_time = self.last_time.swap(now, Ordering::Relaxed);
        let interval_ms = now.saturating_sub(last_time).max(1);
        let rx_rate = bytes_per_sec(rx.saturating_sub(last_rx), interval_ms);
        let tx_rate = bytes_per_sec(tx.saturating_sub(last_tx), interval_ms);
        (rx_rate, tx_rate)
    }

    /// Returns the current unix time in seconds.
    pub fn now(&self) -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Byte-rate of a counter delta over `dt_ms` milliseconds.
fn bytes_per_sec(delta: u64, dt_ms: u64) -> f64 {
    delta as f64 * 1000.0 / dt_ms.max(1) as f64
}

#[cfg(target_os = "linux")]
fn cpu_counters() -> (u64, u64) {
    let Ok(text) = std::fs::read_to_string("/proc/stat") else {
        return (0, 0);
    };
    let Some(line) = text.lines().next() else {
        return (0, 0);
    };
    let mut fields = line.split_whitespace();
    let _cpu = fields.next();
    let values: Vec<u64> = fields.filter_map(|x| x.parse().ok()).collect();
    if values.len() < 4 {
        return (0, 0);
    }
    // user nice system idle iowait irq softirq steal
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    let total: u64 = values.iter().sum();
    (total.saturating_sub(idle), total)
}

#[cfg(target_os = "linux")]
fn network_counters() -> (u64, u64) {
    // Sum receive and transmit bytes across all interfaces, excluding loopback.
    let Ok(text) = std::fs::read_to_string("/proc/net/dev") else {
        return (0, 0);
    };
    let mut rx = 0u64;
    let mut tx = 0u64;
    for line in text.lines().skip(2) {
        let mut parts = line.split_whitespace();
        let iface = parts.next().unwrap_or("");
        if iface.starts_with("lo:") {
            continue;
        }
        let values: Vec<u64> = parts.filter_map(|x| x.parse().ok()).collect();
        if values.len() >= 9 {
            rx += values[0]; // receive bytes
            tx += values[8]; // transmit bytes
        }
    }
    (rx, tx)
}

#[cfg(target_os = "linux")]
fn memory_usage() -> f64 {
    let Ok(text) = std::fs::read_to_string("/proc/meminfo") else {
        return 0.0;
    };
    let mut total = 0u64;
    let mut available = 0u64;
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or("");
        let value = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0);
        match key {
            "MemTotal:" => total = value,
            "MemAvailable:" => available = value,
            _ => {}
        }
    }
    if total == 0 {
        0.0
    } else {
        ((total.saturating_sub(available)) as f64 / total as f64) * 100.0
    }
}

#[cfg(not(target_os = "linux"))]
fn cpu_counters() -> (u64, u64) {
    (0, 0)
}

#[cfg(not(target_os = "linux"))]
fn network_counters() -> (u64, u64) {
    (0, 0)
}

#[cfg(not(target_os = "linux"))]
fn memory_usage() -> f64 {
    0.0
}

// ---- Remote host statistics ----------------------------------------------

/// Combined statistics for one host (local or remote), used by the nav bar
/// and serialized directly as the `/api/stats` payload.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HostStats {
    /// CPU usage percentage (0.0 to 100.0).
    pub cpu: f64,
    /// Memory usage percentage (0.0 to 100.0).
    pub memory: f64,
    /// Network download rate in bytes/second (host receive).
    pub down: f64,
    /// Network upload rate in bytes/second (host transmit).
    pub up: f64,
    /// Unix time in seconds.
    pub time: u64,
}

impl HostStats {
    /// Whether at least one sample has been received (`time` is set by the
    /// first sample, so a zero time means "not available yet").
    pub fn available(&self) -> bool {
        self.time != 0
    }
}

/// Remote sample collector: parses the tagged lines emitted by
/// [`REMOTE_STATS_CMD`] (`CS`/`MM`/`NR`/`TS`) and computes CPU/network rates
/// from deltas between successive samples.
#[derive(Debug, Default)]
pub struct RemoteStatsTracker {
    cpu_busy: Option<u64>,
    cpu_total: Option<u64>,
    rx: u64,
    tx: u64,
    memory: f64,
    prev_cpu: Option<(u64, u64)>,
    prev_rx: u64,
    prev_tx: u64,
    prev_time: u64,
    stats: HostStats,
}

impl RemoteStatsTracker {
    /// Feed one trimmed line of output from the remote sampler.
    pub fn push(&mut self, line: &str) {
        let mut parts = line.split_whitespace();
        let tag = parts.next().unwrap_or("");
        match tag {
            "CS" => {
                if let Some(busy) = parts.next().and_then(|x| x.parse().ok()) {
                    if let Some(total) = parts.next().and_then(|x| x.parse().ok()) {
                        self.cpu_busy = Some(busy);
                        self.cpu_total = Some(total);
                    }
                }
            }
            "MM" => {
                if let (Some(total), Some(avail)) = (
                    parts.next().and_then(|x| x.parse::<u64>().ok()),
                    parts.next().and_then(|x| x.parse::<u64>().ok()),
                ) {
                    if total > 0 {
                        self.memory = (total.saturating_sub(avail)) as f64 / total as f64 * 100.0;
                    }
                }
            }
            "NR" => {
                if let (Some(rx), Some(tx)) = (
                    parts.next().and_then(|x| x.parse::<u64>().ok()),
                    parts.next().and_then(|x| x.parse::<u64>().ok()),
                ) {
                    self.rx += rx;
                    self.tx += tx;
                }
            }
            "TS" => {
                if let Some(time) = parts.next().and_then(|x| x.parse::<u64>().ok()) {
                    self.finalize_sample(time);
                }
            }
            _ => {}
        }
    }

    /// Returns the latest computed statistics for the host.
    pub fn last(&self) -> &HostStats {
        &self.stats
    }

    /// Compute rates when a full sample (with a fresh timestamp) arrives.
    fn finalize_sample(&mut self, time: u64) {
        let mut cpu = 0.0;
        if let (Some(busy), Some(total)) = (self.cpu_busy, self.cpu_total) {
            if let Some((prev_busy, prev_total)) = self.prev_cpu {
                let busy_delta = busy.saturating_sub(prev_busy);
                let total_delta = total.saturating_sub(prev_total);
                if total_delta > 0 {
                    cpu = busy_delta as f64 / total_delta as f64 * 100.0;
                }
            }
            self.prev_cpu = Some((busy, total));
        }

        let dt = time.saturating_sub(self.prev_time);
        let (up, down) = if self.prev_time != 0 && dt > 0 {
            let rx_delta = self.rx.saturating_sub(self.prev_rx);
            let tx_delta = self.tx.saturating_sub(self.prev_tx);
            // `dt` is in seconds; `bytes_per_sec` takes milliseconds.
            (
                bytes_per_sec(tx_delta, dt * 1000),
                bytes_per_sec(rx_delta, dt * 1000),
            )
        } else {
            (0.0, 0.0)
        };

        // The first sample has no cpu/rate deltas yet; keep rates at zero but
        // publish the timestamp so the UI stops showing placeholders.
        self.stats = HostStats {
            cpu,
            memory: self.memory,
            down,
            up,
            time,
        };
        self.prev_rx = self.rx;
        self.prev_tx = self.tx;
        self.prev_time = time;
        self.rx = 0;
        self.tx = 0;
        self.cpu_busy = None;
        self.cpu_total = None;
    }
}

/// Command executed on the remote host to sample `/proc` once per second.
///
/// Each sample emits one `CS` (cpu busy/total jiffies), one `MM` (memtotal /
/// memory available in KB), an `NR` line per non-loopback interface (rx / tx
/// bytes) and one `TS` (unix time). The loop is a single long-lived channel so
/// no process is forked on the remote every second.
const REMOTE_STATS_CMD: &str = r#"while :; do
awk '
FILENAME ~ /\/stat$/ && /^cpu / { tot=0; idle=0; for (i=2;i<=NF;i++){ tot+=$i; if (i==5||i==6) idle+=$i } printf "CS %d %d\n", tot-idle, tot }
FILENAME ~ /\/meminfo$/ && /^MemTotal:/ { mt=$2 }
FILENAME ~ /\/meminfo$/ && /^MemAvailable:/ { printf "MM %d %d\n", mt, $2; has=1 }
FILENAME ~ /\/meminfo$/ && /^MemFree:/ { free=$2 }
FILENAME ~ /\/net\/dev$/ && /^ *[A-Za-z0-9_.-]+:/ && $1 != "lo:" && NF >= 10 { printf "NR %s %s\n", $2, $10 }
END { if (!has) printf "MM %d %d\n", mt, free; printf "TS %d\n", systime() }
' /proc/stat /proc/meminfo /proc/net/dev 2>/dev/null
sleep 1
done"#;

/// Open a second exec channel on the SSH connection that samples the remote
/// host's `/proc` once per second.
///
/// The command keeps running until the channel is closed; the caller reads the
/// data with [`russh::Channel::wait`] and feeds it to [`RemoteStatsDecoder`].
pub(crate) async fn stats_channel(
    handle: &russh::client::Handle<crate::ssh::SshHandler>,
) -> anyhow::Result<russh::Channel<russh::client::Msg>> {
    let channel = handle.channel_open_session().await?;
    channel.exec(true, REMOTE_STATS_CMD).await?;
    Ok(channel)
}

/// Incremental decoder for the remote sampler's tagged line stream.
#[derive(Debug, Default)]
pub struct RemoteStatsDecoder {
    tracker: RemoteStatsTracker,
    buf: crate::utils::LineBuffer,
    published_time: u64,
}

impl RemoteStatsDecoder {
    /// Feed a raw chunk of output from the stats channel.
    ///
    /// Returns `true` when a complete new sample was parsed (caller should
    /// publish `latest()` into the session).
    pub fn decode(&mut self, data: &[u8]) -> bool {
        let tracker = &mut self.tracker;
        let buf = &mut self.buf;
        buf.feed(data, |line| {
            if let Ok(text) = std::str::from_utf8(line) {
                tracker.push(text.trim());
            }
        });
        let time = self.tracker.last().time;
        if time != self.published_time {
            self.published_time = time;
            true
        } else {
            false
        }
    }

    /// The most recently computed stats, if any sample arrived yet.
    pub fn latest(&self) -> &HostStats {
        self.tracker.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(tracker: &mut RemoteStatsTracker, sample: &str) {
        for line in sample.lines() {
            tracker.push(line);
        }
    }

    #[test]
    fn remote_tracker_computes_rates() {
        let mut tracker = RemoteStatsTracker::default();
        lines(
            &mut tracker,
            "CS 100 200\nMM 8000 4000\nNR 1000 2000\nTS 100\n",
        );
        // First sample publishes the timestamp but has no deltas yet.
        let first = tracker.last().clone();
        assert!(first.available());
        assert_eq!(first.time, 100);
        assert_eq!(first.cpu, 0.0);
        assert_eq!(first.up, 0.0);
        lines(
            &mut tracker,
            "CS 200 300\nMM 8000 4000\nNR 1500 2500\nNR 100 100\nTS 101\n",
        );
        let stats = tracker.last().clone();
        assert!(stats.available());
        assert!((stats.cpu - 100.0).abs() < 0.01);
        assert!((stats.memory - 50.0).abs() < 0.01);
        assert!((stats.down - 600.0).abs() < 0.01);
        assert!((stats.up - 600.0).abs() < 0.01);
        assert_eq!(stats.time, 101);
    }

    #[test]
    fn remote_tracker_resets_between_samples() {
        let mut tracker = RemoteStatsTracker::default();
        lines(&mut tracker, "CS 100 200\nMM 8000 2000\nNR 10 10\nTS 100\n");
        lines(&mut tracker, "CS 200 300\nMM 8000 1000\nNR 20 20\nTS 101\n");
        let stats = tracker.last().clone();
        assert!((stats.memory - 87.5).abs() < 0.01);
        assert!((stats.down - 10.0).abs() < 0.01);
        assert!((stats.cpu - 100.0).abs() < 0.01);
    }
}
