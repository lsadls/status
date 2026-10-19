//! Battery state from `/sys/class/power_supply/<BAT*>`.
//!
//! Sysfs exposes two flavours of battery drivers:
//! * energy based: `energy_now`/`energy_full` (µWh) with `power_now` (µW),
//! * charge based: `charge_now`/`charge_full` (µAh) with `current_now` (µA).
//!
//! Either way the remaining time is `capacity / rate` in hours, which is why the
//! old code's `charge / current` worked and its `current * voltage` (a power
//! calculation divided by 1e12) did not: `voltage_now` is in µV, so µA × µV is
//! 1e-12 W and needs `/1e12` to become watts — but it was printed as `{:.2}`
//! with no unit and was 0 whenever the machine was plugged in and idle.

use crate::sysfs;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Charging state, normalised from the driver's `status` string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Charging,
    Discharging,
    /// `Full`, `Not charging`, or any unknown string: on mains, no time estimate.
    Idle,
    /// `Unknown`/missing.
    Unknown,
}

impl State {
    fn parse(raw: &str) -> Self {
        match raw {
            "Charging" => State::Charging,
            "Discharging" => State::Discharging,
            "Full" | "Not charging" => State::Idle,
            _ => State::Unknown,
        }
    }

    /// One-character indicator used in the prompt.
    pub fn symbol(self) -> char {
        match self {
            State::Charging => '+',
            State::Discharging => '-',
            State::Idle => '#',
            State::Unknown => '?',
        }
    }
}

/// One battery reading.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    /// Charge level, 0..=100.
    pub percent: u8,
    pub state: State,
    /// Remaining (or, while charging, time-to-full) estimate in hours.
    pub hours: Option<f64>,
    /// Current power draw in watts, when the driver reports it.
    pub watts: Option<f64>,
}

impl State {
    /// Whether the battery is moving charge, i.e. whether a rate is meaningful.
    ///
    /// `Full` and `Not charging` mean power is coming from the mains, so any
    /// `current_now` the driver still reports is the machine's own draw, not
    /// the battery's. Printing it as battery power is wrong even though the
    /// number exists.
    pub fn transfers_charge(self) -> bool {
        matches!(self, State::Charging | State::Discharging)
    }
}

impl Sample {
    /// Render the battery field.
    ///
    /// A battery that reads 100 % *and* is not discharging collapses to
    /// `"100"`: on mains at full charge there is no direction to draw and no
    /// estimate to give. Every other case keeps the symbol plus whatever was
    /// measured — including a discharging battery that happens to sit at 100 %,
    /// which is on its way down and still has a rate and a time.
    pub fn format(&self) -> String {
        if self.percent >= 100 && self.state != State::Discharging {
            return "100".to_string();
        }
        let mut out = format!("{}{}", self.percent, self.state.symbol());
        if let Some(w) = self.watts {
            out.push_str(&format!(" {w:.1}W"));
        }
        if let Some(h) = self.hours {
            if h.is_finite() && h >= 0.0 {
                let hours = h.floor();
                let minutes = ((h - hours) * 60.0).round();
                // rounding can push minutes to 60
                let (hours, minutes) = if minutes >= 60.0 {
                    (hours + 1.0, 0.0)
                } else {
                    (hours, minutes)
                };
                out.push_str(&format!(" {}:{:02}", hours as u64, minutes as u64));
            }
        }
        out
    }
}

/// Reads one battery.
#[derive(Debug)]
pub struct Bat {
    path: PathBuf,
    /// Capacity the driver treats as full, in the driver's own unit.
    full: Option<f64>,
}

impl Bat {
    /// Build the sampler, auto-detecting the battery directory when `path` is
    /// empty. Returns `Err` when the toggle is on but no battery exists, so
    /// `main` can say that once instead of dropping the field silently.
    pub fn new(path: &str) -> Result<Self, String> {
        let path = if path.is_empty() {
            detect_battery().ok_or("no battery found under /sys/class/power_supply")?
        } else {
            PathBuf::from(path.trim_end_matches('/'))
        };
        if !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        let full = read_any(&path, &["energy_full", "charge_full"]);
        Ok(Bat { path, full })
    }

