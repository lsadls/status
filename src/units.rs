//! Human-facing quantity formatting.
//!
//! Every displayed number is a KiB value plus three independent decisions:
//!
//! 1. **scale** — divide by 1, 1024, or 1024² (the `unit` key),
//! 2. **suffix** — `K`/`M`/`G` by default, or whatever the user typed in
//!    `suffix`,
//! 3. **auto-scaling** — optionally step up a unit when the number gets large,
//!    so `2048 K/s` prints as `2 M/s`.
//!
//! These live in one type because they must agree: a value divided by 1024 but
//! labelled `K` is a lie, and that exact bug existed before this module
//! (`disk` labelled `unit = 2` as `G` while dividing by 1024). The previous
//! code also kept one such table per module; they now all use [`Amount`].

use crate::config::Config;

/// Size of one KiB, the base all kernel counters are reported in.
pub const KIB: u64 = 1024;

/// Largest meaningful `unit` value (0 = KiB, 1 = MiB, 2 = GiB).
const UNIT_MAX: u8 = 2;

/// A configured quantity format: scale, suffix and auto-scaling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Amount {
    /// Divisor applied to a KiB value: 1, 1024 or 1024².
    divisor: u64,
    /// Label printed after the number, e.g. `"M"` or `"MiB"`. `None` prints
    /// the bare number: an empty `suffix` in the config means "no label", not
    /// "pick a letter for me".
    suffix: Option<String>,
    /// Step the suffix up (`K` → `M` → `G`) once the value would reach 1024.
    auto: bool,
}

