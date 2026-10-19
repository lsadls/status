//! Timestamp rendering.

use chrono::{format::StrftimeItems, Local};

/// Fallback when `[date] fmt` is empty or unusable.
pub const DEFAULT_FORMAT: &str = "%Y-%m-%d %H:%M:%S";

/// Clock formatter.
#[derive(Debug)]
pub struct Date {
    fmt: String,
    /// Set when the configured format was rejected and the default is used.
    note: Option<String>,
}

impl Date {
    /// Build from the `[date] fmt` value; empty means [`DEFAULT_FORMAT`].
    ///
    /// The format string comes from a file the user edits, so it is validated
    /// here, once, instead of at render time: chrono panics from `Display`
    /// when it meets an unknown specifier (`%Q`) or a trailing `%`, and a typo
    /// in the config must not take down the dashboard.
    pub fn new(fmt: &str) -> Self {
        let note = if fmt.is_empty() {
            None
        } else if is_valid(fmt) {
            return Date {
                fmt: fmt.to_string(),
                note: None,
            };
        } else {
            Some(format!(
                "[date] fmt = {fmt:?} is not a valid strftime pattern, using {DEFAULT_FORMAT}"
            ))
        };
        Date {
            fmt: DEFAULT_FORMAT.to_string(),
            note,
        }
    }

    /// Why the requested format was rejected, if it was.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Current local time.
    ///
    /// The format was validated in [`Date::new`], so this cannot panic.
    pub fn render(&self) -> String {
        Local::now().format(&self.fmt).to_string()
    }
}

/// Whether chrono can render `fmt` without panicking.
///
/// `StrftimeItems` yields an `Item::Error` for an unknown specifier or a
/// dangling `%`, which is exactly the set of inputs that make `Display` panic.
fn is_valid(fmt: &str) -> bool {
    !StrftimeItems::new(fmt).any(|item| item == chrono::format::Item::Error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_format_falls_back_to_default() {
        assert_eq!(Date::new("").fmt, DEFAULT_FORMAT);
        assert_eq!(Date::new("%H").fmt, "%H");
        assert!(Date::new("").note().is_none());
    }

    #[test]
    fn renders_requested_fields() {
        let s = Date::new("%Y").render();
        assert_eq!(s.len(), 4, "{s}");
        assert!(s.chars().all(|c| c.is_ascii_digit()), "{s}");
    }

    #[test]
    fn invalid_format_falls_back_instead_of_panicking() {
        // Both inputs made chrono panic from `Display` before validation.
        for bad in ["%Q", "%Y-%"] {
            let d = Date::new(bad);
            assert_eq!(d.fmt, DEFAULT_FORMAT, "{bad} should be rejected");
            assert!(d.note().is_some(), "{bad} should be reported");
            assert_eq!(d.render().len(), DEFAULT_FORMAT.len() + 2, "{bad}");
        }
    }

    #[test]
    fn valid_but_unusual_format_is_kept() {
        // A literal `%` is expressible as `%%`; only a dangling one is invalid.
        let d = Date::new("100%% %H");
        assert_eq!(d.fmt, "100%% %H");
        assert!(d.note().is_none());
        assert!(d.render().starts_with("100% "), "{}", d.render());
    }
}
