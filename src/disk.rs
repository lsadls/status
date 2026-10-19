//! Disk throughput and filesystem space.
//!
//! Two independent sources:
//! * `/proc/diskstats` (fields, see `man 5 proc_diskstats`): sectors read /
//!   written per whole block device. Sectors 1-8 are *not* used; the byte
//!   counters are sectors × 512.
//! * `statvfs(3)` on the configured mountpoint: filesystem capacity.

use crate::{
    sysfs,
    units::{bytes_to_kib, rate_of, Amount},
};
use std::{fs, path::PathBuf};

/// Bytes per sector as reported by `/proc/diskstats`.
///
/// The kernel always uses 512-byte units here regardless of the device's
/// physical block size (`man 5 proc_diskstats`). The previous code reported
/// raw sectors, i.e. numbers 512× too large.
const SECTOR_BYTES: u64 = 512;

/// One I/O counter snapshot, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub read: u64,
    pub written: u64,
}

/// Throughput derived from two snapshots, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub read_kib: u64,
    pub written_kib: u64,
}

impl Sample {
    pub fn between(prev: Counters, now: Counters) -> Self {
        Sample {
            read_kib: sysfs::counter_delta(prev.read, now.read) / 1024,
            written_kib: sysfs::counter_delta(prev.written, now.written) / 1024,
        }
    }

    /// Render as `"1K/s↑ 2K/s↓"` (write arrow first, as before).
    ///
    /// The unit is completed with `/s` unless the user already wrote a rate
    /// (`suffix=MB/s`), which would otherwise come out as `MB/s/s`.
    pub fn format(&self, amount: &Amount) -> String {
        let (written, up) = rate_of(amount, self.written_kib);
        let (read, down) = rate_of(amount, self.read_kib);
        format!("{written}{up}↑ {read}{down}↓")
    }
}

/// Which filesystem figure the `[disk] figure` option selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Figure {
    /// Space still writable by unprivileged users (`f_bavail`).
    Available,
    /// Space occupied (`f_blocks - f_bfree`).
    Used,
    /// Filesystem size (`f_blocks`).
    Total,
}

impl Figure {
    /// `0 = available, 1 = used, 2 = total` (see `status.ini`). Unknown values
    /// fall back to available, matching the documented default.
    pub fn from_config(value: u8) -> Self {
        match value {
            1 => Figure::Used,
            2 => Figure::Total,
            _ => Figure::Available,
        }
    }
}

/// One filesystem snapshot, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub total_kib: u64,
    pub avail_kib: u64,
    pub used_kib: u64,
}

impl Space {
    pub fn select(&self, figure: Figure) -> u64 {
        match figure {
            Figure::Available => self.avail_kib,
            Figure::Used => self.used_kib,
            Figure::Total => self.total_kib,
        }
    }

    /// Render with one decimal, e.g. `"12.3GiB"`.
    pub fn format(&self, figure: Figure, amount: &Amount) -> String {
        amount.format_decimal(self.select(figure))
    }
}

/// Reads `/proc/diskstats` and filesystem statistics.
#[derive(Debug)]
pub struct Disk {
    blk: String,
    mount: PathBuf,
    figure: Figure,
    /// Display format for capacity (`[disk] size_unit` / `size_suffix`).
    size: Amount,
    /// Display format for throughput (`[disk] unit` / `suffix` / `auto`).
    speed: Amount,
    /// Throughput is only read when `[disk_io] show` is on; the counters are skipped
    /// entirely otherwise, so a disabled metric costs no `openat`.
    io_enabled: bool,
    diskstats: PathBuf,
}

impl Disk {
    pub fn new(blk: &str, mount: &str, figure: Figure, size: Amount, speed: Amount) -> Self {
        Disk {
            blk: blk.to_string(),
            mount: mount.to_string().into(),
            figure,
            size,
            speed,
            io_enabled: true,
            diskstats: "/proc/diskstats".into(),
        }
    }

    #[cfg(test)]
    fn with_diskstats(mut self, path: PathBuf) -> Self {
        self.diskstats = path;
        self
    }