impl Amount {
    /// The default unit letter for a configured `unit` value (`0` = K).
    ///
    /// Values above 2 clamp to `G` so a typo cannot silently produce
    /// kilobyte-sized IO numbers; the config layer warns separately.
    pub fn letter(value: u8) -> &'static str {
        match value.min(UNIT_MAX) {
            0 => "K",
            1 => "M",
            _ => "G",
        }
    }

    /// The divisor matching a configured `unit` value.
    pub fn divisor_for(value: u8) -> u64 {
        KIB.pow(u32::from(value.min(UNIT_MAX)))
    }

    /// Build from a `unit` value, using that unit's letter as the label.
    ///
    /// This is the built-in default (a rate reads `K/s`, memory `MiB`): the
    /// letter is chosen for the user because no config expressed an opinion.
    /// A config line is different — see [`Amount::from_unit`], where an empty
    /// `suffix` means "print the number alone".
    pub fn with_letter(value: u8) -> Self {
        Amount {
            divisor: Amount::divisor_for(value),
            suffix: Some(Amount::letter(value).to_string()),
            auto: false,
        }
    }

    /// Build from a `unit` value and the configured suffix.
    ///
    /// `suffix` is `None` when the key is absent or written empty, and the
    /// number is then printed without a label. Filling in the unit letter
    /// there (and a `/s` on top of it) made `suffix =` behave exactly like a
    /// non-empty value, which is why the setting looked ignored.
    ///
    /// A suffix that starts with a unit letter (`MB`, `GiB`) also selects that
    /// unit, because the two cannot disagree: dividing by 1024 while printing
    /// `512MB` would be a lie of exactly the kind this module exists to prevent.
    pub fn from_unit(value: u8, suffix: Option<&str>, auto: bool) -> Self {
        let suffix = suffix.map(str::trim).filter(|s| !s.is_empty());
        let named = suffix.as_ref().and_then(|s| {
            s.as_bytes().first().and_then(|first| match first {
                b'K' | b'k' => Some(0),
                b'M' => Some(1),
                b'G' => Some(2),
                _ => None,
            })
        });
        let (value, suffix) = match named {
            Some(unit) => (unit, suffix.map(str::to_string)),
            None => (value, suffix.map(str::to_string)),
        };
        Amount {
            divisor: Amount::divisor_for(value),
            suffix,
            auto,
        }
    }

    /// Same format with auto-scaling switched on.
    #[cfg(test)]
    pub fn with_auto(mut self, auto: bool) -> Self {
        self.auto = auto;
        self
    }

    /// The suffix printed before any auto-scaling.
    ///
    /// Test-only accessor: production code always goes through
    /// [`Amount::format_int`], which applies scaling.
    #[cfg(test)]
    pub fn suffix(&self) -> Option<&str> {
        self.suffix.as_deref()
    }

    /// The divisor applied before any auto-scaling.
    #[cfg(test)]
    pub fn divisor(&self) -> u64 {
        self.divisor
    }

    /// Whether auto-scaling is on.
    #[cfg(test)]
    pub fn auto(&self) -> bool {
        self.auto
    }

    /// Convert a KiB value into this unit, keeping one decimal place.
    pub fn scale_f64(&self, kib: u64) -> f64 {
        kib as f64 / self.divisor as f64
    }

    /// Render `kib` as an integer followed by the suffix, e.g. `"3M"`.
    ///
    /// With auto-scaling on, the suffix steps up while the integer part would
    /// not fit in three digits (1024 and above), so a busy link reads `2M`
    /// rather than `2048K`. The step is capped at `G`: there is no `T` unit in
    /// the kernel's vocabulary for these counters.
    pub fn format_int(&self, kib: u64) -> String {
        let (number, suffix) = self.split_int(kib);
        format!("{number}{suffix}")
    }

    /// Number and unit separately, so a caller can decorate the unit
    /// (`"{n}{unit}/s"`) without having to guess which unit was chosen.
    pub fn split_int(&self, kib: u64) -> (String, String) {
        // Auto-scaling only rewrites a suffix it understands: `packets` must
        // not silently become `2M` behind the user's back.
        let (divisor, suffix) = if self.auto && self.starts_with_unit_letter() {
            let (divisor, suffix) = self.auto_scale(kib);
            (divisor, Some(suffix))
        } else {
            (self.divisor, self.suffix.clone())
        };
        ((kib / divisor).to_string(), suffix.unwrap_or_default())
    }

    /// Whether the suffix begins with `K`, `M` or `G` (with or without a
    /// trailing `B`), i.e. whether [`Amount::auto_scale`] can rewrite it.
    fn starts_with_unit_letter(&self) -> bool {
        matches!(
            self.suffix.as_deref().and_then(|s| s.as_bytes().first()),
            Some(b'K' | b'M' | b'G')
        )
    }

    /// Render `kib` with one decimal, e.g. `"130.5GiB"`.
    ///
    /// Auto-scaling is ignored here on purpose: a filesystem size does not
    /// change between lines, so a value flipping from `1023.9MiB` to
    /// `1.0GiB` would make the column jitter without conveying anything.
    pub fn format_decimal(&self, kib: u64) -> String {
        format!(
            "{:.1}{}",
            self.scale_f64(kib),
            self.suffix.as_deref().unwrap_or_default()
        )
    }

    /// Pick the divisor and suffix to print `kib` with, stepping *up* by 1024.
    ///
    /// The configured suffix is the floor: `[disk_io] suffix = MB` keeps
    /// printing `512MB` for a small rate instead of silently dropping back to
    /// `K`. Only when the value outgrows its unit does the leading letter
    /// advance, and only a suffix that starts with such a letter is rewritten
    /// (`MB` → `GB`); anything else (`packets`) is replaced by the unit letter,
    /// because scaling it would invent a name the user never asked for.
    fn auto_scale(&self, kib: u64) -> (u64, String) {
        // Start at the configured unit, never below it.
        let mut index = kibi_index(self.divisor);
        if let Some(configured) = self.suffix.as_deref().and_then(|s| s.as_bytes().first()) {
            let from_suffix = match configured {
                b'K' => 0,
                b'M' => 1,
                b'G' => 2,
                _ => index,
            };
            index = index.max(from_suffix);
        }
        while kib / Amount::divisor_for(index) >= KIB && index < UNIT_MAX {
            index += 1;
        }
        let divisor = Amount::divisor_for(index);
        // Keep whatever followed a user-supplied unit letter (`MB` → `B`).
        let rest = match self.suffix.as_deref() {
            Some(s) if matches!(s.as_bytes().first(), Some(b'K' | b'M' | b'G')) => &s[1..],
            _ => "",
        };
        (divisor, format!("{}{rest}", Amount::letter(index)))
    }
}

/// How many times `divisor` can be divided by 1024 before reaching 1, capped
/// at [`UNIT_MAX`].
fn kibi_index(divisor: u64) -> u8 {
    let mut d = divisor;
    let mut index = 0;
    while d > 1 && index < UNIT_MAX {
        d /= KIB;
        index += 1;
    }
    index
}

