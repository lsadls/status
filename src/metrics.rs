//! Sampling orchestration: the one place that decides *when* counters are read.
//!
//! # Sampling contract
//!
//! `/proc` and `/sys` expose monotonic counters, so every rate needs two
//! readings separated by a known interval. `Sampler::sample` therefore:
//!
//! 1. reads every counter (`before`),
//! 2. sleeps [`SAMPLE_INTERVAL`],
//! 3. reads them again and returns the deltas plus all point-in-time metrics
//!    (temperature, memory, clock) taken during step 3.
//!
//! Because the interval is a fixed constant, `delta / interval` is a rate
//! without measuring wall time; that also makes the numbers comparable between
//! runs. The previous implementation re-snapshotted the baseline at the top of
//! every iteration, so the first line after each 10-second refresh was computed
//! from a stale baseline.
//!
//! A metric that cannot be read degrades to `n/a` on that line; nothing here
//! aborts the program.

use crate::{
    bat::Bat,
    config::Config,
    cpu::Cpu,
    date::Date,
    disk::{self, Disk},
    mem::{Figure as MemFigure, Mem},
    net::{self, Net},
    prefix::Prefixes,
    units,
};
use std::{fs, thread::sleep, time::Duration};

/// Interval between the two counter snapshots of one line.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// How often slow facts (filesystem space, interface choice) are re-read.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10);

/// All metrics for one rendered line.
#[derive(Debug, Clone)]
pub struct Reading {
    /// CPU busy percentage; `None` when the counters did not advance.
    pub cpu_busy_percent: Result<Option<f64>, String>,
    /// Average CPU frequency in MHz; `None` when disabled or unreadable.
    pub cpu_freq_mhz: Option<f64>,
    /// Whether the frequency field should be printed at all.
    pub cpu_freq_enabled: bool,
    /// CPU temperature in °C; `Ok(None)` when the toggle is off.
    pub cpu_temp_c: Result<Option<f64>, String>,
    /// Memory figure selected by `[mem] figure`, already formatted.
    pub mem: Result<String, String>,
    /// Battery line; `None` when `[bat] show = no`.
    pub battery: Option<Result<String, String>>,
    /// Network throughput, formatted; `None` when `[net] show = no`.
    pub net: Option<Result<String, String>>,
    /// Filesystem figure, formatted.
    pub disk_volume: Result<String, String>,
    /// Disk throughput, formatted; `None` when `[disk_io] show = no`.
    pub disk_io: Option<Result<String, String>>,
    /// Timestamp.
    pub date: String,
    /// Labels printed before each field; see [`Prefixes`].
    pub prefixes: Prefixes,
}

impl Reading {
    /// Render the single line shown to the user.
    ///
    /// Disabled optional fields (temperature, battery) are omitted entirely;
    /// enabled but unreadable fields render as a short `n/a` token.
    pub fn format(&self) -> String {
        let mut parts: Vec<String> = Vec::with_capacity(9);
        let p = &self.prefixes;

        // CPU usage, frequency and temperature form one group with no ` | `
        // between them: they describe a single device, and the old layout
        // separated `2400MHz` from `45°C` even though both belong to the CPU.
        let busy = match &self.cpu_busy_percent {
            Ok(Some(p)) => format!("{p:.0}%"),
            Ok(None) => "0%".to_string(),
            Err(_) => "n/a".to_string(),
        };
        let mut cpu = format!("{}{busy}", Prefixes::render(&p.cpu));
        if self.cpu_freq_enabled {
            match self.cpu_freq_mhz {
                Some(f) => cpu.push_str(&format!(" {f:.0}MHz")),
                None => cpu.push_str(" n/a MHz"),
            }
        }
        match &self.cpu_temp_c {
            Ok(Some(t)) => cpu.push_str(&format!(" {t:.0}°C")),
            // Disabled temperature contributes nothing at all.
            Ok(None) => {}
            Err(_) => cpu.push_str(" n/a °C"),
        }
        parts.push(cpu);

        parts.push(format!(
            "{}{}",
            Prefixes::render(&p.mem),
            token(&self.mem, "n/a mem")
        ));

        if let Some(bat) = &self.battery {
            let value = match bat {
                Ok(s) => s.clone(),
                Err(_) => "n/a bat".to_string(),
            };
            parts.push(format!("{}{value}", Prefixes::render(&p.bat)));
        }

        if let Some(net) = &self.net {
            parts.push(format!(
                "{}{}",
                Prefixes::render(&p.net),
                token(net, "n/a net")
            ));
        }
        parts.push(format!(
            "{}{}",
            Prefixes::render(&p.disk_size),
            token(&self.disk_volume, "n/a disk")
        ));
        if let Some(io) = &self.disk_io {
            parts.push(format!(
                "{}{}",
                Prefixes::render(&p.disk_io),
                token(io, "n/a io")
            ));
        }
        parts.push(format!(
            "{}{}",
            Prefixes::render(&p.date),
            self.date
        ));

        format!("[{}]", parts.join(" | "))
    }
}