    #[cfg(test)]
    fn with_path(path: PathBuf) -> Self {
        let full = read_any(&path, &["energy_full", "charge_full"]);
        Bat { path, full }
    }

    /// Read one sample.
    ///
    /// `capacity` is the only mandatory field (percent); everything else is
    /// optional so a driver that lacks `power_now` still shows a percentage.
    pub fn sample(&self) -> Result<Sample, String> {
        let percent = sysfs::read_u64(self.path.join("capacity"))
            .map_err(|e| format!("{}: {e}", self.path.join("capacity").display()))?
            .min(100) as u8;
        let state = sysfs::read_trimmed(self.path.join("status"))
            .map(|s| State::parse(&s))
            .unwrap_or(State::Unknown);

        // Remaining charge in the driver's unit, and the matching rate.
        let (charge, rate) = if let Some(now) = read_any(&self.path, &["energy_now", "charge_now"])
        {
            let rate = read_any(&self.path, &["power_now", "current_now"]);
            (Some(now), rate)
        } else {
            (None, None)
        };

        let hours = match (state, charge, rate) {
            (State::Discharging, Some(c), Some(r)) if r > 0.0 => Some(c / r),
            (State::Charging, Some(c), Some(r)) if r > 0.0 => {
                // `full` is unknown only when the driver exposes no `*_full`;
                // then scale the remaining charge to 100 %.
                let full =
                    self.full
                        .unwrap_or(c / f64::from(percent.max(1)) * 100.0);
                (full > c).then(|| (full - c) / r)
            }
            _ => None,
        };

        // Power is only a battery quantity while charge is moving. On mains
        // (`Full`, `Not charging`) the driver can keep reporting a current —
        // e.g. 1.02 A at 17.03 V on a full pack — and multiplying it would
        // claim the battery is drawing 17.4 W when it is not charging at all.
        let watts = if state.transfers_charge() {
            read_any(&self.path, &["power_now"])
                .map(|uw| uw / 1e6)
                .or_else(|| {
                    // Fall back to P = U × I with microvolts and microamps.
                    let volt = read_any(&self.path, &["voltage_now"])?;
                    let amp = read_any(&self.path, &["current_now"])?;
                    Some(volt * amp / 1e12)
                })
                .filter(|w| *w > 0.0)
        } else {
            None
        };

        Ok(Sample {
            percent,
            state,
            hours,
            watts,
        })
    }
}

fn read_any(dir: &Path, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|n| sysfs::read_f64(dir.join(n)).ok())
}

