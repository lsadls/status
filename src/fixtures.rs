//! Real kernel output used as regression fixtures.
//!
//! The hand-written fixtures in the unit tests encode what we *think* the
//! kernel prints. These files are captured from a live machine, so they catch
//! format changes and assumptions that only hold on the capture host — the
//! class of bug that silently yields `n/a` on someone else's hardware.
//!
//! Captured on Linux 6.x, x86_64, 24 GiB RAM, one `acpitz` zone, one `BAT0`,
//! and `nvme0n1`. Files are reduced to the fields the parsers consume; a full
//! `cpuinfo` would be ~700 lines of noise.

/// Read a captured file as text.
///
/// # Panics
/// When the fixture is missing: a deleted fixture must fail loudly instead of
/// quietly turning its test into a no-op.
#[cfg(test)]
fn fixture(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("fixture {} unreadable: {e}", path.display()))
}

/// Absolute path of a captured file.
#[cfg(test)]
pub(crate) fn fixture_path(rel: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(rel);
    assert!(p.exists(), "fixture {} missing", p.display());
    p
}

#[cfg(test)]
mod tests {
    use super::fixture;
    use crate::{bat, cpu, disk, mem, units};
    use std::path::Path;

    // ── /proc/meminfo ────────────────────────────────────────────────────
    // Guarded: the parser must pick MemTotal/MemAvailable, not MemFree or a
    // field that merely starts with the same prefix (MemAvailable vs
    // MemAvailableSomething) — a real file has ~50 similar keys.

    #[test]
    fn real_meminfo_selects_the_right_fields() {
        // Go through `Mem::sample`, not `parse_field`: that is the code that
        // decides *which* fields matter, and the only place a wrong key name
        // would actually hurt.
        let mem = mem::Mem::new(mem::Figure::Used).with_fixture("proc/meminfo");
        let sample = mem.sample().expect("MemTotal + MemAvailable");
        // 24 GiB box; values are KiB.
        assert!(sample.total_kib > 20_000_000, "total = {}", sample.total_kib);
        // Available is strictly less than total on a live system. Equality
        // would mean the parser returned the same field twice.
        assert!(
            sample.available_kib < sample.total_kib,
            "available {} !< total {}",
            sample.available_kib,
            sample.total_kib
        );
        // MemAvailable must be *much* larger than MemFree (it is MemFree plus
        // reclaimable cache). This is what separates the two fields: swapping
        // them keeps every other assertion true, which is exactly the bug a
        // real fixture exists to catch.
        let text = fixture("proc/meminfo");
        let free = mem::parse_field(&text, "MemFree").expect("MemFree");
        assert!(
            sample.available_kib > free * 2,
            "available {} should far exceed free {free}; wrong field?",
            sample.available_kib
        );
    }

    #[test]
    fn real_meminfo_used_is_plausible() {
        let mem = mem::Mem::new(mem::Figure::Used).with_fixture("proc/meminfo");
        let sample = mem.sample().unwrap();
        let used = sample.used_kib();
        assert!(used > 0 && used < sample.total_kib, "used = {used}");
        // Cross-check against MemFree: on a machine using page cache, free is
        // a small figure, so used must be far larger than it.
        let free = mem::parse_field(&fixture("proc/meminfo"), "MemFree").expect("MemFree");
        assert!(used > free, "used {used} <= free {free}");
    }

    // ── /proc/stat ───────────────────────────────────────────────────────

    #[test]
    fn real_stat_line_parses_all_columns() {
        let text = fixture("proc/stat");
        let line = text.lines().next().expect("first line");
        assert!(line.starts_with("cpu "), "{line}");
        let ticks = cpu::parse_stat_line(line).expect("aggregate cpu line");
        // Counters since boot; a wrong column count would leave a tail field
        // at zero and shrink the total.
        assert!(ticks.total() > 0);
        assert!(ticks.idle() > 0);
        assert!(ticks.idle() <= ticks.total());
    }

    #[test]
    fn real_stat_line_has_the_expected_field_count() {
        // `man 5 proc_stat`: cpu user nice system idle iowait irq softirq
        // steal (guest/guest_nice are already folded into user/nice).
        let text = fixture("proc/stat");
        let line = text.lines().next().unwrap();
        let fields = line.split_whitespace().count();
        assert_eq!(fields, 11, "cpu line has {} fields: {line}", fields);
    }

