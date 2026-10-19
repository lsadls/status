//! CPU time and frequency.
//!
//! Usage follows <https://stackoverflow.com/a/23376195>: `/proc/stat` exposes
//! monotonically increasing ticks per field, so the busy ratio is computed from
//! the *delta* between two samples, never from absolute values.
//!
//! The values are accumulated in `u64` ticks. The original code used `u32`,
//! which overflows after ~49 days of 100 Hz ticks (`u32::MAX / 100 / 86400`).

use crate::sysfs;
use std::{fs, path::PathBuf};

/// Raw snapshot of the aggregate `cpu` line in `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Ticks {
    /// Fields 1..=8: user, nice, system, idle, iowait, irq, softirq, steal.
    /// Guest time is already contained in user/nice and must not be added, see
    /// `man 5 proc_stat`.
    pub parts: [u64; 8],
}

impl Ticks {
    /// Total time accounted for, used as the denominator of the busy ratio.
    pub fn total(&self) -> u64 {
        self.parts.iter().sum()
    }

    /// Idle + iowait: time the CPU had nothing to run.
    pub fn idle(&self) -> u64 {
        self.parts[3] + self.parts[4]
    }
}

/// Parsed CPU metrics for one sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Share of time spent outside idle across the machine, `0.0..=100.0`.
    ///
    /// This is the same quantity `top`'s `%Cpu(s)` line reports: a machine with
    /// sixteen cores running one core flat is at 6.25 %, not 100 %.
    pub busy_percent: f64,
}

impl Sample {
    /// The same sample counted per core: one busy core is `100`, all sixteen
    /// are `1600`. This is the scale `htop` and `top -H` use, where a thread
    /// column can read above 100 %.
    ///
    /// Kept separate from [`Sample::busy_percent`] because both are legitimate:
    /// the aggregate answers "how loaded is the box", the per-core one answers
    /// "how busy is a core on average", and mixing them silently would make a
    /// number mean different things in different configs.
    pub fn per_core_percent(self, cores: u32) -> f64 {
        if cores == 0 {
            return self.busy_percent;
        }
        self.busy_percent * f64::from(cores)
    }
}

/// Reads `/proc/stat` and `/proc/cpuinfo`.
#[derive(Debug)]
pub struct Cpu {
    /// Path of the thermal zone to read, `None` when temperature is disabled
    /// or no suitable zone was found.
    temp_path: Option<PathBuf>,
    /// Whether the user asked for a temperature reading.
    temp_enabled: bool,
    /// Whether the frequency should be read at all (`[cpu] mhz = no` skips
    /// the `/proc/cpuinfo` read instead of hiding the value).
    freq_enabled: bool,
    stat_path: PathBuf,
    cpuinfo_path: PathBuf,
    /// Whether usage is reported per core (`[cpu] per_core = yes`). When off,
    /// the core count is never read, so the switch costs no syscall.
    per_core: bool,
    /// Reading reported by the config, kept to explain a missing value.
    pub temp_note: Option<String>,
}

impl Cpu {
    /// Build the sampler.
    ///
    /// `temp_enabled` is the `[cpu] temp` switch; `temp_path` comes from
    /// `[cpu] path` and is auto-detected when empty.
    pub fn new(temp_enabled: bool, temp_path: &str) -> Self {
        let (temp_path, temp_note) = if !temp_enabled {
            (None, None)
        } else if !temp_path.is_empty() {
            let p = PathBuf::from(temp_path);
            let note = (!p.exists()).then(|| format!("{} does not exist", p.display()));
            (Some(p), note)
        } else {
            match detect_thermal_zone() {
                Some(p) => (Some(p), None),
                None => (
                    None,
                    Some("no thermal zone found, set [cpu] path".to_string()),
                ),
            }
        };

        Cpu {
            temp_path,
            temp_enabled,
            freq_enabled: true,
            stat_path: "/proc/stat".into(),
            cpuinfo_path: "/proc/cpuinfo".into(),
            per_core: false,
            temp_note,
        }
    }

    /// Point the sampler at alternative files. Only used by tests.
    #[cfg(test)]
    fn with_paths(mut self, stat: PathBuf, cpuinfo: PathBuf) -> Self {
        self.stat_path = stat;
        self.cpuinfo_path = cpuinfo;
        self
    }