    /// The throughput display format.
    pub fn speed(&self) -> &Amount {
        &self.speed
    }

    /// Enable or disable I/O throughput in one step.
    ///
    /// Returns `false` when throughput is off, which lets the sampler skip the
    /// counter reads instead of computing a rate nobody will see.
    pub fn io_enabled(&self) -> bool {
        self.io_enabled
    }

    /// Turn throughput off; capacity is still readable.
    pub fn without_io(mut self) -> Self {
        self.io_enabled = false;
        self
    }

    /// Read one I/O counter snapshot for the configured block device.
    ///
    /// Returns an error (instead of a silent zero) when the device is absent:
    /// `[disk] device = sda` on an NVMe machine used to report a steady `0K/s`.
    pub fn counters(&self) -> Result<Counters, String> {
        let text = fs::read_to_string(&self.diskstats)
            .map_err(|e| format!("{}: {e}", self.diskstats.display()))?;
        let cs = parse_diskstats(&text, &self.blk)
            .ok_or_else(|| format!("{}: device not found in {}", self.blk, self.diskstats.display()))?;
        Ok(cs)
    }

    /// Filesystem space of the configured mountpoint.
    pub fn space(&self) -> Result<Space, String> {
        let stat = nix::sys::statvfs::statvfs(self.mount.as_path())
            .map_err(|e| format!("{}: {e}", self.mount.display()))?;
        Ok(space_from(&stat))
    }

    /// Render a space sample with the configured figure and capacity format.
    pub fn format_space(&self, space: Space) -> String {
        space.format(self.figure, &self.size)
    }
}

/// Extract read/written bytes for `blk` from `/proc/diskstats` text.
///
/// Columns (1-based): 1 major, 2 minor, 3 name, 4 reads completed,
/// 5 reads merged, 6 sectors read, 7 ms reading, 8 writes completed,
/// 9 writes merged, 10 sectors written, ...
pub(crate) fn parse_diskstats(text: &str, blk: &str) -> Option<Counters> {
    let line = text
        .lines()
        .find(|l| l.split_whitespace().nth(2) == Some(blk))?;
    let field = |idx: usize| l_col(line, idx);
    let sectors_read = field(6)?;
    let sectors_written = field(10)?;
    Some(Counters {
        read: sectors_read.saturating_mul(SECTOR_BYTES),
        written: sectors_written.saturating_mul(SECTOR_BYTES),
    })
}

fn l_col(line: &str, idx: usize) -> Option<u64> {
    line.split_whitespace().nth(idx - 1)?.parse::<u64>().ok()
}

