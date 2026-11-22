//! Licence file loader. File only, strict, and never prints the values.
//!
//! The file is a strict subset of TOML: `key = "string"` or `key = 123`, `#` comments, blank
//! lines. Tables, arrays, duplicate keys, unknown keys and any other syntax are errors. Error
//! values never contain the ID or the name.

use crate::region::{mask_is_sane, IdRule, Region};
use crate::REDACTED;
use std::fmt;
use std::path::{Path, PathBuf};

/// Most the file may hold.
const MAX_BYTES: u64 = 4096;

/// Why a licence file or value was rejected. No variant carries the licence ID or name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Io(String),
    TooLarge,
    /// The file is readable by group or others (Unix). `chmod 600` it.
    TooPermissive,
    Syntax {
        line: usize,
    },
    DuplicateKey {
        line: usize,
    },
    UnknownKey {
        line: usize,
    },
    BadRegion,
    BadIdShape,
    BadName,
    BadMask,
    BadInterval,
    /// `region = "other"` needs `id_mask` and `id_interval_secs`.
    OtherNeedsRule,
    /// A value still holds the placeholder from licence.example.toml.
    Placeholder,
    /// No licence file was found at any of the search locations.
    NotFound,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "cannot read the licence file: {e}"),
            Self::TooLarge => write!(f, "licence file is larger than {MAX_BYTES} bytes"),
            Self::TooPermissive => write!(f, "licence file is readable by others; run chmod 600 on it"),
            Self::Syntax { line } => write!(f, "licence file line {line}: not `key = \"value\"`"),
            Self::DuplicateKey { line } => write!(f, "licence file line {line}: duplicate key"),
            Self::UnknownKey { line } => write!(f, "licence file line {line}: unknown key"),
            Self::BadRegion => write!(f, "region must be \"us\" or \"other\""),
            Self::BadIdShape => write!(f, "licence ID does not look like a valid ID for the region (the tools check the shape only and cannot verify it against any database)"),
            Self::BadName => write!(f, "operator name is empty, too long or has control characters"),
            Self::BadMask => write!(f, "id_mask must be 3 to 16 characters of L, D, A-Z, 0-9 or -"),
            Self::BadInterval => write!(f, "id_interval_secs must be 1 to 86400"),
            Self::OtherNeedsRule => write!(f, "region \"other\" has no built-in rules: set id_mask and id_interval_secs yourself, otherwise licensed mode stays blocked"),
            Self::Placeholder => write!(f, "licence file still holds the placeholder values from licence.example.toml"),
            Self::NotFound => write!(f, "no licence file found (--licence-file, MOQ_SUBGHZ_LICENCE, or $XDG_CONFIG_HOME/moq-subghz/licence.toml)"),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Parsed licence file. `Debug` never prints the ID or the name.
#[derive(Clone, PartialEq, Eq)]
pub struct LicenceConfig {
    region: Region,
    id: Option<String>,
    name: Option<String>,
    interval: Option<u32>,
}

impl fmt::Debug for LicenceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set = |o: &Option<String>| if o.is_some() { REDACTED } else { "<unset>" };
        let region = match self.region {
            Region::Us => "us",
            Region::Other(_) => "other",
        };
        f.debug_struct("LicenceConfig")
            .field("region", &region)
            .field("licence_id", &set(&self.id))
            .field("operator_name", &set(&self.name))
            .field("id_interval_secs", &self.interval)
            .finish()
    }
}

impl LicenceConfig {
    /// Build from parts (tests, or a tool that reads the values elsewhere). Applies the same
    /// checks as the file loader. Empty strings count as unset.
    pub fn from_parts(
        region: Region,
        id: Option<&str>,
        name: Option<&str>,
        interval: Option<u32>,
    ) -> Result<Self, ConfigError> {
        let id = id.map(str::trim).filter(|s| !s.is_empty());
        let id = match id {
            Some(raw) => Some(region.check_id(raw)?),
            None => None,
        };
        let name = name.map(str::trim).filter(|s| !s.is_empty());
        let name = match name {
            Some(n) => {
                if is_placeholder(n) {
                    return Err(ConfigError::Placeholder);
                }
                if n.chars().count() > 80 || n.chars().any(char::is_control) {
                    return Err(ConfigError::BadName);
                }
                Some(n.to_string())
            }
            None => None,
        };
        if let Some(s) = interval {
            if !(1..=86_400).contains(&s) {
                return Err(ConfigError::BadInterval);
            }
        }
        Ok(Self {
            region,
            id,
            name,
            interval,
        })
    }