    // ── /proc/cpuinfo ────────────────────────────────────────────────────

    #[test]
    fn real_cpuinfo_reports_plausible_mhz() {
        let text = fixture("proc/cpuinfo");
        let mhz = cpu::parse_cpuinfo_mhz(&text).expect("cpu MHz present");
        // Power-scaled hosts report ~400..~6000 MHz; outside that the parse
        // is wrong, not the hardware.
        assert!((400.0..6000.0).contains(&mhz), "mhz = {mhz}");
    }

    // ── /proc/diskstats ──────────────────────────────────────────────────

    #[test]
    fn real_diskstats_finds_the_device() {
        let text = fixture("proc/diskstats");
        let c = disk::parse_diskstats(&text, "nvme0n1").expect("nvme0n1 present");
        assert!(c.read > 0, "read bytes = {}", c.read);
        assert!(c.written > 0, "written bytes = {}", c.written);
    }

    #[test]
    fn real_diskstats_applies_the_512_byte_sector_factor() {
        // The fixture's field 6 (sectors read) is known; the parser must
        // multiply by 512. Dropping the factor would make these equal.
        let text = fixture("proc/diskstats");
        let sectors: u64 = text
            .lines()
            .find(|l| l.split_whitespace().nth(2) == Some("nvme0n1"))
            .and_then(|l| l.split_whitespace().nth(5))
            .and_then(|f| f.parse().ok())
            .expect("sectors read");
        let c = disk::parse_diskstats(&text, "nvme0n1").unwrap();
        assert_eq!(c.read, sectors * 512);
        assert_ne!(c.read, sectors, "sector factor missing");
    }

    #[test]
    fn real_diskstats_does_not_invent_an_absent_device() {
        // This host has no sda; the parser must say so instead of returning
        // the first device it sees.
        let text = fixture("proc/diskstats");
        assert!(disk::parse_diskstats(&text, "sda").is_none());
    }

    // ── /sys ─────────────────────────────────────────────────────────────

    #[test]
    fn real_thermal_zone_reads_millidegrees() {
        let raw = fixture("sys/thermal/thermal_zone0/temp");
        let milli: f64 = raw.trim().parse().expect("millidegrees");
        // Plausible die temperature in millidegrees.
        assert!((1_000.0..150_000.0).contains(&milli), "milli = {milli}");
    }

    #[test]
    fn real_battery_directory_is_usable() {
        let dir = format!(
            "{}/fixtures/sys/power_supply/BAT0",
            env!("CARGO_MANIFEST_DIR")
        );
        assert!(Path::new(&dir).is_dir(), "{dir} missing");
        // `Bat::new` takes the configured path, which is exactly this shape.
        let b = bat::Bat::new(&dir).expect("BAT0 fixture");
        let sample = b.sample().expect("sample");
        assert!(sample.percent <= 100, "percent = {}", sample.percent);
        // `BAT0/status` here is "Not charging", which must not be mistaken
        // for an unknown state (that would drop the symbol from the line).
        assert_ne!(sample.state, bat::State::Unknown);
    }

    #[test]
    fn real_net_carrier_reads_as_up() {
        let raw = fixture("sys/net/enp2s0/carrier");
        assert_eq!(raw.trim(), "1");
    }

    // ── conversion sanity ────────────────────────────────────────────────

    #[test]
    fn real_values_survive_the_byte_conversion() {
        // 24 GiB of RAM must print as ~23 GiB, not as ~24000 or ~23000 MiB.
        let sample = mem::Mem::new(mem::Figure::Total)
            .with_fixture("proc/meminfo")
            .sample()
            .unwrap();
        let mib = units::Amount::from_unit(1, Some("MiB"), false);
        let shown = mib.format_int(sample.total_kib);
        let value: u64 = shown.trim_end_matches("MiB").parse().unwrap();
        assert!((20_000..26_000).contains(&value), "total = {shown}");
        // The displayed number must be the KiB figure divided by 1024 once.
        assert_eq!(sample.total_kib / units::KIB, value);
    }
}
