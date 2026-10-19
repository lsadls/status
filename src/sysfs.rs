//! Minimal readers for the kernel pseudo-filesystems (`/sys`, `/proc`).
//!
//! Everything here is fallible but cheap: each call opens one small file.
//! Callers are expected to degrade a single metric to "unavailable" instead of
//! aborting the whole dashboard, because most of these files simply do not
//! exist on every machine (no battery, no thermal zone, virtual NIC, ...).
//!
//! `/proc` and `/sys` are not real files, so they must be polled. Every metric
//! that reports a *rate* is therefore computed from two snapshots taken by the
//! caller; see `metrics` for the sampling rules.

use std::{fs, io, path::Path};

/// Read a sysfs/procfs file as trimmed text.
pub fn read_trimmed(path: impl AsRef<Path>) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_string())
}

/// Read a file containing a single unsigned integer (e.g. `rx_bytes`,
/// `capacity`, `temp`).
pub fn read_u64(path: impl AsRef<Path>) -> io::Result<u64> {
    let text = read_trimmed(path)?;
    text.parse::<u64>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{text:?}: {e}")))
}

/// Read a file containing a single float (e.g. `current_now`).
pub fn read_f64(path: impl AsRef<Path>) -> io::Result<f64> {
    let text = read_trimmed(path)?;
    text.parse::<f64>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{text:?}: {e}")))
}

/// Compare a file's text content with `expected`, treating "missing" and
/// "unreadable" as `false`. Used for `carrier`/`status`-style files where a
/// read failure is a legitimate answer ("this interface is down").
pub fn reads_as(path: impl AsRef<Path>, expected: &str) -> bool {
    read_trimmed(path).is_ok_and(|text| text == expected)
}

/// Counter difference between two samples.
///
/// Counters are monotonically increasing, but the kernel can wrap them or the
/// backing device can be reset, making `now < prev`. The true delta is then
/// unknown; reporting `now` would present the whole post-reset total as one
/// interval's worth of traffic (a spurious spike), so a reset yields `0`.
pub fn counter_delta(prev: u64, now: u64) -> u64 {
    now.saturating_sub(prev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_delta_is_saturating() {
        assert_eq!(counter_delta(1_000, 1_500), 500);
        assert_eq!(counter_delta(1_000, 1_000), 0);
        // counter reset / wraparound: unknown, so report nothing rather than
        // pretending the post-reset total happened within one interval.
        assert_eq!(counter_delta(9_000, 10), 0);
    }

    #[test]
    fn missing_file_is_an_error_not_a_panic() {
        assert!(read_u64("/nonexistent/status-test/capacity").is_err());
        assert!(!reads_as("/nonexistent/status-test/carrier", "1"));
    }
}
