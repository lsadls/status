//! Network interface selection and throughput from `/sys/class/net`.
//!
//! Counters (`statistics/rx_bytes`, `tx_bytes`) are monotonic byte totals, so a
//! rate needs two samples. Sampling is done by `metrics`, this module only
//! knows how to read one snapshot and how to pick an interface.

use crate::{
    sysfs,
    units::{rate_of, Amount},
};
use std::{
    fs,
    io,
    path::{Path, PathBuf},
};

/// One counter snapshot, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counters {
    pub rx: u64,
    pub tx: u64,
}

/// Throughput derived from two snapshots, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub rx_kib: u64,
    pub tx_kib: u64,
}

impl Sample {
    /// Turn two counter snapshots into a throughput sample.
    pub fn between(prev: Counters, now: Counters) -> Self {
        Sample {
            rx_kib: sysfs::counter_delta(prev.rx, now.rx) / 1024,
            tx_kib: sysfs::counter_delta(prev.tx, now.tx) / 1024,
        }
    }

    /// Render as `"1K/s↓ 2K/s↑"`.
    ///
    /// The unit is completed with `/s` unless the user already wrote a rate
    /// (`suffix=MB/s`), which would otherwise come out as `MB/s/s`.
    pub fn format(&self, amount: &Amount) -> String {
        let (rx, down) = rate_of(amount, self.rx_kib);
        let (tx, up) = rate_of(amount, self.tx_kib);
        format!("{rx}{down}↓ {tx}{up}↑")
    }
}

/// Reads counters for one interface.
#[derive(Debug)]
pub struct Net {
    /// Interface this sampler reads, as resolved (`enp2s0`), or empty when the
    /// configured name was empty and detection should be retried later.
    pub nic: String,
    root: PathBuf,
    amount: Amount,
    /// Why the interface was chosen automatically; shown once at startup.
    pub note: Option<String>,
}

impl Net {
    /// Build the sampler for the configured `[net] interface`.
    ///
    /// An empty `nic` triggers auto-detection: the first interface that is up
    /// (`carrier == 1`) and has a physical-looking name. Unlike the previous
    /// version this never slices the interface name by fixed offsets
    /// (`&path[15..17]`), so short or long names cannot panic.
    pub fn new(nic: &str) -> Result<Self, String> {
        if !nic.is_empty() {
            let root = PathBuf::from("/sys/class/net").join(nic);
            if !root.exists() {
                return Err(format!("interface {nic} not found under /sys/class/net"));
            }
            return Ok(Net {
                nic: nic.to_string(),
                root,
                amount: Amount::with_letter(0),
                note: None,
            });
        }

        let (nic, note) = detect_interface("/sys/class/net")?;
        Ok(Net {
            nic: nic.clone(),
            root: PathBuf::from("/sys/class/net").join(nic),
            amount: Amount::with_letter(0),
            note: Some(note),
        })
    }

    /// Attach the configured display format.
    pub fn with_amount(mut self, amount: Amount) -> Self {
        self.amount = amount;
        self
    }

    pub fn amount(&self) -> &Amount {
        &self.amount
    }

    /// Point the sampler at a fixture directory. Only used by tests.
    #[cfg(test)]
    fn with_root(mut self, root: PathBuf) -> Self {
        self.root = root.join(&self.nic);
        self
    }

    pub fn nic(&self) -> &str {
        &self.nic
    }

    /// Read one counter snapshot.
    pub fn counters(&self) -> io::Result<Counters> {
        Ok(Counters {
            rx: sysfs::read_u64(self.root.join("statistics/rx_bytes"))?,
            tx: sysfs::read_u64(self.root.join("statistics/tx_bytes"))?,
        })
    }
}

/// Pick an interface: first one that is up with a physical-looking name,
/// otherwise fall back to `lo` (which always exists) and say so.
fn detect_interface(root: &str) -> Result<(String, String), String> {
    let entries = fs::read_dir(root).map_err(|e| format!("{root}: {e}"))?;
    let mut candidates: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    // read_dir order is filesystem order; sort for a stable choice across runs.
    candidates.sort();

    let mut loose: Option<String> = None;
    for nic in &candidates {
        if nic == "lo" {
            continue;
        }
        let path = Path::new(root).join(nic);
        // `carrier` cannot be read while the link is down; that is a valid
        // "down" answer, not an error to report.
        if !sysfs::reads_as(path.join("carrier"), "1") {
            continue;
        }
        if looks_physical(nic) {
            return Ok((nic.clone(), format!("auto-detected {nic} (carrier up)")));
        }
        loose.get_or_insert(nic.clone());
    }

    if let Some(nic) = loose {
        return Ok((nic.clone(), format!("auto-detected {nic} (fallback)")));
    }
    if candidates.iter().any(|n| n == "lo") {
        return Ok((
            "lo".to_string(),
            "no physical interface is up, using lo".to_string(),
        ));
    }
    Err(format!("no network interface found under {root}"))
}