fn token(value: &Result<String, String>, placeholder: &str) -> String {
    match value {
        Ok(s) => s.clone(),
        Err(_) => placeholder.to_string(),
    }
}

/// Owns every sampler plus the counter baselines.
#[derive(Debug)]
pub struct Sampler {
    cpu: Cpu,
    mem: Mem,
    /// `None` when `[net] show = no`: no interface discovery, no reads.
    /// The `Err` arm also carries the configured interface name, so a retry on
    /// a later tick asks for the same device the config named.
    net: Option<Result<Net, String>>,
    /// The two values the line needs straight from the config: the interface
    /// name to retry detection with, and the field labels.
    layout: Layout,
    disk: Disk,
    bat: Option<Bat>,
    date: Date,
}

/// The config values that are not metrics: they shape the line, not a reading.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    /// `[net] interface` as written, needed only to retry detection.
    pub nic: String,
    /// Labels printed before each field.
    pub prefixes: Prefixes,
}

impl Sampler {
    /// Build from already-validated config values.
    pub fn new(
        cpu: Cpu,
        mem: Mem,
        net: Option<Result<Net, String>>,
        disk: Disk,
        bat: Option<Bat>,
        date: Date,
        layout: Layout,
    ) -> Self {
        Sampler {
            cpu,
            mem,
            net,
            disk,
            bat,
            date,
            layout,
        }
    }

    /// The active network sampler, if the metric is enabled and usable.
    fn net_ok(&self) -> Option<&Net> {
        self.net.as_ref().and_then(|n| n.as_ref().ok())
    }

    /// Re-run interface detection (called on the slow refresh tick).
    ///
    /// Only retried while the interface is still missing: a working interface
    /// should not be re-detected underneath the counter baselines, otherwise
    /// the next rate would mix two different devices.
    ///
    /// Returns a note to print when discovery just succeeded, so the user
    /// learns which interface was picked even if it appeared after startup.
    pub fn rediscover(&mut self) -> Option<String> {
        // Nothing to rediscover when the metric is switched off entirely.
        let current = self.net.as_ref()?;
        current.as_ref().err()?;
        match Net::new(&self.layout.nic) {
            Ok(net) => {
                let note = net.note.clone();
                self.net = Some(Ok(net));
                note.map(|n| format!("[net] {n}"))
            }
            Err(_) => None,
        }
    }

    /// Diagnostic lines worth showing once at startup.
    pub fn notes(&self) -> Vec<String> {
        // Only successes are reported here: a failure was already pushed onto
        // the build warnings by `net_from`, and printing it twice made the
        // same line appear twice on stderr.
        self.net_ok()
            .and_then(|net| net.note.as_ref())
            .map(|note| vec![format!("[net] {note}")])
            .unwrap_or_default()
    }