/// Pick the first `BAT*` entry of `/sys/class/power_supply`.
fn detect_battery() -> Option<PathBuf> {
    let entries = fs::read_dir("/sys/class/power_supply").ok()?;
    let mut batts: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("BAT"))
        })
        .collect();
    batts.sort();
    batts.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture(name: &str, fields: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("status-bat-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (k, v) in fields {
            fs::write(dir.join(k), format!("{v}\n")).unwrap();
        }
        dir
    }

    #[test]
    fn state_parsing_matches_driver_strings() {
        assert_eq!(State::parse("Charging"), State::Charging);
        assert_eq!(State::parse("Discharging"), State::Discharging);
        assert_eq!(State::parse("Full"), State::Idle);
        assert_eq!(State::parse("Not charging"), State::Idle);
        assert_eq!(State::parse("weird"), State::Unknown);
    }

    #[test]
    fn discharging_reports_remaining_time() {
        let dir = fixture(
            "discharging",
            &[
                ("capacity", "50"),
                ("status", "Discharging"),
                ("charge_now", "2500000"),
                ("current_now", "1000000"),
                ("voltage_now", "12000000"),
            ],
        );
        let b = Bat::with_path(dir);
        let s = b.sample().unwrap();
        assert_eq!(s.percent, 50);
        assert_eq!(s.state, State::Discharging);
        assert!((s.hours.unwrap() - 2.5).abs() < 1e-9);
        // 12 V × 1 A = 12 W
        assert!((s.watts.unwrap() - 12.0).abs() < 1e-6);
        assert_eq!(s.format(), "50- 12.0W 2:30");
    }

    #[test]
    fn not_charging_does_not_show_a_bogus_estimate() {
        // This is the state of a plugged-in laptop; the old code printed "0:0".
        let dir = fixture(
            "notcharging",
            &[
                ("capacity", "99"),
                ("status", "Not charging"),
                ("charge_now", "5148000"),
                ("current_now", "0"),
                ("voltage_now", "16466000"),
            ],
        );
        let s = Bat::with_path(dir).sample().unwrap();
        assert_eq!(s.state, State::Idle);
        assert_eq!(s.hours, None);
        // 0 A × 16.4 V is not a meaningful power reading
        assert_eq!(s.watts, None);
        assert_eq!(s.format(), "99#");
    }

    #[test]
    fn charging_uses_power_now_when_available() {
        let dir = fixture(
            "charging",
            &[
                ("capacity", "40"),
                ("status", "Charging"),
                ("energy_now", "20000000"),
                ("energy_full", "50000000"),
                ("power_now", "15000000"),
            ],
        );
        let s = Bat::with_path(dir).sample().unwrap();
        assert_eq!(s.state, State::Charging);
        assert!((s.hours.unwrap() - 2.0).abs() < 1e-9);
        assert!((s.watts.unwrap() - 15.0).abs() < 1e-9);
    }

    #[test]
    fn full_on_mains_collapses_to_the_number() {
        // The real machine that exposed this: status=Full, capacity=100, and a
        // driver that still reports 1.02 A at 17.03 V. That product is the
        // laptop's draw, not the battery charging, so it must not be printed.
        let dir = fixture(
            "full",
            &[
                ("capacity", "100"),
                ("status", "Full"),
                ("charge_now", "5200000"),
                ("charge_full", "5200000"),
                ("current_now", "1020000"),
                ("voltage_now", "17029000"),
            ],
        );
        let s = Bat::with_path(dir).sample().unwrap();
        assert_eq!(s.state, State::Idle);
        assert_eq!(s.watts, None);
        assert_eq!(s.hours, None);
        assert_eq!(s.format(), "100");
    }

    #[test]
    fn full_but_discharging_still_shows_rate_and_time() {
        // 100 % is not the same as "charging done": a pack that just came off
        // the mains reads 100 while dropping, and that is exactly when the
        // remaining time is worth showing.
        let dir = fixture(
            "full-discharging",
            &[
                ("capacity", "100"),
                ("status", "Discharging"),
                ("energy_now", "30000000"),
                ("energy_full", "50000000"),
                ("power_now", "10000000"),
            ],
        );
        let s = Bat::with_path(dir).sample().unwrap();
        assert!(s.watts.is_some());
        assert_eq!(s.format(), "100- 10.0W 3:00");
    }

    #[test]
    fn idle_at_full_charge_hides_power_even_with_a_zero_free_current() {
        let dir = fixture(
            "notcharging-full",
            &[
                ("capacity", "100"),
                ("status", "Not charging"),
                ("current_now", "500000"),
                ("voltage_now", "16000000"),
            ],
        );
        let s = Bat::with_path(dir).sample().unwrap();
        assert_eq!(s.watts, None);
        assert_eq!(s.format(), "100");
    }

    #[test]
    fn missing_capacity_is_an_error() {
        let dir = fixture("empty", &[("status", "Discharging")]);
        assert!(Bat::with_path(dir).sample().is_err());
    }

    #[test]
    fn power_is_only_read_while_charge_moves() {
        assert!(State::Charging.transfers_charge());
        assert!(State::Discharging.transfers_charge());
        assert!(!State::Idle.transfers_charge());
        assert!(!State::Unknown.transfers_charge());
    }

    #[test]
    fn minute_rounding_does_not_produce_1_60() {
        let s = Sample {
            percent: 10,
            state: State::Discharging,
            hours: Some(1.999),
            watts: None,
        };
        assert_eq!(s.format(), "10- 2:00");
    }
}