    /// Parse the text of a licence file.
    pub fn parse_str(text: &str) -> Result<Self, ConfigError> {
        let mut seen: Vec<&'static str> = Vec::new();
        let (mut region, mut id, mut name, mut mask, mut interval) = (None, None, None, None, None);
        for (n, raw) in text.lines().enumerate() {
            let line = n + 1;
            let l = strip_comment(raw).trim();
            if l.is_empty() {
                continue;
            }
            let (key, val) = l.split_once('=').ok_or(ConfigError::Syntax { line })?;
            let key = match key.trim() {
                "region" => "region",
                "licence_id" => "licence_id",
                "operator_name" => "operator_name",
                "id_mask" => "id_mask",
                "id_interval_secs" => "id_interval_secs",
                _ => return Err(ConfigError::UnknownKey { line }),
            };
            if seen.contains(&key) {
                return Err(ConfigError::DuplicateKey { line });
            }
            seen.push(key);
            let val = val.trim();
            if key == "id_interval_secs" {
                let v: u32 = val.parse().map_err(|_| ConfigError::BadInterval)?;
                interval = Some(v);
                continue;
            }
            let s = parse_string(val).ok_or(ConfigError::Syntax { line })?;
            match key {
                "region" => region = Some(s),
                "licence_id" => id = Some(s),
                "operator_name" => name = Some(s),
                _ => mask = Some(s),
            }
        }
        let region = match region.as_deref() {
            Some("us") => Region::Us,
            Some("other") => {
                let rule = match mask.as_deref() {
                    Some(m) if mask_is_sane(m) => IdRule::Mask(m.to_string()),
                    Some(_) => return Err(ConfigError::BadMask),
                    None => IdRule::Missing,
                };
                Region::Other(rule)
            }
            _ => return Err(ConfigError::BadRegion),
        };
        if mask.is_some() && matches!(region, Region::Us) {
            return Err(ConfigError::UnknownKey { line: 0 });
        }
        if matches!(region, Region::Other(_)) && interval.is_none() {
            return Err(ConfigError::OtherNeedsRule);
        }
        if id.as_deref().is_some_and(|s| is_placeholder(s.trim())) {
            return Err(ConfigError::Placeholder);
        }
        Self::from_parts(region, id.as_deref(), name.as_deref(), interval)
    }

    /// Read and parse a file. Refuses files over 4 KiB and, on Unix, files that group or others
    /// can read.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let meta = std::fs::metadata(path).map_err(|e| ConfigError::Io(e.kind().to_string()))?;
        if meta.len() > MAX_BYTES {
            return Err(ConfigError::TooLarge);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(ConfigError::TooPermissive);
            }
        }
        let text =
            std::fs::read_to_string(path).map_err(|e| ConfigError::Io(e.kind().to_string()))?;
        Self::parse_str(&text)
    }

    pub fn region(&self) -> &Region {
        &self.region
    }

    /// Seconds between identifications for this region, if known.
    pub fn id_interval_secs(&self) -> Option<u32> {
        self.region.id_interval_secs(self.interval)
    }

    pub fn has_id(&self) -> bool {
        self.id.is_some()
    }

    pub fn has_name(&self) -> bool {
        self.name.is_some()
    }

    /// The ID, or [`REDACTED`] when `redact` is set. `None` when unset.
    pub fn id(&self, redact: bool) -> Option<&str> {
        self.id
            .as_deref()
            .map(|s| if redact { REDACTED } else { s })
    }

    /// The name, or [`REDACTED`] when `redact` is set. `None` when unset.
    pub fn name(&self, redact: bool) -> Option<&str> {
        self.name
            .as_deref()
            .map(|s| if redact { REDACTED } else { s })
    }

    /// One line for run logs and reports: `licence_id=... operator=...`. With `redact` (the
    /// `--redact-licence` option) both values become `[redacted]`.
    pub fn log_line(&self, redact: bool) -> String {
        format!(
            "licence_id={} operator={}",
            self.id(redact).unwrap_or("<unset>"),
            self.name(redact).unwrap_or("<unset>")
        )
    }

    /// Replace the ID and name (any case for the ID) in `text` with [`REDACTED`]. For
    /// scrubbing logs and reports before publishing.
    pub fn redact_text(&self, text: &str) -> String {
        let mut out = text.to_string();
        if let Some(n) = &self.name {
            out = out.replace(n.as_str(), REDACTED);
        }
        if let Some(id) = &self.id {
            out = replace_ci(&out, id, REDACTED);
        }
        out
    }
}