    /// Take one sample, sleeping [`SAMPLE_INTERVAL`] between counter reads.
    pub fn sample(&mut self) -> Reading {
        // Only enabled metrics are read: a disabled section costs no syscall.
        let cpu_before = self.cpu.ticks();
        let net_before = self.net_ok().map(Net::counters);
        let disk_before = self.disk.io_enabled().then(|| self.disk.counters());

        sleep(SAMPLE_INTERVAL);

        let cpu_after = self.cpu.ticks();
        let net_after = self.net_ok().map(Net::counters);
        let disk_after = self.disk.io_enabled().then(|| self.disk.counters());

        let cpu_busy_percent = match (cpu_before, cpu_after) {
            (Ok(b), Ok(a)) => Ok(self.cpu.usage(b, a)),
            (Err(e), _) | (_, Err(e)) => Err(e),
        };

        let net = self.net.as_ref().map(|net| match (net, net_before, net_after) {
            (Err(e), _, _) => Err(e.clone()),
            (Ok(net), Some(Ok(b)), Some(Ok(a))) => {
                Ok(net::Sample::between(b, a).format(net.amount()))
            }
            (Ok(net), _, _) => Err(format!("counter read failed for {}", net.nic())),
        });

        let disk_io = disk_before.zip(disk_after).map(|(b, a)| match (b, a) {
            (Ok(b), Ok(a)) => Ok(disk::Sample::between(b, a).format(self.disk.speed())),
            (Err(e), _) | (_, Err(e)) => Err(e),
        });

        Reading {
            cpu_busy_percent,
            cpu_freq_mhz: self.cpu.freq_mhz(),
            cpu_freq_enabled: self.cpu.freq_enabled(),
            cpu_temp_c: self.cpu.temp_celsius(),
            mem: self
                .mem
                .sample()
                .map(|s| self.mem.amount().format_int(self.mem.value_kib(s))),
            battery: self.bat.as_ref().map(|b| b.sample().map(|s| s.format())),
            net,
            disk_volume: self.disk.space().map(|s| self.disk.format_space(s)),
            disk_io,
            date: self.date.render(),
            prefixes: self.layout.prefixes.clone(),
        }
    }
}

/// Build the CPU sampler.
///
/// Three independent toggles: `temp` drives the temperature sensor, `mhz`
/// drives the `cpu MHz` read from `/proc/cpuinfo`, and `per_core` decides
/// whether the core count is read at all. Turning any of them off removes the
/// read, not just the number.
pub fn cpu_from(cfg: &Config, warnings: &mut Vec<String>) -> Cpu {
    let temp_enabled = cfg.get_flag("cpu", "temp", false, warnings);
    let per_core = cfg.get_flag("cpu", "per_core", false, warnings);
    let mut cpu = Cpu::new(temp_enabled, &cfg.get_string("cpu", "zone", ""))
        .with_per_core(per_core);
    if !cfg.get_flag("cpu", "mhz", true, warnings) {
        cpu = cpu.without_freq();
    }
    if let Some(note) = &cpu.temp_note {
        warnings.push(format!("[cpu] temperature: {note}"));
    }
    cpu
}

/// Build the memory sampler.
///
/// Memory always defaults to MiB, because that is how `free -m` and every other
/// Linux tool reports it; `[mem] unit`/`suffix` still override it.
pub fn mem_from(cfg: &Config, warnings: &mut Vec<String>) -> Mem {
    let amount = units::amount_from(cfg, "mem", 1, "MiB", false, warnings);
    let figure = cfg.get_choice(
        "mem",
        "figure",
        &[("used", 0), ("available", 1), ("total", 2)],
        0,
        warnings,
    );
    Mem::new(MemFigure::from_config(figure))
        .with_amount(amount)
}

/// Build the disk sampler.
///
/// Capacity and throughput have separate formats: people describe a filesystem
/// in GiB but a busy disk in KiB/s, so the two keys default differently.
/// Throughput has its own on/off switch (`[disk_io] show`), stored next to the
/// format it belongs to, rather than a second copy of it under `[disk]`. It
/// removes the counter reads, not just the output.
pub fn disk_from(cfg: &Config, warnings: &mut Vec<String>) -> Disk {
    // `[disk_size]` holds the capacity format so that `[disk] unit` can keep
    // meaning "rate", which is what it meant before the two were split.
    let size = units::amount_from(cfg, "disk_size", 2, "GiB", false, warnings);
    let speed = units::amount_from(cfg, "disk_io", 0, "", false, warnings);
    let disk = Disk::new(
        &disk_device(&cfg.get_string("disk", "device", "")),
        &cfg.get_string("disk", "mount", "/"),
        disk::Figure::from_config(cfg.get_choice(
            "disk",
            "figure",
            &[("available", 0), ("used", 1), ("total", 2)],
            0,
            warnings,
        )),
        size,
        speed,
    );
    let io = cfg.get_flag("disk_io", "show", true, warnings);
    if io {
        disk
    } else {
        disk.without_io()
    }
}