/// One configured quantity format read from a config section.
///
/// Recognised keys (all optional):
///
/// | key | meaning |
/// |-----|---------|
/// | `unit` | `0` = KiB, `1` = MiB, `2` = GiB |
/// | `suffix` | printed instead of the unit letter |
/// | `auto` | `1` steps the suffix up as the value grows |
///
/// Callers pass the defaults that match their metric: rates default to KiB
/// with auto-scaling off (opt-in), volumes to GiB, memory to MiB.
///
/// `default_suffix` is the label used when the file has no `suffix` line at
/// all, so a capacity reads `GiB` rather than a bare `G`. A `suffix` line that
/// is present but empty is a deliberate "no label", and is passed through as
/// `None`.
pub fn amount_from(
    cfg: &Config,
    section: &str,
    default_unit: u8,
    default_suffix: &str,
    default_auto: bool,
    warnings: &mut Vec<String>,
) -> Amount {
    let unit = cfg.get_u8(section, "unit", default_unit, warnings);
    let suffix = match cfg.written(section, "suffix") {
        None => Some(default_suffix),
        Some("") => None,
        Some(written) => Some(written),
    };
    let auto = cfg.get_flag(section, "auto", default_auto, warnings);
    Amount::from_unit(unit, suffix, auto)
}

/// Convert a byte count from `statvfs` into KiB.
///
/// `statvfs` reports bytes when the block size is multiplied by the block
/// count; keep everything internally in KiB so that [`Amount`] applies
/// uniformly.
/// Format a per-second rate as number and unit, e.g. (`"2"`, `"M/s"`).
///
/// An empty `suffix` in the config means "no label at all", so a bare number
/// stays bare — appending `/s` to it would print a unit the user removed. With
/// a suffix, the trailing `/s` is added unless it already reads as a rate
/// (`MB/s`), which would otherwise come out as `MB/s/s`.
pub fn rate_of(amount: &Amount, kib: u64) -> (String, String) {
    let (number, unit) = amount.split_int(kib);
    if unit.is_empty() || unit.ends_with("/s") {
        return (number, unit);
    }
    (number, format!("{unit}/s"))
}