/// Heuristic on the interface *name*: kernel default names start with `en`
/// (ethernet), `wl` (wireless), `ww` (wwan) or `eth`/`wlan` (older naming).
/// Virtual devices (`virbr`, `docker`, `veth`, `br-`, `tun`) are excluded.
fn looks_physical(nic: &str) -> bool {
    const PREFIXES: [&str; 6] = ["en", "wl", "ww", "eth", "wlan", "usb"];
    const VIRTUAL: [&str; 8] = [
        "virbr", "veth", "docker", "br-", "tun", "tap", "vnet", "vmnet",
    ];
    !VIRTUAL.iter().any(|v| nic.starts_with(v))
        && PREFIXES.iter().any(|p| nic.starts_with(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(name: &str, nics: &[(&str, Option<&str>, u64, u64)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("status-net-{name}"));
        let _ = fs::remove_dir_all(&dir);
        for (nic, carrier, rx, tx) in nics {
            let base = dir.join(nic);
            fs::create_dir_all(base.join("statistics")).unwrap();
            if let Some(c) = carrier {
                fs::write(base.join("carrier"), c).unwrap();
            }
            fs::write(base.join("statistics/rx_bytes"), format!("{rx}\n")).unwrap();
            fs::write(base.join("statistics/tx_bytes"), format!("{tx}\n")).unwrap();
        }
        dir
    }

    #[test]
    fn reads_both_directions_from_distinct_files() {
        // The old implementation read rx_bytes twice, so tx was always 0.
        let root = fixture("both", &[("wlan0", Some("1\n"), 2048, 5120)]);
        let net = Net::new("wlan0")
            .unwrap()
            .with_root(root);
        let c = net.counters().unwrap();
        assert_eq!(c, Counters { rx: 2048, tx: 5120 });
    }

    #[test]
    fn detects_up_interface_and_skips_loopback() {
        let root = fixture(
            "detect",
            &[("lo", Some("1\n"), 1, 1), ("enp2s0", Some("1\n"), 1, 1)],
        );
        let (nic, note) = detect_interface(root.to_str().unwrap()).unwrap();
        assert_eq!(nic, "enp2s0");
        assert!(note.contains("carrier up"), "{note}");
    }

    #[test]
    fn skips_interfaces_whose_carrier_cannot_be_read() {
        // wlan0 answers EINVAL while down; that must not abort detection.
        let root = fixture(
            "down",
            &[("wlan0", None, 1, 1), ("enp2s0", Some("1\n"), 1, 1)],
        );
        let (nic, _) = detect_interface(root.to_str().unwrap()).unwrap();
        assert_eq!(nic, "enp2s0");
    }

    #[test]
    fn falls_back_to_loopback_when_nothing_is_up() {
        let root = fixture("onlylo", &[("lo", Some("1\n"), 1, 1)]);
        let (nic, note) = detect_interface(root.to_str().unwrap()).unwrap();
        assert_eq!(nic, "lo");
        assert!(note.contains("lo"), "{note}");
    }

    #[test]
    fn short_names_do_not_panic() {
        // The old `&name[15..17]` slice panicked on short names like "en0".
        assert!(looks_physical("en0"));
        assert!(!looks_physical("lo"));
        assert!(!looks_physical("virbr0"));
    }

    #[test]
    fn rate_uses_configured_unit_once() {
        let s = Sample::between(
            Counters { rx: 0, tx: 0 },
            Counters {
                rx: 3 * 1024 * 1024,
                tx: 2 * 1024,
            },
        );
        assert_eq!(s.rx_kib, 3072);
        assert_eq!(s.format(&Amount::with_letter(1)), "3M/s↓ 0M/s↑");
        assert_eq!(
            s.format(&Amount::with_letter(0)),
            "3072K/s↓ 2K/s↑"
        );
    }

    #[test]
    fn default_rate_is_kib_and_only_auto_scales_when_enabled() {
        let s = Sample::between(
            Counters { rx: 0, tx: 0 },
            Counters {
                rx: 2 * 1024 * 1024,
                tx: 512,
            },
        );
        // Default: no auto-scaling, so a 2 MiB/s burst still reads in KiB.
        let plain = Amount::with_letter(0);
        assert_eq!(s.format(&plain), "2048K/s↓ 0K/s↑");
        // Opt-in: the same sample steps up to MiB.
        let auto = Amount::with_letter(0).with_auto(true);
        assert_eq!(s.format(&auto), "2M/s↓ 0K/s↑");
    }

    #[test]
    fn a_suffix_that_already_reads_as_a_rate_is_not_given_a_second_per_second() {
        // `suffix=MB/s` used to render `0MB/s/s↓` because the format string
        // appended `/s` on top of the user's own unit.
        let sample = Sample {
            rx_kib: 2048,
            tx_kib: 0,
        };
        let a = Amount::from_unit(2, Some("MB/s"), false);
        assert_eq!(sample.format(&a), "2MB/s↓ 0MB/s↑");
    }

    #[test]
    fn custom_rate_suffix_is_used_verbatim() {
        let s = Sample::between(Counters { rx: 0, tx: 0 }, Counters { rx: 2048, tx: 0 });
        assert_eq!(
            s.format(&Amount::from_unit(0, Some("KB"), false)),
            "2KB/s↓ 0KB/s↑"
        );
    }

    #[test]
    fn counter_reset_does_not_produce_a_huge_rate() {
        let s = Sample::between(
            Counters {
                rx: 9_000_000,
                tx: 9_000_000,
            },
            Counters { rx: 1024, tx: 0 },
        );
        // After a reset the delta is unknown: no fabricated spike.
        assert_eq!(s.rx_kib, 0);
        assert_eq!(s.tx_kib, 0);
    }
}