/// Resolve `[disk] device`, falling back to a device that actually exists.
///
/// The built-in default used to be `sda`, which silently reported `n/a` on
/// every machine with NVMe storage. An empty value now auto-detects instead.
fn disk_device(configured: &str) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        return configured.to_string();
    }
    detect_disk().unwrap_or_else(|| "sda".to_string())
}

/// First real block device, sorted for a stable choice across runs.
///
/// `/sys/block` lists whole devices only (partitions live under their parent),
/// so `nvme0n1p1` can never be picked. Entries without a `device` symlink are
/// virtual (`zram0`, `loop0`, `dm-0`) and are skipped, because their counters
/// stay at zero and would look like an idle disk.
fn detect_disk() -> Option<String> {
    let mut names: Vec<String> = fs::read_dir("/sys/block")
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join("device").exists())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.into_iter().next()
}

/// Build the network sampler.
///
/// Returns `(sampler, configured interface)`: the name is handed back so the
/// caller can pass it to [`Sampler::new`] without reading the config a second
/// time (which would also repeat the rename notice).
///
/// The sampler is `None` when `[net] show = no`, in which case no interface is
/// discovered and no counter is ever read. A discovery failure is kept as a
/// value so the rest of the dashboard still runs.
pub fn net_from(cfg: &Config, warnings: &mut Vec<String>) -> (Option<Result<Net, String>>, String) {
    if !cfg.get_flag("net", "show", true, warnings) {
        return (None, String::new());
    }
    let nic = cfg.get_string("net", "interface", "");
    let amount = units::amount_from(cfg, "net", 0, "", false, warnings);
    let sampler = Net::new(&nic).map(|net| net.with_amount(amount)).map_err(|e| {
        warnings.push(format!("[net] {e}"));
        e
    });
    (Some(sampler), nic)
}

/// Build the battery sampler; `None` when the toggle is off or it is unusable.
pub fn bat_from(cfg: &Config, warnings: &mut Vec<String>) -> Option<Bat> {
    if !cfg.get_flag("bat", "show", false, warnings) {
        return None;
    }
    match Bat::new(&cfg.get_string("bat", "path", "")) {
        Ok(b) => Some(b),
        Err(e) => {
            warnings.push(format!("[bat] disabled: {e}"));
            None
        }
    }
}

