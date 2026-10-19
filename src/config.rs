//! Configuration loading for `status.ini`.
//!
//! The format is a small INI subset: `[section]` headers (case-insensitive),
//! `key = value` pairs, `#`/`;` comments, and missing values (`key =` or a bare
//! `key`). Unknown sections and keys are ignored, so a config file written for
//! an older version keeps working; a missing or malformed key falls back to its
//! default instead of aborting the program.
//!
//! Contract: [`Config::load`] never panics. File-level failures (not found,
//! unreadable) are reported to the caller, per-key problems are reported as
//! warnings on stderr and replaced by defaults.

use std::{collections::HashMap, env, fs, path::PathBuf};

/// File name looked up when no explicit path is given.
pub const FILE_NAME: &str = "status.ini";

/// Parsed config: `section -> key -> value`, both keys lowercased.
#[derive(Debug, Default, Clone)]
pub struct Config {
    sections: HashMap<String, HashMap<String, String>>,
}

/// Where the config was read from, and anything the user should know about.
#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

impl Config {
    /// Parse INI text. Never fails: malformed lines are skipped and reported
    /// through `warnings` so the caller decides how loud to be.
    pub fn parse(text: &str, warnings: &mut Vec<String>) -> Self {
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut current = String::new();

        for (idx, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            if let Some(rest) = line.strip_prefix('[') {
                match rest.strip_suffix(']') {
                    Some(name) => current = name.trim().to_lowercase(),
                    None => warnings.push(format!(
                        "line {}: unterminated section header, line ignored",
                        idx + 1
                    )),
                }
                continue;
            }

            let (key, value) = match line.split_once('=') {
                Some((key, value)) => (key.trim(), value.trim()),
                // A bare `key` means "present, empty value" (comments live on
                // their own lines, so a bare word is a key, not prose).
                None => (line, ""),
            };
            if key.is_empty() {
                warnings.push(format!("line {}: empty key, line ignored", idx + 1));
                continue;
            }
            sections
                .entry(current.clone())
                .or_default()
                .insert(key.to_lowercase(), value.to_string());
        }

        Config { sections }
    }

    /// Read the config from `path`, or from [`default_path`] when `None`.
    ///
    /// Returns an error only when the file cannot be read at all; the caller
    /// then continues with defaults.
    pub fn load(path: Option<&str>) -> Result<Loaded, String> {
        let path = match path {
            Some(p) => PathBuf::from(p),
            None => match default_path() {
                Some(p) => p,
                None => return Err("cannot determine a config directory".to_string()),
            },
        };
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut warnings = Vec::new();
        let config = Config::parse(&text, &mut warnings);
        Ok(Loaded {
            config,
            path,
            warnings,
        })
    }

    /// Raw value of `section.key`, if present and non-empty.
    ///
    /// Lookups are case-insensitive in both directions: names are stored
    /// lowercased, so the incoming names must be lowercased too (a config file
    /// written as `[DISK] DEVICE = sda` must behave like `[disk] device = sda`).
    fn get(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .get(&section.to_lowercase())?
            .get(&key.to_lowercase())
            .map(|v| v.as_str())
            .filter(|v| !v.is_empty())
    }

    /// Numeric value of `section.key`, parsed as `u8`.
    ///
    /// Out-of-range or unparsable values are reported in `warnings` and the
    /// default is used, because these keys select a unit/mode.
    pub fn get_u8(&self, section: &str, key: &str, default: u8, warnings: &mut Vec<String>) -> u8 {
        let Some(raw) = self.get(section, key) else {
            return default;
        };
        match raw.parse::<u8>() {
            Ok(v) => v,
            Err(_) => {
                warnings.push(format!(
                    "[{section}] {key} = {raw:?} is not a number 0-255, using {default}"
                ));
                default
            }
        }
    }

    /// String value of `section.key`, or `default` when absent/empty.
    pub fn get_string(&self, section: &str, key: &str, default: &str) -> String {
        self.get(section, key).unwrap_or(default).to_string()
    }

    /// The text written for `section.key`, including an explicit empty value.
    ///
    /// `None` means the key is not in the file at all. `Some("")` means the
    /// user wrote `key =` on purpose, which some keys treat as "print nothing"
    /// rather than "use the default".
    pub fn written(&self, section: &str, key: &str) -> Option<&str> {
        self.sections
            .get(&section.to_lowercase())?
            .get(&key.to_lowercase())
            .map(|v| v.as_str())
    }

    /// A named choice whose spellings are listed in `choices`, e.g.
    /// `[("used", 0), ("available", 1), ("total", 2)]`.
    ///
    /// Only the names are accepted: a number here would force the reader to
    /// remember an ordering that the config file never states. Anything else
    /// warns and uses `default`.
    pub fn get_choice(
        &self,
        section: &str,
        key: &str,
        choices: &[(&str, u8)],
        default: u8,
        warnings: &mut Vec<String>,
    ) -> u8 {
        let Some(raw) = self.get(section, key) else {
            return default;
        };
        let lower = raw.to_ascii_lowercase();
        if let Some((_, value)) = choices.iter().find(|(name, _)| *name == lower) {
            return *value;
        }
        let names: Vec<&str> = choices.iter().map(|(name, _)| *name).collect();
        warnings.push(format!(
            "[{section}] {key} = {raw:?} is not one of {}; using {default}",
            names.join("/")
        ));
        default
    }