fn replace_ci(text: &str, needle: &str, with: &str) -> String {
    let (t, n) = (text.to_ascii_uppercase(), needle.to_ascii_uppercase());
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    // to_ascii_uppercase keeps byte offsets, so indices map back onto `text`.
    while let Some(p) = t[i..].find(&n) {
        out.push_str(&text[i..i + p]);
        out.push_str(with);
        i += p + n.len();
    }
    out.push_str(&text[i..]);
    out
}

fn is_placeholder(s: &str) -> bool {
    let u = s.to_ascii_uppercase();
    u.starts_with("REPLACE") || u.contains("PLACEHOLDER") || u == "N0CALL" || u.contains("YOUR")
}

fn strip_comment(l: &str) -> &str {
    let mut in_str = false;
    let mut esc = false;
    for (i, c) in l.char_indices() {
        match c {
            '\\' if in_str => esc = !esc,
            '"' if !esc => in_str = !in_str,
            '#' if !in_str => return &l[..i],
            _ => esc = false,
        }
    }
    l
}

fn parse_string(v: &str) -> Option<String> {
    let inner = v.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        match c {
            '"' => return None,
            '\\' => match it.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                _ => return None,
            },
            c => out.push(c),
        }
    }
    Some(out)
}

/// Where to look for the licence file, in order: `--licence-file`, `MOQ_SUBGHZ_LICENCE`,
/// `$XDG_CONFIG_HOME/moq-subghz/licence.toml`, `$HOME/.config/moq-subghz/licence.toml`.
/// Pure function so it can be tested; callers pass in the environment values.
pub fn resolve_path(
    cli: Option<&Path>,
    env_var: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Option<PathBuf> {
    fn nonempty(s: Option<&str>) -> Option<&str> {
        s.filter(|s| !s.is_empty())
    }
    if let Some(p) = cli {
        return Some(p.to_path_buf());
    }
    if let Some(p) = nonempty(env_var) {
        return Some(PathBuf::from(p));
    }
    if let Some(x) = nonempty(xdg_config_home) {
        return Some(Path::new(x).join("moq-subghz/licence.toml"));
    }
    nonempty(home).map(|h| Path::new(h).join(".config/moq-subghz/licence.toml"))
}

/// Resolve the path from the real environment and load it.
pub fn load_from_env(cli: Option<&Path>) -> Result<LicenceConfig, ConfigError> {
    let get = |k: &str| std::env::var(k).ok();
    let path = resolve_path(
        cli,
        get("MOQ_SUBGHZ_LICENCE").as_deref(),
        get("XDG_CONFIG_HOME").as_deref(),
        get("HOME").as_deref(),
    )
    .ok_or(ConfigError::NotFound)?;
    if !path.exists() {
        return Err(ConfigError::NotFound);
    }
    LicenceConfig::load(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str =
        "region = \"us\"\nlicence_id = \"K1AB\"  # c\noperator_name = \"Test Person\"\n";

    #[test]
    fn parses_good_file() {
        let c = LicenceConfig::parse_str(GOOD).unwrap();
        assert_eq!(c.id(false), Some("K1AB"));
        assert_eq!(c.name(false), Some("Test Person"));
        assert_eq!(c.id_interval_secs(), Some(600));
    }

    #[test]
    fn strict_rejections() {
        let e = |t: &str| LicenceConfig::parse_str(t).unwrap_err();
        assert_eq!(
            e("region = \"us\"\nfoo = \"x\"\n"),
            ConfigError::UnknownKey { line: 2 }
        );
        assert_eq!(
            e("region = \"us\"\nregion = \"us\"\n"),
            ConfigError::DuplicateKey { line: 2 }
        );
        assert_eq!(e("[a]\n"), ConfigError::Syntax { line: 1 });
        assert_eq!(e("region = us\n"), ConfigError::Syntax { line: 1 });
        assert_eq!(e("region = \"eu\"\n"), ConfigError::BadRegion);
        assert_eq!(e(""), ConfigError::BadRegion);
        assert_eq!(
            e("region = \"us\"\nlicence_id = \"ZZZZ\"\n"),
            ConfigError::BadIdShape
        );
        assert_eq!(
            e("region = \"us\"\nlicence_id = \"K1AB\"\noperator_name = \"a\\nb\"\n"),
            ConfigError::Syntax { line: 3 }
        );
        assert_eq!(
            e("region = \"us\"\nid_interval_secs = 0\n"),
            ConfigError::BadInterval
        );
    }

    #[test]
    fn placeholders_rejected() {
        let t = "region = \"us\"\nlicence_id = \"N0CALL\"\noperator_name = \"x\"\n";
        assert_eq!(
            LicenceConfig::parse_str(t).unwrap_err(),
            ConfigError::Placeholder
        );
        let t =
            "region = \"us\"\nlicence_id = \"K1AB\"\noperator_name = \"REPLACE WITH YOUR NAME\"\n";
        assert_eq!(
            LicenceConfig::parse_str(t).unwrap_err(),
            ConfigError::Placeholder
        );
    }

    #[test]
    fn other_region_needs_explicit_rule() {
        let t = "region = \"other\"\nlicence_id = \"AB1CD\"\noperator_name = \"T P\"\n";
        assert_eq!(
            LicenceConfig::parse_str(t).unwrap_err(),
            ConfigError::OtherNeedsRule
        );
        let t2 = format!("{t}id_mask = \"LLDLL\"\n");
        assert_eq!(
            LicenceConfig::parse_str(&t2).unwrap_err(),
            ConfigError::OtherNeedsRule
        );
        let t3 = format!("{t2}id_interval_secs = 900\n");
        let c = LicenceConfig::parse_str(&t3).unwrap();
        assert_eq!(c.id_interval_secs(), Some(900));
        assert_eq!(
            LicenceConfig::parse_str(
                "region = \"other\"\nid_mask = \"xx\"\nid_interval_secs = 9\n"
            )
            .unwrap_err(),
            ConfigError::BadMask
        );
    }

    #[test]
    fn debug_never_prints_values() {
        let c = LicenceConfig::parse_str(GOOD).unwrap();
        for s in [format!("{c:?}"), format!("{c:#?}")] {
            assert!(!s.contains("K1AB") && !s.contains("Test Person"), "{s}");
            assert!(s.contains("[redacted]"));
        }
    }

    #[test]
    fn errors_never_print_values() {
        let t = "region = \"us\"\nlicence_id = \"SECRETID\"\noperator_name = \"Secret Name\"\n";
        let e = LicenceConfig::parse_str(t).unwrap_err();
        let s = format!("{e} {e:?}");
        assert!(!s.contains("SECRETID") && !s.contains("Secret Name"));
        let bad = "region = \"us\"\nlicence_id = \"K1AB\"\noperator_name = \"Secret\\q\"\n";
        let e = LicenceConfig::parse_str(bad).unwrap_err();
        assert!(!format!("{e} {e:?}").contains("Secret"));
    }

    #[test]
    fn redaction() {
        let c = LicenceConfig::parse_str(GOOD).unwrap();
        assert_eq!(c.log_line(false), "licence_id=K1AB operator=Test Person");
        assert_eq!(
            c.log_line(true),
            "licence_id=[redacted] operator=[redacted]"
        );
        assert_eq!(
            c.redact_text("run by Test Person (k1ab), K1AB again"),
            "run by [redacted] ([redacted]), [redacted] again"
        );
        assert_eq!(c.id(true), Some("[redacted]"));
    }

    #[test]
    fn path_resolution_order() {
        let cli = Path::new("/c.toml");
        assert_eq!(
            resolve_path(Some(cli), Some("/e"), Some("/x"), Some("/h")),
            Some(cli.into())
        );
        assert_eq!(
            resolve_path(None, Some("/e"), Some("/x"), Some("/h")),
            Some("/e".into())
        );
        assert_eq!(
            resolve_path(None, Some(""), Some("/x"), Some("/h")),
            Some("/x/moq-subghz/licence.toml".into())
        );
        assert_eq!(
            resolve_path(None, None, None, Some("/h")),
            Some("/h/.config/moq-subghz/licence.toml".into())
        );
        assert_eq!(resolve_path(None, None, None, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn load_checks_permissions_and_size() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("moq-licence-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("licence.toml");
        std::fs::write(&p, GOOD).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            LicenceConfig::load(&p).unwrap_err(),
            ConfigError::TooPermissive
        );
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(LicenceConfig::load(&p).is_ok());
        std::fs::write(&p, "#".repeat(5000)).unwrap();
        assert_eq!(LicenceConfig::load(&p).unwrap_err(), ConfigError::TooLarge);
        std::fs::remove_file(&p).unwrap();
        std::fs::remove_dir(&dir).unwrap();
        assert!(matches!(LicenceConfig::load(&p), Err(ConfigError::Io(_))));
    }
}