/// Build the clock formatter.
pub fn date_from(cfg: &Config, warnings: &mut Vec<String>) -> Date {
    let date = Date::new(&cfg.get_string("date", "fmt", ""));
    if let Some(note) = date.note() {
        warnings.push(note.to_string());
    }
    date
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A line where every optional metric is present, used as the baseline
    /// that individual tests then break one field at a time.
    fn complete() -> Reading {
        Reading {
            cpu_busy_percent: Ok(Some(12.4)),
            cpu_freq_mhz: Some(2400.0),
            cpu_freq_enabled: true,
            cpu_temp_c: Ok(Some(45.0)),
            mem: Ok("8192MiB".to_string()),
            battery: Some(Ok("50- 12.0W 2:30".to_string())),
            net: Some(Ok("3K/s↓ 1K/s↑".to_string())),
            disk_volume: Ok("1.0GiB".to_string()),
            disk_io: Some(Ok("2M/s↑ 1M/s↓".to_string())),
            date: "2024-01-02 03:04:05".to_string(),
            prefixes: Prefixes::default(),
        }
    }

    #[test]
    fn fields_are_joined_by_a_separator() {
        assert_eq!(
            complete().format(),
            "[cpu: 12% 2400MHz 45°C | mem: 8192MiB | bat: 50- 12.0W 2:30 \
             | net: 3K/s↓ 1K/s↑ | disk: 1.0GiB | io: 2M/s↑ 1M/s↓ \
             | date: 2024-01-02 03:04:05]"
        );
    }

    #[test]
    fn disk_throughput_has_one_switch() {
        // `[disk]` says what to measure, `[disk_io]` how to print it; only the
        // latter decides whether the counters are read. A switch in two places
        // is what let `[disk_io] enable = 0` be silently ignored.
        for (ini, io) in [
            ("[disk_io]\nshow=no\n", false),
            ("[disk_io]\nshow=yes\n", true),
            ("[disk]\nshow=no\n", true),
            ("", true),
        ] {
            let mut warnings = Vec::new();
            let cfg = crate::config::Config::parse(ini, &mut warnings);
            assert_eq!(
                disk_from(&cfg, &mut warnings).io_enabled(),
                io,
                "config: {ini:?}"
            );
        }
    }

    #[test]
    fn empty_prefixes_restore_the_bare_line() {
        // Turning every label off must give the exact line the program printed
        // before prefixes existed: a decoration, not a re-layout.
        let r = Reading {
            prefixes: Prefixes::from_config(&crate::config::Config::parse(
                "[prefix]\ncpu=\nmem=\nbat=\nnet=\ndisk_size=\ndisk_io=\ndate=\n",
                &mut Vec::new(),
            )),
            ..complete()
        };
        assert_eq!(
            r.format(),
            "[12% 2400MHz 45°C | 8192MiB | 50- 12.0W 2:30 | 3K/s↓ 1K/s↑ \
             | 1.0GiB | 2M/s↑ 1M/s↓ | 2024-01-02 03:04:05]"
        );
    }

    #[test]
    fn frequency_and_temperature_are_not_separated_by_a_bar() {
        // Both numbers describe the same device, so a ` | ` between them reads
        // as if they were unrelated metrics.
        let line = complete().format();
        let head = line.split(" | ").next().unwrap();
        assert_eq!(head, "[cpu: 12% 2400MHz 45°C");
        assert!(!line.contains("2400MHz | "), "{line}");
    }

    #[test]
    fn disabled_frequency_leaves_no_gap() {
        let r = Reading {
            cpu_freq_enabled: false,
            cpu_freq_mhz: None,
            ..complete()
        };
        assert_eq!(
            r.format(),
            "[cpu: 12% 45°C | mem: 8192MiB | bat: 50- 12.0W 2:30 \
             | net: 3K/s↓ 1K/s↑ | disk: 1.0GiB | io: 2M/s↑ 1M/s↓ \
             | date: 2024-01-02 03:04:05]"
        );
    }

    #[test]
    fn disabled_optional_fields_are_omitted_entirely() {
        // `[cpu] temp = no`, `[bat] show = no`, `[net] show = no` and
        // `[disk_io] show = no` must not leave empty slots behind.
        let r = Reading {
            cpu_temp_c: Ok(None),
            battery: None,
            net: None,
            disk_io: None,
            ..complete()
        };
        assert_eq!(
            r.format(),
            "[cpu: 12% 2400MHz | mem: 8192MiB | disk: 1.0GiB \
             | date: 2024-01-02 03:04:05]"
        );
    }

    #[test]
    fn unreadable_metrics_degrade_to_placeholders() {
        // One broken sensor must not cost the rest of the line.
        let r = Reading {
            cpu_busy_percent: Err("cannot read".to_string()),
            cpu_freq_mhz: None,
            cpu_temp_c: Err("cannot read".to_string()),
            mem: Err("cannot read".to_string()),
            battery: Some(Err("cannot read".to_string())),
            net: Some(Err("cannot read".to_string())),
            disk_volume: Err("cannot read".to_string()),
            disk_io: Some(Err("cannot read".to_string())),
            ..complete()
        };
        assert_eq!(
            r.format(),
            "[cpu: n/a n/a MHz n/a °C | mem: n/a mem | bat: n/a bat \
             | net: n/a net | disk: n/a disk | io: n/a io \
             | date: 2024-01-02 03:04:05]"
        );
    }

    #[test]
    fn idle_cpu_without_tick_movement_shows_zero() {
        let r = Reading {
            cpu_busy_percent: Ok(None),
            ..complete()
        };
        assert!(r.format().starts_with("[cpu: 0% 2400MHz 45°C"));
    }
}
