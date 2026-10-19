//! Memory usage from `/proc/meminfo`.

use crate::units::Amount;
use std::path::PathBuf;

/// Which figure the `[mem] figure` option selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Figure {
    /// `MemTotal - MemAvailable`: memory in use.
    Used,
    /// `MemAvailable`: memory a new workload could claim.
    Available,
    /// `MemTotal`.
    Total,
}

impl Figure {
    /// Map the config value: `0 = used, 1 = avail, 2 = total`.
    /// Unknown values fall back to `used`, matching the historical default.
    pub fn from_config(value: u8) -> Self {
        match value {
            1 => Figure::Available,
            2 => Figure::Total,
            _ => Figure::Used,
        }
    }
}

/// Memory snapshot in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub total_kib: u64,
    pub available_kib: u64,
}

impl Sample {
    /// KiB in use, saturating at zero so a kernel that reports
    /// `MemAvailable > MemTotal` cannot wrap around.
    pub fn used_kib(&self) -> u64 {
        self.total_kib.saturating_sub(self.available_kib)
    }

    /// KiB for the configured figure.
    pub fn select(&self, figure: Figure) -> u64 {
        match figure {
            Figure::Used => self.used_kib(),
            Figure::Available => self.available_kib,
            Figure::Total => self.total_kib,
        }
    }
}

/// Reads `/proc/meminfo`.
#[derive(Debug)]
pub struct Mem {
    figure: Figure,
    /// Display format for the memory figure (`[mem] unit` / `suffix`).
    amount: Amount,
    path: PathBuf,
}

impl Mem {
    pub fn new(figure: Figure) -> Self {
        Mem {
            figure,
            amount: Amount::from_unit(1, Some("MiB"), false),
            path: "/proc/meminfo".into(),
        }
    }

    /// Attach the configured display format.
    pub fn with_amount(mut self, amount: Amount) -> Self {
        self.amount = amount;
        self
    }

    /// The display format for the selected figure.
    pub fn amount(&self) -> &Amount {
        &self.amount
    }

    /// Point the sampler at a captured fixture (tests in `fixtures`).
    #[cfg(test)]
    pub(crate) fn with_fixture(mut self, rel: &str) -> Self {
        self.path = crate::fixtures::fixture_path(rel);
        self
    }

    #[cfg(test)]
    fn with_path(mut self, path: PathBuf) -> Self {
        self.path = path;
        self
    }

    /// Read `MemTotal` and `MemAvailable`.
    ///
    /// `MemAvailable` was added in Linux 3.14; on older kernels it is absent
    /// and the caller gets an error rather than a bogus `total - 0`.
    pub fn sample(&self) -> Result<Sample, String> {
        let text = std::fs::read_to_string(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))?;
        let total_kib = parse_field(&text, "MemTotal").ok_or("MemTotal missing")?;
        let available_kib = parse_field(&text, "MemAvailable").ok_or("MemAvailable missing")?;
        Ok(Sample {
            total_kib,
            available_kib,
        })
    }

    /// The configured figure, in KiB, for one sample.
    pub fn value_kib(&self, sample: Sample) -> u64 {
        sample.select(self.figure)
    }
}

pub(crate) fn parse_field(text: &str, name: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|v| v.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const MEMINFO: &str = "MemTotal:       16384000 kB\nMemFree:   2000000 kB\n\
MemAvailable:    8192000 kB\nBuffers:     100000 kB\n";

    #[test]
    fn parses_total_and_available() {
        let dir = std::env::temp_dir().join("status-mem-test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("meminfo");
        fs::write(&path, MEMINFO).unwrap();

        let s = Mem::new(Figure::Used).with_path(path).sample().unwrap();
        assert_eq!(s.total_kib, 16_384_000);
        assert_eq!(s.available_kib, 8_192_000);
        assert_eq!(s.used_kib(), 8_192_000);
    }

    #[test]
    fn figures_map_to_config_values() {
        let s = Sample {
            total_kib: 100,
            available_kib: 40,
        };
        assert_eq!(s.select(Figure::from_config(0)), 60);
        assert_eq!(s.select(Figure::from_config(1)), 40);
        assert_eq!(s.select(Figure::from_config(2)), 100);
        assert_eq!(s.select(Figure::from_config(9)), 60);
    }

    #[test]
    fn used_saturates_instead_of_wrapping() {
        let s = Sample {
            total_kib: 100,
            available_kib: 400,
        };
        assert_eq!(s.used_kib(), 0);
    }

    #[test]
    fn missing_available_field_is_an_error() {
        let dir = std::env::temp_dir().join("status-mem-test-old");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("meminfo");
        fs::write(&path, "MemTotal: 1000 kB\nMemFree: 500 kB\n").unwrap();
        let err = Mem::new(Figure::Used).with_path(path).sample().unwrap_err();
        assert!(err.contains("MemAvailable"), "{err}");
    }
}
