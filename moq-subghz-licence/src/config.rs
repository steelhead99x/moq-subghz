//! Licence file loader. File only, strict, and never prints the values.
//!
//! The file is a strict subset of TOML: `key = "string"` or `key = 123`, `#` comments, blank
//! lines. Tables, arrays, duplicate keys, unknown keys and any other syntax are errors. Error
//! values never contain the ID or the name.

use crate::region::Region;
use crate::{Redaction, REDACTED};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Most the file may hold.
const MAX_BYTES: u64 = 4096;

#[cfg(all(unix, any(target_os = "linux", target_os = "android")))]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
const O_NONBLOCK: i32 = 0x4; // macOS and the BSDs

#[cfg(unix)]
extern "C" {
    fn geteuid() -> u32;
}

/// Why a licence file or value was rejected. No variant carries the licence ID or name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Io(String),
    TooLarge,
    /// The file is readable by group or others (Unix). `chmod 600` it.
    TooPermissive,
    /// The path is not a regular file (a FIFO, a device or a directory).
    NotRegular,
    /// The file belongs to another user (Unix).
    WrongOwner,
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
    /// The region exists but its rules have not been reviewed, so licensed mode is blocked for it.
    RegionNotReviewed,
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
            Self::NotRegular => write!(f, "the licence file is not a regular file"),
            Self::WrongOwner => write!(f, "the licence file is owned by another user"),
            Self::Syntax { line } => write!(f, "licence file line {line}: not `key = \"value\"`"),
            Self::DuplicateKey { line } => write!(f, "licence file line {line}: duplicate key"),
            Self::UnknownKey { line } => write!(f, "licence file line {line}: unknown key"),
            Self::BadRegion => write!(f, "region must be \"us\""),
            Self::BadIdShape => write!(f, "licence ID does not look like a valid ID for the region (the tools check the shape only and cannot verify it against any database)"),
            Self::BadName => write!(f, "operator name is empty, too long or has control, bidirectional or invisible formatting characters"),
            Self::RegionNotReviewed => write!(f, "region \"other\" is blocked: only \"us\" is enabled until a country has been reviewed with citations"),
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
}

