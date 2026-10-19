//! Labels printed in front of each field.
//!
//! The line is a sequence of numbers whose meaning is only obvious once you
//! have read the docs. A prefix (`cpu: 11% 1530MHz 49°C`) names the metric in
//! the line itself, which matters when the reader is a status bar with no room
//! for a legend.
//!
//! Defaults are the built-in labels (`cpu:` and so on). A key written empty
//! removes that label — the same rule `suffix` follows — and a key absent
//! keeps the default, so a config that says nothing about prefixes still shows
//! them.

use crate::config::Config;

/// Labels for the fields that carry one.
///
/// `[disk]` has no prefix: it does not print anything itself, it feeds
/// `[disk_size]` and `[disk_io]`, which do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefixes {
    pub cpu: String,
    pub mem: String,
    pub bat: String,
    pub net: String,
    pub disk_size: String,
    pub disk_io: String,
    pub date: String,
}

impl Default for Prefixes {
    fn default() -> Self {
        Prefixes {
            cpu: "cpu:".to_string(),
            mem: "mem:".to_string(),
            bat: "bat:".to_string(),
            net: "net:".to_string(),
            disk_size: "disk:".to_string(),
            disk_io: "io:".to_string(),
            date: "date:".to_string(),
        }
    }
}

impl Prefixes {
    /// Read the `[prefix]` section, keeping [`Prefixes::default`] for keys the
    /// file does not mention.
    ///
    /// The key names mirror the sections they label, except `disk_size` and
    /// `disk_io`, which are spelled out because `disk:` alone would be
    /// ambiguous across the two capacity/throughput fields.
    pub fn from_config(cfg: &Config) -> Self {
        let d = Prefixes::default();
        let pick = |key: &str, default: &str| -> String {
            match cfg.written("prefix", key) {
                // Key absent: the built-in label still applies.
                None => default.to_string(),
                Some(written) => written.to_string(),
            }
        };
        Prefixes {
            cpu: pick("cpu", &d.cpu),
            mem: pick("mem", &d.mem),
            bat: pick("bat", &d.bat),
            net: pick("net", &d.net),
            disk_size: pick("disk_size", &d.disk_size),
            disk_io: pick("disk_io", &d.disk_io),
            date: pick("date", &d.date),
        }
    }

    /// The label plus the separating space, or nothing when empty.
    ///
    /// A prefix never contains its own trailing space, so `cpu:` cannot end up
    /// as `cpu:  11%` by being configured twice with one.
    pub fn render(label: &str) -> String {
        let label = label.trim_end();
        if label.is_empty() {
            String::new()
        } else {
            format!("{label} ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefixes(text: &str) -> Prefixes {
        let mut warnings = Vec::new();
        Prefixes::from_config(&Config::parse(text, &mut warnings))
    }

    #[test]
    fn built_in_labels_are_used_when_the_section_is_missing() {
        let p = prefixes("");
        assert_eq!(p.cpu, "cpu:");
        assert_eq!(p.disk_size, "disk:");
        assert_eq!(p.disk_io, "io:");
    }

    #[test]
    fn only_the_keys_written_are_overridden() {
        let p = prefixes("[prefix]\ncpu = CPU\n");
        assert_eq!(p.cpu, "CPU");
        assert_eq!(p.mem, "mem:");
    }

    #[test]
    fn an_empty_key_removes_that_label() {
        // `mem =` is a decision to print the number alone, not a typo to be
        // replaced by the default.
        let p = prefixes("[prefix]\nmem =\n");
        assert_eq!(p.mem, "");
        assert_eq!(Prefixes::render(&p.mem), "");
        assert_eq!(Prefixes::render(&p.cpu), "cpu: ");
    }

    #[test]
    fn trailing_space_is_not_doubled() {
        let p = prefixes("[prefix]\ncpu = cpu:   \n");
        assert_eq!(Prefixes::render(&p.cpu), "cpu: ");
    }
}