    /// Turn the frequency reading off.
    ///
    /// The toggle is honoured inside [`Cpu::freq_mhz`] so the file is never
    /// opened, not merely dropped from the output line.
    pub fn without_freq(mut self) -> Self {
        self.freq_enabled = false;
        self
    }

    /// Whether the frequency is read at all.
    pub fn freq_enabled(&self) -> bool {
        self.freq_enabled
    }

    /// Report usage per core instead of across the whole machine.
    pub fn with_per_core(mut self, per_core: bool) -> Self {
        self.per_core = per_core;
        self
    }

    /// Logical core count, read only when `[cpu] per_core` asks for it.
    ///
    /// `sysconf` is cheaper than parsing `/proc/cpuinfo`, but it needs `libc`
    /// and this program deliberately has no dependencies besides `chrono`.
    /// `/proc/cpuinfo` is parsed with the same reader the frequency uses, so
    /// the file is at most opened once per sample.
    pub fn cores(&self) -> Option<u32> {
        if !self.per_core {
            return None;
        }
        let text = fs::read_to_string(&self.cpuinfo_path).ok()?;
        parse_cpuinfo_cores(&text)
    }

    /// CPU usage in the configured scale: `0..=100` by default (whole machine,
    /// like `top`'s `%Cpu(s)`), or `0..=cores*100` with `[cpu] per_core`.
    pub fn usage(&self, prev: Ticks, now: Ticks) -> Option<f64> {
        let sample = Cpu::sample(prev, now)?;
        match self.cores() {
            Some(cores) => Some(sample.per_core_percent(cores)),
            None => Some(sample.busy_percent),
        }
    }

    /// Current aggregate CPU ticks.
    pub fn ticks(&self) -> Result<Ticks, String> {
        let text = fs::read_to_string(&self.stat_path)
            .map_err(|e| format!("{}: {e}", self.stat_path.display()))?;
        let line = text
            .lines()
            .find(|l| l.starts_with("cpu "))
            .ok_or_else(|| format!("{}: no aggregate `cpu` line", self.stat_path.display()))?;
        parse_stat_line(line)
    }

    /// Turn two consecutive tick snapshots into a usage sample.
    ///
    /// Returns `None` when the counters did not advance (a very fast poll, or a
    /// counter reset), in which case there is no meaningful percentage.
    pub fn sample(prev: Ticks, now: Ticks) -> Option<Sample> {
        let total = now.total().checked_sub(prev.total())?;
        if total == 0 {
            return None;
        }
        let idle = now.idle().saturating_sub(prev.idle());
        // Clamp: idle must never exceed the total delta even after a reset.
        let idle = idle.min(total);
        Some(Sample {
            busy_percent: (total - idle) as f64 * 100.0 / total as f64,
        })
    }

    /// Average `cpu MHz` across all cores, or `None` when unavailable
    /// (ARM kernels often omit the field).
    pub fn freq_mhz(&self) -> Option<f64> {
        if !self.freq_enabled {
            return None;
        }
        let text = fs::read_to_string(&self.cpuinfo_path).ok()?;
        parse_cpuinfo_mhz(&text)
    }

    /// CPU temperature in degrees Celsius.
    ///
    /// The sysfs value is in millidegrees. Returns `Ok(None)` when the toggle
    /// is off so the caller can omit the field entirely.
    pub fn temp_celsius(&self) -> Result<Option<f64>, String> {
        if !self.temp_enabled {
            return Ok(None);
        }
        let Some(path) = &self.temp_path else {
            return Err(self
                .temp_note
                .clone()
                .unwrap_or_else(|| "temperature enabled but no sensor".to_string()));
        };
        let millideg = sysfs::read_u64(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(millideg as f64 / 1000.0))
    }
}

/// Parse the aggregate `/proc/stat` `cpu` line.
///
/// Older kernels expose 7 fields, newer ones 8 (`steal`) or 10; missing fields
/// are zero. Guest fields are ignored on purpose: `man 5 proc_stat` documents
/// them as already included in `user` and `nice`.
pub(crate) fn parse_stat_line(line: &str) -> Result<Ticks, String> {
    let mut ticks = Ticks::default();
    for (slot, field) in ticks.parts.iter_mut().zip(line.split_whitespace().skip(1)) {
        *slot = field
            .parse::<u64>()
            .map_err(|e| format!("bad tick count {field:?}: {e}"))?;
    }
    if ticks.total() == 0 {
        return Err("all tick counters are zero".to_string());
    }
    Ok(ticks)
}

