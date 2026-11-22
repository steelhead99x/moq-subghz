//! Guard: fails if a real-looking licence ID or operator name is committed in a tracked file or
//! sits in a generated report or log under `results/`.
//!
//! What it checks (it cannot know your real values, so it looks for shapes):
//!  1. no tracked file is named `licence.toml`;
//!  2. no text file has a `licence_id` / `operator_name` / `licence_id=` / `operator=` value
//!     other than a placeholder or `[redacted]`;
//!  3. no token in a tracked text file or under `results/` has the shape of a US callsign;
//!  4. if a private licence file loads here (MOQ_SUBGHZ_LICENCE or the XDG path), its actual ID
//!     and name appear nowhere in tracked files or `results/`.
//!
//! The crate's own sources and tests (`moq-subghz-licence/`) are exempt: they hold test data.
//! Rule 3 will flag a real callsign of anyone, which is the point; a false positive on an
//! unrelated token should be fixed by rewording that file.

use moq_subghz_licence::config::{load_from_env, ConfigError};
use moq_subghz_licence::region::us_callsign_plausible;
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn tracked() -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root())
        .args(["ls-files", "-z"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        // No git (a source tarball): the guard still covers results/.
        _ => Vec::new(),
    }
}

fn walk(dir: &Path, out: &mut Vec<String>, base: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out, base);
        } else if let Ok(r) = p.strip_prefix(base) {
            out.push(r.to_string_lossy().into_owned());
        }
    }
}

fn exempt(rel: &str) -> bool {
    rel.starts_with("moq-subghz-licence/") || rel == "licence.example.toml"
}

fn text_of(rel: &str) -> Option<String> {
    let p = root().join(rel);
    let meta = std::fs::metadata(&p).ok()?;
    if meta.len() > 8 * 1024 * 1024 {
        return None;
    }
    String::from_utf8(std::fs::read(&p).ok()?).ok()
}

fn scan_set() -> Vec<String> {
    let mut v: Vec<String> = tracked();
    let mut res = Vec::new();
    walk(&root().join("results"), &mut res, &root());
    for r in res {
        if !v.contains(&r) {
            v.push(r);
        }
    }
    v.retain(|r| !exempt(r));
    v
}

fn safe_value(v: &str) -> bool {
    let v = v
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == ',')
        .trim();
    let u = v.to_ascii_uppercase();
    v.is_empty()
        || u == "[REDACTED]"
        || u == "<UNSET>"
        || u.starts_with("REPLACE")
        || u == "N0CALL"
        || u == "..."
}

/// Value after a `key=` or `key =` marker found at a word start in `line`, if any.
fn key_values<'a>(line: &'a str, key: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = line[from..].find(key) {
        let at = from + p;
        let before_ok = at == 0
            || !(line.as_bytes()[at - 1].is_ascii_alphanumeric()
                || line.as_bytes()[at - 1] == b'_');
        let rest = line[at + key.len()..].trim_start();
        if before_ok {
            if let Some(v) = rest.strip_prefix('=') {
                out.push(v.split_whitespace().next().unwrap_or(""));
            }
        }
        from = at + key.len();
    }
    out
}

#[test]
fn no_licence_file_is_tracked() {
    for f in tracked() {
        let name = f.rsplit('/').next().unwrap();
        assert!(
            !(name.starts_with("licence")
                && name.ends_with(".toml")
                && name != "licence.example.toml"),
            "a licence file is tracked: {f}"
        );
    }
}

#[test]
fn no_real_looking_id_or_name_in_tracked_files_or_results() {
    let mut bad = Vec::new();
    for rel in scan_set() {
        let Some(text) = text_of(&rel) else { continue };
        for (n, line) in text.lines().enumerate() {
            for key in ["licence_id", "operator_name", "operator"] {
                for v in key_values(line, key) {
                    // `operator = ...` in prose or Rust is common; only flag the report/log form.
                    if key == "operator" && !line.contains("licence_id") {
                        continue;
                    }
                    if !safe_value(v) {
                        bad.push(format!("{rel}:{}: value after `{key}`", n + 1));
                    }
                }
            }
            for tok in line.split(|c: char| !c.is_ascii_alphanumeric()) {
                if (4..=6).contains(&tok.len()) && us_callsign_plausible(tok) {
                    bad.push(format!("{rel}:{}: callsign-shaped token", n + 1));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "possible licence data committed or in results/ (values not shown):\n{}",
        bad.join("\n")
    );
}

#[test]
fn private_values_appear_nowhere_public() {
    let cfg = match load_from_env(None) {
        Ok(c) => c,
        Err(ConfigError::NotFound) | Err(_) => return, // no private file here (CI): nothing to compare
    };
    let (id, name) = (cfg.id(false), cfg.name(false));
    let mut hits = Vec::new();
    let mut files = scan_set();
    // The crate's own files are exempt from shape checks but must not hold the real values.
    files.extend(tracked().into_iter().filter(|r| exempt(r)));
    for rel in files {
        let Some(text) = text_of(&rel) else { continue };
        let up = text.to_ascii_uppercase();
        if id.is_some_and(|i| up.contains(&i.to_ascii_uppercase()))
            || name.is_some_and(|n| text.contains(n))
        {
            hits.push(rel);
        }
    }
    assert!(
        hits.is_empty(),
        "your private licence values are in: {hits:?}"
    );
}

#[test]
fn guard_helpers_work() {
    assert!(safe_value("[redacted]") && safe_value("\"REPLACE-ME\"") && safe_value("N0CALL"));
    assert!(!safe_value("\"K1AB\""));
    assert_eq!(
        key_values("licence_id=K1AB operator=Foo", "licence_id"),
        vec!["K1AB"]
    );
    assert!(key_values("my_licence_id=K1AB", "licence_id").is_empty());
}