pub fn bytes_to_kib(bytes: u128) -> u64 {
    // 128-bit intermediate: a huge filesystem can exceed u64 when squared.
    u64::try_from(bytes / KIB as u128).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg(text: &str) -> Config {
        let mut warnings = Vec::new();
        Config::parse(text, &mut warnings)
    }

    #[test]
    fn unit_letters_match_divisors() {
        // The historical bug: `unit = 2` was labelled `G` but divided by 1024.
        for (value, letter, divisor) in [(0, "K", 1u64), (1, "M", 1024), (2, "G", 1024 * 1024)] {
            let a = Amount::with_letter(value);
            assert_eq!(a.suffix(), Some(letter));
            assert_eq!(a.divisor(), divisor);
        }
        // unknown values clamp to the largest unit instead of lying
        assert_eq!(Amount::with_letter(9).suffix(), Some("G"));
        assert_eq!(Amount::from_unit(9, Some("G"), false).divisor(), 1024 * 1024);
    }

    #[test]
    fn scale_uses_the_same_factor_as_the_label() {
        // `format_int` and `format_decimal` must agree on the divisor, which is
        // the pair of channels that previously drifted apart.
        let m = Amount::with_letter(1);
        assert_eq!(m.format_int(3 * KIB), "3M");
        let d = Amount::with_letter(1);
        assert_eq!(d.format_decimal(3 * KIB + 512), "3.5M");
    }

    #[test]
    fn custom_suffix_replaces_the_letter() {
        let a = Amount::from_unit(1, Some("MB"), false);
        assert_eq!(a.format_int(3 * KIB), "3MB");
        // The divisor still comes from `unit`, not from the suffix text.
        assert_eq!(a.divisor(), KIB);
    }

    #[test]
    fn an_empty_suffix_prints_the_number_alone() {
        // `suffix =` in the file means "no label". Falling back to the unit
        // letter made the setting look ignored, and a rate then even grew a
        // `/s` on top of it.
        for blank in [None, Some(""), Some("   ")] {
            let a = Amount::from_unit(2, blank, false);
            assert_eq!(a.suffix(), None, "{blank:?}");
            assert_eq!(a.format_int(2048 * KIB), "2");
        }
    }

    #[test]
    fn a_rate_without_a_suffix_has_no_per_second_marker() {
        let a = Amount::from_unit(0, None, false);
        assert_eq!(rate_of(&a, 275), ("275".to_string(), String::new()));
        // A suffix keeps its `/s`, and one that already reads as a rate does
        // not get a second one.
        let k = Amount::from_unit(0, Some("K"), false);
        assert_eq!(rate_of(&k, 275).1, "K/s");
        let mb = Amount::from_unit(2, Some("MB/s"), false);
        assert_eq!(rate_of(&mb, 2048 * KIB).1, "MB/s");
    }

    #[test]
    fn auto_scaling_steps_up_until_the_number_fits() {
        let a = Amount::with_letter(0).with_auto(true);
        assert_eq!(a.format_int(512), "512K");
        assert_eq!(a.format_int(1024), "1M");
        assert_eq!(a.format_int(2 * KIB * KIB), "2G");
        // Stops at G: no terabyte label exists for these counters.
        assert_eq!(a.format_int(4096 * KIB * KIB), "4096G");
    }

    #[test]
    fn auto_scaling_off_keeps_the_configured_unit() {
        let a = Amount::with_letter(0);
        assert_eq!(a.format_int(2048), "2048K");
        // A volume starting at GiB stays in GiB even for a large filesystem.
        let g = Amount::with_letter(2);
        assert_eq!(g.format_int(2048 * KIB), "2G");
    }

    #[test]
    fn a_suffix_starting_with_a_unit_letter_selects_that_unit() {
        // `suffix = MB` must not print `512MB` for 512 KiB: the letter says
        // which divisor to use, so the pair can never disagree.
        let a = Amount::from_unit(0, Some("MB"), true);
        assert_eq!(a.divisor(), KIB);
        assert_eq!(a.format_int(3 * KIB), "3MB");
        // Growing past 1024 MiB rewrites the letter, keeping the trailing `B`.
        assert_eq!(a.format_int(2048 * KIB), "2GB");
    }

    #[test]
    fn a_suffix_without_a_unit_letter_keeps_the_configured_unit() {
        let a = Amount::from_unit(0, Some("packets"), true);
        assert_eq!(a.divisor(), 1);
        assert_eq!(a.format_int(512), "512packets");
        // A word the module cannot rewrite is left untouched rather than
        // silently swapped for `2M`.
        assert_eq!(a.format_int(2048), "2048packets");
    }

    #[test]
    fn auto_scaling_leaves_an_unrecognised_word_alone() {
        let a = Amount::from_unit(0, Some("bytes"), true);
        assert_eq!(a.format_int(2048), "2048bytes");
    }

    #[test]
    fn decimals_do_not_auto_scale() {
        // A filesystem size must not jitter between MiB and GiB.
        let a = Amount::from_unit(1, Some("MiB"), true);
        assert_eq!(a.format_decimal(2048 * KIB), "2048.0MiB");
    }

    #[test]
    fn amount_from_reads_the_section_keys() {
        let c = cfg("[disk]\nunit = 2\nsuffix = GB\nauto = 1\n");
        let mut w = Vec::new();
        let a = amount_from(&c, "disk", 2, "", false, &mut w);
        assert!(w.is_empty());
        assert_eq!(a.suffix(), Some("GB"));
        assert_eq!(a.divisor(), KIB * KIB);
        assert!(a.auto());
    }

    #[test]
    fn amount_from_uses_the_caller_defaults_when_keys_are_absent() {
        let c = cfg("[net]\n");
        let mut w = Vec::new();
        // No `suffix` line at all: the caller's default label applies.
        let a = amount_from(&c, "net", 0, "K", false, &mut w);
        assert_eq!((a.suffix(), a.divisor(), a.auto()), (Some("K"), 1, false));
    }

    #[test]
    fn amount_from_treats_a_written_empty_suffix_as_no_label() {
        // `suffix =` is a decision, not a missing key: the number prints bare.
        let c = cfg("[net]\nsuffix =\n");
        let mut w = Vec::new();
        let a = amount_from(&c, "net", 0, "K", false, &mut w);
        assert_eq!(a.suffix(), None);
        assert_eq!(a.format_int(275), "275");
    }

    #[test]
    fn volume_conversion() {
        let kib = bytes_to_kib(1024 * 1024 * 1024);
        assert_eq!(kib, 1024 * 1024);
        assert_eq!(Amount::from_unit(2, Some(""), false).scale_f64(kib), 1.0);
    }
}