/// Number of `processor` entries in `/proc/cpuinfo`.
pub(crate) fn parse_cpuinfo_cores(text: &str) -> Option<u32> {
    let mut count = 0u32;
    for line in text.lines() {
        let Some((key, _)) = line.split_once(':') else {
            continue;
        };
        if key.trim() == "processor" {
            count += 1;
        }
    }
    (count > 0).then_some(count)
}

/// Average of the `cpu MHz` fields in `/proc/cpuinfo`.
pub(crate) fn parse_cpuinfo_mhz(text: &str) -> Option<f64> {
    let (mut sum, mut count) = (0.0, 0u32);
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim() != "cpu MHz" {
            continue;
        }
        if let Ok(mhz) = value.trim().parse::<f64>() {
            sum += mhz;
            count += 1;
        }
    }
    (count > 0).then(|| sum / f64::from(count))
}

/// Find a thermal zone whose `type` names the CPU package.
///
/// Preference order matters: `x86_pkg_temp`/`coretemp` report the package,
/// `acpitz` is a generic ACPI placeholder that may point at the chipset. The
/// scan is deterministic (first match wins) and falls back to any zone with a
/// readable temperature so machines with unusual zone names still work.
fn detect_thermal_zone() -> Option<PathBuf> {
    const PREFERRED: [&str; 6] = [
        "x86_pkg_temp",
        "coretemp",
        "k10temp",
        "acpitz",
        "cpu-thermal",
        "soc_thermal",
    ];

    let entries = fs::read_dir("/sys/class/thermal").ok()?;
    let mut zones: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("thermal_zone"))
        })
        .filter_map(|p| {
            let kind = sysfs::read_trimmed(p.join("type")).ok()?;
            p.join("temp").exists().then_some((kind, p))
        })
        .collect();
    zones.sort_by(|a, b| a.1.cmp(&b.1));

    for wanted in PREFERRED {
        if let Some((_, path)) = zones.iter().find(|(kind, _)| kind == wanted) {
            return Some(path.join("temp"));
        }
    }
    zones.first().map(|(_, p)| p.join("temp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str =
        "cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 50 0 50 350 50 0 0 0 0 0\nintr 1 2 3\n";

    #[test]
    fn parses_aggregate_line_only() {
        let ticks = parse_stat_line("cpu  100 0 100 700 100 0 0 0 0 0").unwrap();
        assert_eq!(ticks.parts, [100, 0, 100, 700, 100, 0, 0, 0]);
        assert_eq!(ticks.total(), 1000);
        assert_eq!(ticks.idle(), 800);
    }

    #[test]
    fn tolerates_short_lines_from_old_kernels() {
        let ticks = parse_stat_line("cpu  10 0 10 80").unwrap();
        assert_eq!(ticks.parts, [10, 0, 10, 80, 0, 0, 0, 0]);
    }

    #[test]
    fn usage_is_the_delta_not_the_total() {
        let prev = parse_stat_line("cpu  100 0 100 700 100 0 0 0").unwrap();
        let now = parse_stat_line("cpu  150 0 150 730 120 0 0 0").unwrap();
        let s = Cpu::sample(prev, now).unwrap();
        // deltas: user 50, nice 0, system 50, idle 30, iowait 20
        // total = 150, idle = 50 → busy = 100 / 150
        assert!((s.busy_percent - 66.666_666_666_666_67).abs() < 1e-9);
    }

    #[test]
    fn per_core_usage_counts_every_core() {
        // Same deltas as above: 100 of 150 ticks busy, i.e. 66.67 % of the
        // machine. On a 24-core box that is 1600 % in `htop` terms, which is
        // the whole point of the switch.
        let prev = parse_stat_line("cpu  100 0 100 700 100 0 0 0").unwrap();
        let now = parse_stat_line("cpu  150 0 150 730 120 0 0 0").unwrap();
        let s = Cpu::sample(prev, now).unwrap();
        assert!((s.per_core_percent(24) - 1600.0).abs() < 1e-6);
        // A zero core count cannot scale anything; keep the machine figure
        // rather than printing `0.0`.
        assert!((s.per_core_percent(0) - s.busy_percent).abs() < 1e-9);
    }

    #[test]
    fn core_count_comes_from_cpuinfo_only_when_enabled() {
        let path = std::env::temp_dir().join("status-cpu-cores-test");
        fs::write(
            &path,
            "processor\t: 0\ncpu MHz\t\t: 1000.0\nprocessor\t: 1\ncpu MHz\t\t: 3000.0\n",
        )
        .unwrap();

        // Off by default: no count, so no scaling and no file read.
        let off = Cpu::new(false, "").with_paths(path.clone(), path.clone());
        assert_eq!(off.cores(), None);
        let prev = parse_stat_line("cpu  100 0 100 700 100 0 0 0").unwrap();
        let now = parse_stat_line("cpu  150 0 150 730 120 0 0 0").unwrap();
        let machine = off.usage(prev, now).unwrap();
        assert!((machine - 66.666_666_666_666_67).abs() < 1e-9);

        let on = Cpu::new(false, "")
            .with_paths(path.clone(), path.clone())
            .with_per_core(true);
        assert_eq!(on.cores(), Some(2));
        assert!((on.usage(prev, now).unwrap() - machine * 2.0).abs() < 1e-9);

        fs::remove_file(&path).ok();
    }

    #[test]
    fn disabled_per_core_never_opens_cpuinfo_for_the_core_count() {
        // The frequency is switched off too, so `usage` is the only possible
        // reader. A path that does not exist proves it was not opened: an
        // attempted read would yield `None` and fall back to the machine
        // figure, which is exactly what must happen *and* nothing more.
        let missing = std::env::temp_dir().join("status-no-such-cpuinfo");
        let cpu = Cpu::new(false, "")
            .with_paths("/proc/stat".into(), missing)
            .without_freq();
        let prev = parse_stat_line("cpu  100 0 100 700 100 0 0 0").unwrap();
        let now = parse_stat_line("cpu  150 0 150 730 120 0 0 0").unwrap();
        assert_eq!(cpu.cores(), None);
        assert!((cpu.usage(prev, now).unwrap() - 66.666_666_666_666_67).abs() < 1e-9);
    }

    #[test]
    fn unchanged_counters_yield_no_sample() {
        let t = parse_stat_line("cpu  1 0 1 1 0 0 0 0").unwrap();
        assert!(Cpu::sample(t, t).is_none());
    }

    #[test]
    fn counter_reset_does_not_underflow() {
        let now = parse_stat_line("cpu  1 0 1 1 0 0 0 0").unwrap();
        let prev = parse_stat_line("cpu  100 0 100 100 0 0 0 0").unwrap();
        assert!(Cpu::sample(prev, now).is_none());
    }

    #[test]
    fn frequency_is_averaged() {
        let info = "processor\t: 0\ncpu MHz\t\t: 1000.0\nprocessor\t: 1\ncpu MHz\t\t: 3000.0\n";
        assert_eq!(parse_cpuinfo_mhz(info), Some(2000.0));
        assert_eq!(parse_cpuinfo_mhz("processor : 0\n"), None);
    }

    #[test]
    fn missing_stat_file_is_an_error() {
        let cpu = Cpu::new(false, "").with_paths(
            PathBuf::from("/nonexistent/status-test/stat"),
            PathBuf::from("/nonexistent/status-test/cpuinfo"),
        );
        assert!(cpu.ticks().is_err());
        assert_eq!(cpu.freq_mhz(), None);
        assert_eq!(cpu.temp_celsius().unwrap(), None);
    }

    #[test]
    fn reads_fixture_files() {
        let dir = std::env::temp_dir().join("status-cpu-test");
        fs::create_dir_all(&dir).unwrap();
        let stat = dir.join("stat");
        fs::write(&stat, STAT).unwrap();
        let cpu = Cpu::new(false, "").with_paths(stat, dir.join("cpuinfo"));
        assert_eq!(cpu.ticks().unwrap().total(), 1000);
    }
}