/// Convert a `statvfs` result into KiB figures.
///
/// `f_bavail` (available to an unprivileged process) is used for "available",
/// since `f_bfree` includes blocks reserved for root — reporting `f_bfree`
/// would advertise space an ordinary user cannot write to. "Used" is derived
/// from `f_bfree` so that `used + avail` never double counts reserved blocks.
fn space_from(stat: &nix::sys::statvfs::Statvfs) -> Space {
    let frsize = u128::from(stat.fragment_size());
    Space {
        total_kib: bytes_to_kib(stat.blocks() as u128 * frsize),
        avail_kib: bytes_to_kib(stat.blocks_available() as u128 * frsize),
        used_kib: bytes_to_kib(
            (stat.blocks().saturating_sub(stat.blocks_free())) as u128 * frsize,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // nvme0n1: reads=2267659 merged=151061 sectors=155359205, writes fields follow
    const DISKSTATS: &str = " 259       2 nvme0n1 2267659 151061 155359205 1051959 1957326 2063222 121988566 2816395 0 1434103 4462092\n\
   8       0 sda 100 0 2048 20 200 0 4096 40 0 60 60\n";

    #[test]
    fn reads_sector_columns_as_bytes() {
        let c = parse_diskstats(DISKSTATS, "sda").unwrap();
        // column 6 = sectors read, column 10 = sectors written
        assert_eq!(
            c,
            Counters {
                read: 2048 * 512,
                written: 4096 * 512
            }
        );
    }

    #[test]
    fn reads_nvme_with_leading_whitespace() {
        let c = parse_diskstats(DISKSTATS, "nvme0n1").unwrap();
        assert_eq!(c.read, 155_359_205 * 512);
        assert_eq!(c.written, 121_988_566 * 512);
    }

    #[test]
    fn unknown_device_is_reported_not_silently_zero() {
        assert!(parse_diskstats(DISKSTATS, "sdb").is_none());
        let disk = Disk::new(
            "sdb",
            "/",
            Figure::Available,
            Amount::with_letter(2),
            Amount::with_letter(0),
        )
        .with_diskstats(std::env::temp_dir().join("status-disk-test-diskstats"));
        assert!(disk.counters().is_err());
    }

    #[test]
    fn figures_map_to_config_values() {
        let space = Space {
            total_kib: 100,
            avail_kib: 30,
            used_kib: 70,
        };
        assert_eq!(space.select(Figure::from_config(0)), 30);
        assert_eq!(space.select(Figure::from_config(1)), 70);
        assert_eq!(space.select(Figure::from_config(2)), 100);
        assert_eq!(space.select(Figure::from_config(9)), 30);
    }

    #[test]
    fn volume_defaults_to_gib_rather_than_mib() {
        // A 130 GiB filesystem must read `130.5GiB`, not `133632.0MiB`:
        // megabytes are not how people describe disk size.
        let space = Space {
            total_kib: 136_845_312, // 130.5 GiB
            avail_kib: 1024 * 1024,
            used_kib: 1024 * 1024,
        };
        let default = Amount::from_unit(2, Some("GiB"), false);
        assert_eq!(space.format(Figure::Total, &default), "130.5GiB");
    }

    #[test]
    fn volume_honours_a_custom_capacity_suffix() {
        let space = Space {
            total_kib: 1024 * 1024, // 1 GiB
            avail_kib: 0,
            used_kib: 1024 * 1024,
        };
        let a = Amount::from_unit(2, Some("GB"), false);
        assert_eq!(space.format(Figure::Total, &a), "1.0GB");
    }

    #[test]
    fn io_format_labels_match_divisors() {
        let s = Sample::between(
            Counters::default(),
            Counters {
                read: 1024 * 1024,
                written: 2 * 1024 * 1024,
            },
        );
        assert_eq!(s.format(&Amount::with_letter(1)), "2M/s↑ 1M/s↓");
        assert_eq!(s.format(&Amount::with_letter(2)), "0G/s↑ 0G/s↓");
        assert_eq!(
            s.format(&Amount::with_letter(0)),
            "2048K/s↑ 1024K/s↓"
        );
        // The default KiB rate with auto-scaling off keeps small values honest.
        assert_eq!(s.format(&Amount::with_letter(0).with_auto(true)), "2M/s↑ 1M/s↓");
    }

    #[test]
    fn disabled_io_skips_the_diskstats_read_entirely() {
        // Point the device at a *nonexistent* block device: if the counters
        // were still read, `counters()` would error. Turning I/O off must make
        // the whole read disappear, not merely hide the result.
        let disk = Disk::new(
            "does-not-exist",
            "/",
            Figure::Total,
            Amount::from_unit(2, Some("GiB"), false),
            Amount::with_letter(0),
        )
        .without_io();
        assert!(!disk.io_enabled());
        // Capacity still works while throughput is off.
        assert!(disk.space().is_ok());
    }

    #[test]
    fn real_root_space_is_sane() {
        let disk = Disk::new(
            "nvme0n1",
            "/",
            Figure::Total,
            Amount::from_unit(2, Some("GiB"), false),
            Amount::with_letter(0),
        );
        let space = disk.space().unwrap();
        assert!(space.total_kib > 0, "{space:?}");
        assert!(space.avail_kib <= space.total_kib, "{space:?}");
        assert!(space.used_kib <= space.total_kib, "{space:?}");
    }
}