impl fmt::Debug for LicenceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let set = |o: &Option<String>| if o.is_some() { REDACTED } else { "<unset>" };
        let region = match self.region {
            Region::Us => "us",
        };
        f.debug_struct("LicenceConfig")
            .field("region", &region)
            .field("licence_id", &set(&self.id))
            .field("operator_name", &set(&self.name))
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
                if n.chars().count() > 80 || n.chars().any(is_unsafe_char) {
                    return Err(ConfigError::BadName);
                }
                Some(n.to_string())
            }
            None => None,
        };
        Ok(Self { region, id, name })
    }

    /// Parse the text of a licence file.
    pub fn parse_str(text: &str) -> Result<Self, ConfigError> {
        // A UTF-8 byte order mark is harmless; without this it would glue itself to the first key.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut seen: Vec<&'static str> = Vec::new();
        let (mut region, mut id, mut name) = (None, None, None);
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
                _ => return Err(ConfigError::UnknownKey { line }),
            };
            if seen.contains(&key) {
                return Err(ConfigError::DuplicateKey { line });
            }
            seen.push(key);
            let val = val.trim();
            let s = parse_string(val).ok_or(ConfigError::Syntax { line })?;
            match key {
                "region" => region = Some(s),
                "licence_id" => id = Some(s),
                _ => name = Some(s),
            }
        }
        let region = Region::from_name(region.as_deref().unwrap_or(""))?;
        if id.as_deref().is_some_and(|s| is_placeholder(s.trim())) {
            return Err(ConfigError::Placeholder);
        }
        Self::from_parts(region, id.as_deref(), name.as_deref())
    }

    /// Read and parse a file. The checks run on the opened file, not on the path, so a swap
    /// between check and read cannot change what was checked: it must be a regular file of at
    /// most 4 KiB, owned by the running user and (Unix) not readable by group or others. A
    /// symlink is followed and its target is what gets checked. The read itself is capped.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let io = |e: std::io::Error| match e.kind() {
            std::io::ErrorKind::NotFound => ConfigError::NotFound,
            k => ConfigError::Io(k.to_string()),
        };
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::OpenOptionsExt;
            // O_NONBLOCK so opening a FIFO with no writer fails the regular-file check below
            // instead of hanging.
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(O_NONBLOCK)
                .open(path)
        };
        // Not Unix: the permission and owner checks cannot be made, so refuse.
        #[cfg(not(unix))]
        let file: std::io::Result<std::fs::File> = Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "unsupported platform",
        ));
        let file = file.map_err(io)?;
        let meta = file.metadata().map_err(io)?;
        if !meta.is_file() {
            return Err(ConfigError::NotRegular);
        }
        if meta.len() > MAX_BYTES {
            return Err(ConfigError::TooLarge);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if meta.mode() & 0o077 != 0 {
                return Err(ConfigError::TooPermissive);
            }
            // SAFETY: geteuid takes no arguments and cannot fail.
            if meta.uid() != unsafe { geteuid() } {
                return Err(ConfigError::WrongOwner);
            }
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(ConfigError::TooLarge);
        }
        let text =
            String::from_utf8(bytes).map_err(|_| ConfigError::Io("invalid data".to_string()))?;
        Self::parse_str(&text)
    }

    pub fn region(&self) -> &Region {
        &self.region
    }

    /// Longest gap between identifications for this region, in seconds.
    pub fn id_interval_secs(&self) -> u32 {
        self.region.id_interval_secs()
    }

    pub fn has_id(&self) -> bool {
        self.id.is_some()
    }

    pub fn has_name(&self) -> bool {
        self.name.is_some()
    }

    /// The ID as it goes on air (clear text, [`crate::ident::id_frame`]). Not for logs.
    pub(crate) fn on_air_id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The ID, or [`REDACTED`]. `None` when unset.
    pub(crate) fn id(&self, r: Redaction) -> Option<&str> {
        self.id.as_deref().map(|s| shown(s, r))
    }

    /// The name, or [`REDACTED`]. `None` when unset.
    pub(crate) fn name(&self, r: Redaction) -> Option<&str> {
        self.name.as_deref().map(|s| shown(s, r))
    }

    /// The one place that formats the ID and name for run logs and reports:
    /// `licence_id=... operator=...`. Every tool writes the pair through this function (via
    /// [`crate::Station::log_line`]); with [`Redaction::Redact`] (`--redact-licence`) both
    /// values become `[redacted]`.
    pub fn log_line(&self, r: Redaction) -> String {
        format!(
            "licence_id={} operator={}",
            self.id(r).unwrap_or("<unset>"),
            self.name(r).unwrap_or("<unset>")
        )
    }

    /// True when `text` holds the ID (any case) or the name. For scanning produced logs and
    /// reports.
    pub fn appears_in(&self, text: &str) -> bool {
        self.id
            .as_deref()
            .is_some_and(|id| text.to_ascii_uppercase().contains(&id.to_ascii_uppercase()))
            || self.name.as_deref().is_some_and(|n| text.contains(n))
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

fn shown(s: &str, r: Redaction) -> &str {
    match r {
        Redaction::Show => s,
        Redaction::Redact => REDACTED,
    }
}

/// Control characters, line and paragraph separators and invisible formatting characters
/// (Unicode category Cf: bidirectional overrides, zero-width characters, the BOM, tags). They
/// would flow into logs and could reorder or hide text.
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{ad}' | '\u{600}'..='\u{605}' | '\u{61c}' | '\u{6dd}' | '\u{70f}' | '\u{8e2}'
            | '\u{180e}' | '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206f}' | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}' | '\u{110bd}' | '\u{110cd}' | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0001}' | '\u{e0020}'..='\u{e007f}')
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
    // The word YOUR (from "REPLACE WITH YOUR NAME", "YOUR-ID"), not any name that contains it.
    u.starts_with("REPLACE")
        || u.contains("PLACEHOLDER")
        || u == "N0CALL"
        || u.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w == "YOUR")
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
    // The XDG spec says a relative XDG_CONFIG_HOME is invalid and must be ignored.
    if let Some(x) = nonempty(xdg_config_home).filter(|x| Path::new(x).is_absolute()) {
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
        assert_eq!(c.id(Redaction::Show), Some("K1AB"));
        assert_eq!(c.name(Redaction::Show), Some("Test Person"));
        assert_eq!(c.id_interval_secs(), 600);
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
        // The user-supplied mask and interval keys are gone.
        assert_eq!(
            e("region = \"us\"\nid_mask = \"LLDLL\"\n"),
            ConfigError::UnknownKey { line: 2 }
        );
        assert_eq!(
            e("region = \"us\"\nid_interval_secs = 900\n"),
            ConfigError::UnknownKey { line: 2 }
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
    fn other_region_is_blocked_whatever_the_file_says() {
        let t = "region = \"other\"\nlicence_id = \"AB1CD\"\noperator_name = \"T P\"\n";
        assert_eq!(
            LicenceConfig::parse_str(t).unwrap_err(),
            ConfigError::RegionNotReviewed
        );
        // Supplying your own rule does not unblock it: those keys no longer exist.
        let t2 = format!("{t}id_mask = \"LLDLL\"\nid_interval_secs = 900\n");
        assert_eq!(
            LicenceConfig::parse_str(&t2).unwrap_err(),
            ConfigError::UnknownKey { line: 4 }
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
        assert_eq!(
            c.log_line(Redaction::Show),
            "licence_id=K1AB operator=Test Person"
        );
        assert_eq!(
            c.log_line(Redaction::Redact),
            "licence_id=[redacted] operator=[redacted]"
        );
        assert_eq!(
            c.redact_text("run by Test Person (k1ab), K1AB again"),
            "run by [redacted] ([redacted]), [redacted] again"
        );
        assert_eq!(c.id(Redaction::Redact), Some("[redacted]"));
        assert!(c.appears_in("by test person? no. by Test Person yes"));
        assert!(c.appears_in("callsign k1ab"));
        assert!(!c.appears_in("nothing here"));
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
        // A relative XDG_CONFIG_HOME is ignored.
        assert_eq!(
            resolve_path(None, None, Some("rel"), Some("/h")),
            Some("/h/.config/moq-subghz/licence.toml".into())
        );
        assert_eq!(
            resolve_path(None, None, None, Some("/h")),
            Some("/h/.config/moq-subghz/licence.toml".into())
        );
        assert_eq!(resolve_path(None, None, None, None), None);
    }

    #[test]
    fn names_with_hidden_or_reordering_characters_are_refused() {
        for bad in [
            "Jane \u{202e}Doe",
            "Jane\u{200b}Doe",
            "Jane\u{2066}Doe",
            "\u{feff}Jane",
            "Jane\u{2028}Doe",
            "Jane\tDoe",
            "Jane\u{e0041}Doe",
        ] {
            assert_eq!(
                LicenceConfig::from_parts(Region::Us, Some("K1AB"), Some(bad)).unwrap_err(),
                ConfigError::BadName,
                "{bad:?}"
            );
        }
        for ok in ["Jos\u{e9} Mu\u{f1}oz", "Yourkevich", "Anne-Marie O'Neil"] {
            assert!(LicenceConfig::from_parts(Region::Us, Some("K1AB"), Some(ok)).is_ok());
        }
    }

    #[test]
    fn placeholders_match_the_word_your_only() {
        for ph in [
            "YOUR NAME",
            "your-id",
            "Replace me",
            "x placeholder x",
            "N0CALL",
        ] {
            assert!(is_placeholder(ph), "{ph}");
        }
        for ok in ["Yourkevich", "Younger", "Pat Youro"] {
            assert!(!is_placeholder(ok), "{ok}");
        }
    }

    #[test]
    fn a_byte_order_mark_is_ignored() {
        let t = format!("\u{feff}{GOOD}");
        assert!(LicenceConfig::parse_str(&t).is_ok());
    }

    #[cfg(unix)]
    mod load {
        use super::*;
        use std::os::unix::fs::{symlink, PermissionsExt};

        struct Dir(PathBuf);
        impl Dir {
            fn new(tag: &str) -> Self {
                let d = std::env::temp_dir()
                    .join(format!("moq-licence-test-{}-{tag}", std::process::id()));
                std::fs::create_dir_all(&d).unwrap();
                Dir(d)
            }
            fn file(&self, name: &str, body: &str, mode: u32) -> PathBuf {
                let p = self.0.join(name);
                std::fs::write(&p, body).unwrap();
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
                p
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                // Delete the files we made one by one, then the directory itself.
                if let Ok(rd) = std::fs::read_dir(&self.0) {
                    for e in rd.flatten() {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
                let _ = std::fs::remove_dir(&self.0);
            }
        }

        #[test]
        fn checks_permissions_and_size() {
            let d = Dir::new("perm");
            let p = d.file("licence.toml", GOOD, 0o644);
            assert_eq!(
                LicenceConfig::load(&p).unwrap_err(),
                ConfigError::TooPermissive
            );
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert!(LicenceConfig::load(&p).is_ok());
            let big = d.file("big.toml", &"#".repeat(5000), 0o600);
            assert_eq!(
                LicenceConfig::load(&big).unwrap_err(),
                ConfigError::TooLarge
            );
            let exact = d.file("exact.toml", &format!("{GOOD}{}", "#".repeat(4000)), 0o600);
            assert!(LicenceConfig::load(&exact).is_ok());
            assert_eq!(
                LicenceConfig::load(&d.0.join("missing.toml")).unwrap_err(),
                ConfigError::NotFound
            );
            let bad = d.file("bad.toml", "", 0o600);
            std::fs::write(&bad, [0xff, 0xfe, b'\n']).unwrap();
            assert!(matches!(LicenceConfig::load(&bad), Err(ConfigError::Io(_))));
        }

        #[test]
        fn refuses_what_is_not_a_regular_file() {
            let d = Dir::new("kind");
            // A directory.
            assert_eq!(
                LicenceConfig::load(&d.0).unwrap_err(),
                ConfigError::NotRegular
            );
            // A FIFO with no writer: must fail fast, not block. (mkfifo is in coreutils.)
            let fifo = d.0.join("fifo.toml");
            let made = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            assert!(made, "mkfifo is needed for this test");
            std::fs::set_permissions(&fifo, std::fs::Permissions::from_mode(0o600)).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            let f2 = fifo.clone();
            std::thread::spawn(move || tx.send(LicenceConfig::load(&f2)).unwrap());
            let r = rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("load blocked on a FIFO");
            assert_eq!(r.unwrap_err(), ConfigError::NotRegular);
            // A character device.
            assert_eq!(
                LicenceConfig::load(Path::new("/dev/null")).unwrap_err(),
                ConfigError::NotRegular
            );
        }

        #[test]
        fn a_symlink_is_judged_by_its_target() {
            let d = Dir::new("link");
            let target = d.file("real.toml", GOOD, 0o600);
            let link = d.0.join("licence.toml");
            symlink(&target, &link).unwrap();
            assert!(LicenceConfig::load(&link).is_ok());
            let loose = d.file("loose.toml", GOOD, 0o644);
            let link2 = d.0.join("loose-link.toml");
            symlink(&loose, &link2).unwrap();
            assert_eq!(
                LicenceConfig::load(&link2).unwrap_err(),
                ConfigError::TooPermissive
            );
        }
    }
}