    /// Boolean value: `1`/`yes`/`true`/`on` = enabled, `0`/`no`/`false`/`off` =
    /// disabled, anything else = default plus a warning.
    ///
    /// Accepting words matters for keys whose name already says what they do
    /// (`freq = no`): a switch that only understands `0`/`1` forces the reader
    /// to remember which of the two means "off".
    pub fn get_flag(&self, section: &str, key: &str, default: bool, warnings: &mut Vec<String>) -> bool {
        match self.get(section, key) {
            None => default,
            Some(raw) => match parse_flag(raw) {
                Some(on) => on,
                None => {
                    warnings.push(format!(
                        "[{section}] {key} = {raw:?} is not yes/no, using {}",
                        if default { "yes" } else { "no" }
                    ));
                    default
                }
            },
        }
    }
}

/// Accepted spellings for a on/off value.
fn parse_flag(raw: &str) -> Option<bool> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "yes" | "true" | "on" => Some(true),
        "0" | "no" | "false" | "off" => Some(false),
        _ => None,
    }
}

/// The config file: `$XDG_CONFIG_HOME/status.ini`, or `~/.config/status.ini`.
///
/// `$STATUS_CONFIG` overrides both; `--config <PATH>` overrides everything.
/// The file sits directly in the config directory rather than in a `status/`
/// subdirectory, so a single-file program has a single-file path.
pub fn default_path() -> Option<PathBuf> {
    if let Ok(p) = env::var("STATUS_CONFIG") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }

    let dir = match env::var("XDG_CONFIG_HOME") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from(env::var("HOME").ok().filter(|h| !h.is_empty())?)
            .join(".config"),
    };
    Some(dir.join(FILE_NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> (Config, Vec<String>) {
        let mut warnings = Vec::new();
        (Config::parse(text, &mut warnings), warnings)
    }

    #[test]
    fn parses_sections_keys_and_comments() {
        let (cfg, warnings) = parsed(
            "# comment\n[DISK]\nblk = sda\n; another comment\npath=/home\n\n[Date]\nfmt=%H:%M\n",
        );
        assert!(warnings.is_empty());
        assert_eq!(cfg.get("disk", "blk"), Some("sda"));
        assert_eq!(cfg.get_string("date", "fmt", "x"), "%H:%M");
        // section names are case-insensitive
        assert_eq!(cfg.get_string("DISK", "path", "x"), "/home");
    }

    #[test]
    fn empty_value_falls_back_to_default() {
        let (cfg, _) = parsed("[net]\nnic=\nunit=\n");
        assert_eq!(cfg.get_string("net", "nic", "auto"), "auto");
        let mut w = Vec::new();
        assert_eq!(cfg.get_u8("net", "unit", 0, &mut w), 0);
        assert!(w.is_empty(), "empty value is absence, not an error");
    }

    #[test]
    fn numeric_value_is_used_verbatim() {
        let (cfg, _) = parsed("[mem]\nkey=7\n");
        let mut w = Vec::new();
        assert_eq!(cfg.get_u8("mem", "key", 0, &mut w), 7);
        assert!(w.is_empty());
    }

    #[test]
    fn non_numeric_value_warns() {
        let (cfg, _) = parsed("[mem]\nkey=abc\n");
        let mut w = Vec::new();
        assert_eq!(cfg.get_u8("mem", "key", 2, &mut w), 2);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("key"));
    }

    #[test]
    fn unterminated_section_header_warns() {
        let (_, warnings) = parsed("[cpu\nenable=1\n");
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn switches_accept_words_as_well_as_numbers() {
        // `freq = no` reads like the sentence it is; requiring `0` would make
        // the reader remember which of the two numbers means "off".
        let mut w = Vec::new();
        for (text, want) in [
            ("[x]\nk=yes\n", true),
            ("[x]\nk=ON\n", true),
            ("[x]\nk=1\n", true),
            ("[x]\nk=no\n", false),
            ("[x]\nk=Off\n", false),
            ("[x]\nk=0\n", false),
        ] {
            let cfg = Config::parse(text, &mut w);
            assert_eq!(cfg.get_flag("x", "k", !want, &mut w), want, "{text:?}");
        }
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn a_word_that_is_not_a_switch_warns_and_keeps_the_default() {
        let mut w = Vec::new();
        let cfg = Config::parse("[x]\nk=maybe\n", &mut w);
        assert!(cfg.get_flag("x", "k", true, &mut w));
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("k = \"maybe\""), "{w:?}");
    }

    #[test]
    fn choices_accept_the_name_but_not_a_stray_number() {
        let choices = &[("used", 0), ("available", 1), ("total", 2)];
        let mut w = Vec::new();
        for (text, want) in [
            ("[x]\nk=total\n", 2),
            ("[x]\nk=AVAILABLE\n", 1),
            ("[x]\nk=\n", 9),
        ] {
            let cfg = Config::parse(text, &mut w);
            assert_eq!(cfg.get_choice("x", "k", choices, 9, &mut w), want, "{text:?}");
        }
        // A number is not a choice: the file never states the ordering, so
        // `figure = 1` would depend on reading the source to know its meaning.
        let cfg = Config::parse("[x]\nk=7\n", &mut w);
        assert_eq!(cfg.get_choice("x", "k", choices, 9, &mut w), 9);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("used/available/total"), "{w:?}");
        let cfg = Config::parse("[x]\nk=1\n", &mut w);
        assert_eq!(cfg.get_choice("x", "k", choices, 9, &mut w), 9);
    }

    #[test]
    fn values_are_independent_of_section_order() {
        let (cfg, _) = parsed("[date]\nfmt=%Y\n[disk]\nunit=2\n");
        assert_eq!(cfg.get_string("date", "fmt", "x"), "%Y");
        assert_eq!(cfg.get("disk", "unit"), Some("2"));
    }
}
