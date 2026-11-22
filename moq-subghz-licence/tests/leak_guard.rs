//! Guard: fails if a real-looking licence ID or operator name is committed in a tracked file or
//! sits in a generated report or log under `results/`.
//!
//! What it checks (it cannot know your real values, so it looks for shapes):
//!  1. no tracked file is named like a licence file (`licence.toml`, `license.toml`, a backup);
//!  2. no file has a `licence_id` / `operator_name` / `operator` value (`key=value`, `key: value`
//!     or JSON `"key":"value"`) other than a placeholder or `[redacted]`;
//!  3. no token has the shape of a US callsign, in any letter case;
//!  4. if a private licence file loads here (MOQ_SUBGHZ_LICENCE or the XDG path), its actual ID
//!     and name appear nowhere, in any case, JSON-escaped or not.
//!
//! It fails closed: no `git`, a tracked file it cannot read, or a private licence file that exists
//! but does not load (wrong mode, placeholder, syntax) is a test failure, not a skip. Files are
//! scanned as bytes with no size limit and no UTF-8 requirement, so a serial log with one bad byte
//! or a multi-megabyte results log is still checked. The one exception: a tracked file whose first
//! 8 KiB holds a NUL byte (an image, a firmware blob) is binary and gets only the exact private
//! value search, because compressed data would otherwise match the callsign shape by chance.
//! Under `results/` nothing is treated as binary.
//!
//! The crate's own sources and tests (`moq-subghz-licence/`) are exempt from shapes (they hold test
//! data) but not from the private value search. A lower-case token such as `w1ab` that is not a
//! callsign is a false positive: reword that file. The shape is narrow (one or two prefix letters
//! from the FCC set, one digit, one to three letters, 4 to 6 characters in all), so prose hits are rare.

use moq_subghz_licence::config::{load_from_env, ConfigError};
use moq_subghz_licence::region::us_callsign_plausible;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Files git tracks under `root`. Any failure (no git, not a repository, nothing tracked) is an
/// error: an empty list would make every check pass.
fn tracked_with(git: &str, root: &Path) -> Result<Vec<String>, String> {
    let out = Command::new(git)
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .map_err(|e| format!("cannot run `{git}`: {e}"))?;
    if !out.status.success() {
        return Err(format!("`{git} ls-files` failed: {}", out.status));
    }
    let v: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if v.is_empty() {
        return Err("`git ls-files` listed no files".into());
    }
    Ok(v)
}

fn tracked() -> Vec<String> {
    tracked_with("git", &root()).unwrap_or_else(|e| {
        panic!("the leak guard needs git to list tracked files and fails closed without it: {e}")
    })
}

fn walk(dir: &Path, out: &mut Vec<String>, base: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_dir() {
            walk(&p, out, base);
        } else if let Ok(r) = p.strip_prefix(base) {
            out.push(r.to_string_lossy().into_owned());
        }
    }
}

fn exempt(rel: &str) -> bool {
    rel.starts_with("moq-subghz-licence/") || rel == "licence.example.toml"
}

fn scan_set_in(root: &Path, tracked: Vec<String>) -> Vec<String> {
    let mut v = tracked;
    let mut res = Vec::new();
    walk(&root.join("results"), &mut res, root);
    for r in res {
        if !v.contains(&r) {
            v.push(r);
        }
    }
    v
}

fn scan_set() -> Vec<String> {
    let mut v = scan_set_in(&root(), tracked());
    v.retain(|r| !exempt(r));
    v
}

/// Lines of `rel` as raw bytes, plus whether the file is binary (NUL in its first buffer).
/// A `.gz` file is decompressed with `gzip -dc` first (the reports under `results/` are
/// compressed, and the compressed bytes hide everything); a missing or failing gzip panics.
/// A file that vanished (a tracked file deleted in the work tree) yields nothing; any other
/// read error panics.
fn lines_of(root: &Path, rel: &str) -> Option<(bool, Vec<Vec<u8>>)> {
    let path = root.join(rel);
    let mut r: Box<dyn BufRead> = if rel.ends_with(".gz") {
        if !path.exists() {
            return None;
        }
        let out = Command::new("gzip")
            .arg("-dc")
            .arg(&path)
            .output()
            .unwrap_or_else(|e| panic!("cannot run gzip to read {rel}: {e}"));
        assert!(out.status.success(), "gzip could not read {rel}");
        Box::new(std::io::Cursor::new(out.stdout))
    } else {
        match std::fs::File::open(&path) {
            Ok(f) => Box::new(BufReader::new(f)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => panic!("cannot read {rel}: {e}"),
        }
    };
    let binary = r
        .fill_buf()
        .unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
        .contains(&0);
    let mut lines = Vec::new();
    loop {
        let mut buf = Vec::new();
        let n = r
            .read_until(b'\n', &mut buf)
            .unwrap_or_else(|e| panic!("cannot read {rel}: {e}"));
        if n == 0 {
            break;
        }
        lines.push(buf);
    }
    Some((binary, lines))
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

/// Values after a `key=`, `key:`, `key =` or JSON `"key":` marker found at a word start in `line`.
/// A quoted value is taken whole (so a name with spaces is one value); an unquoted one ends at
/// whitespace, a comma or a closing brace.
fn key_values<'a>(line: &'a str, key: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(p) = line[from..].find(key) {
        let at = from + p;
        let before_ok = at == 0
            || !(line.as_bytes()[at - 1].is_ascii_alphanumeric()
                || line.as_bytes()[at - 1] == b'_');
        let mut rest = &line[at + key.len()..];
        from = at + key.len();
        if !before_ok {
            continue;
        }
        // A closing quote of a JSON or TOML-ish key.
        rest = rest.strip_prefix(['"', '\'']).unwrap_or(rest).trim_start();
        let Some(v) = rest.strip_prefix(['=', ':']) else {
            continue;
        };
        let v = v.trim_start();
        let end = match v.chars().next() {
            Some(q @ ('"' | '\'')) => {
                // Up to the closing quote that is not backslash-escaped.
                let b = v.as_bytes();
                let mut i = 1;
                while i < b.len() && b[i] != q as u8 {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                (i + 1).min(v.len())
            }
            _ => v
                .find(|c: char| c.is_whitespace() || c == ',' || c == '}')
                .unwrap_or(v.len()),
        };
        out.push(&v[..end]);
    }
    out
}

fn is_licence_file_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    (n.contains("licence") || n.contains("license"))
        && n.contains(".toml")
        && n != "licence.example.toml"
}

/// Tokens that fit the callsign shape but are known not to be one.
const NOT_CALLSIGNS: &[&str] = &[
    // The FEC label "k=8, encoded" in results/loopback.
    "k8enc",
];

/// Whether `tok` has the shape of a US callsign in a single letter case. Mixed case is ignored
/// (base64 and identifiers are full of it), as are lower-case tokens made only of hex digits
/// (`ae6f` in a serial log); both are the false-positive costs of the case-insensitive scan.
fn callsign_like(tok: &str) -> bool {
    if !(4..=6).contains(&tok.len()) || NOT_CALLSIGNS.contains(&tok.to_ascii_lowercase().as_str()) {
        return false;
    }
    let has_lower = tok.bytes().any(|c| c.is_ascii_lowercase());
    let has_upper = tok.bytes().any(|c| c.is_ascii_uppercase());
    if has_lower && has_upper {
        return false;
    }
    if has_lower
        && tok
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return false;
    }
    us_callsign_plausible(&tok.to_ascii_uppercase())
}

/// Shape findings (file:line, values never shown) in the given files.
fn shape_findings(root: &Path, files: &[String]) -> Vec<String> {
    let mut bad = Vec::new();
    for rel in files {
        let Some((binary, lines)) = lines_of(root, rel) else {
            continue;
        };
        if binary {
            continue;
        }
        for (n, raw) in lines.iter().enumerate() {
            let line = String::from_utf8_lossy(raw);
            let lower = line.to_ascii_lowercase();
            let has_id = lower.contains("licence_id") || lower.contains("license_id");
            for key in ["licence_id", "license_id", "operator_name", "operator"] {
                for v in key_values(&line, key) {
                    // `operator = ...` in prose or Rust is common; only flag the report/log form.
                    if key == "operator" && !has_id {
                        continue;
                    }
                    if !safe_value(v) {
                        bad.push(format!("{rel}:{}: value after `{key}`", n + 1));
                    }
                }
            }
            for tok in line.split(|c: char| !c.is_ascii_alphanumeric()) {
                if callsign_like(tok) {
                    bad.push(format!("{rel}:{}: callsign-shaped token", n + 1));
                }
            }
        }
    }
    bad
}

/// JSON string-body escapes of `s` (as `serde_json` and most loggers write it): quotes, backslash
/// and control characters, with non-ASCII as `\uXXXX` (lower and upper case hex, surrogate pairs).
fn json_escapes(s: &str) -> Vec<String> {
    let mut plain = String::new();
    let mut u = String::new();
    for c in s.chars() {
        match c {
            '"' => {
                plain.push_str("\\\"");
                u.push_str("\\\"");
            }
            '\\' => {
                plain.push_str("\\\\");
                u.push_str("\\\\");
            }
            c if c.is_ascii() => {
                plain.push(c);
                u.push(c);
            }
            c => {
                plain.push(c);
                let mut b = [0u16; 2];
                for unit in c.encode_utf16(&mut b) {
                    u.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    vec![plain, u]
}

/// Collapses runs of whitespace so a name wrapped or padded across spaces still matches.
fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Files that hold the private `id` or `name` in any case, JSON-escaped or not.
fn private_hits(
    root: &Path,
    files: &[String],
    id: Option<&str>,
    name: Option<&str>,
) -> Vec<String> {
    let id = id.map(|i| i.to_ascii_lowercase()).filter(|i| !i.is_empty());
    let names: Vec<String> = name
        .filter(|n| !n.is_empty())
        .map(|n| {
            let mut v: Vec<String> = json_escapes(&squash(n));
            v.push(squash(n));
            v.into_iter().map(|x| x.to_lowercase()).collect()
        })
        .unwrap_or_default();
    let mut hits = Vec::new();
    for rel in files {
        let Some((_, lines)) = lines_of(root, rel) else {
            continue;
        };
        let found = lines.iter().any(|raw| {
            let line = String::from_utf8_lossy(raw);
            let ascii = line.to_ascii_lowercase();
            let squashed = squash(&line.to_lowercase());
            id.as_ref().is_some_and(|i| ascii.contains(i.as_str()))
                || names
                    .iter()
                    .any(|n| squashed.contains(n.as_str()) || ascii.contains(n.as_str()))
        });
        if found {
            hits.push(rel.clone());
        }
    }
    hits
}

#[test]
fn no_licence_file_is_tracked() {
    for f in tracked() {
        let name = f.rsplit('/').next().unwrap();
        assert!(
            !is_licence_file_name(name),
            "a licence file is tracked: {f}"
        );
    }
}

#[test]
fn no_real_looking_id_or_name_in_tracked_files_or_results() {
    let bad = shape_findings(&root(), &scan_set());
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
        // No private file here (CI): nothing to compare.
        Err(ConfigError::NotFound) => return,
        // A file that exists but will not load would silently disable this check.
        Err(e) => panic!("the private licence file does not load, so it cannot be checked: {e}"),
    };
    let mut files = scan_set();
    // The crate's own files are exempt from shape checks but must not hold the real values.
    files.extend(tracked().into_iter().filter(|r| exempt(r)));
    let hits = private_hits(&root(), &files, cfg.id(false), cfg.name(false));
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
    // JSON, colon form, quoted value with a space.
    assert_eq!(
        key_values(r#"{"operator":"Pat Q","x":1}"#, "operator"),
        vec![r#""Pat Q""#]
    );
    assert_eq!(
        key_values("operator_name: Pat", "operator_name"),
        vec!["Pat"]
    );
    assert!(is_licence_file_name("license.toml") && is_licence_file_name("my-licence.toml.bak"));
    assert!(!is_licence_file_name("licence.example.toml") && !is_licence_file_name("notes.md"));
}

// Fixtures are built here, never committed: every callsign, name and bad byte is assembled at run
// time in a scratch directory under the target dir.

fn scratch(name: &str) -> PathBuf {
    let d = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("leak_guard_{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn put(dir: &Path, rel: &str, bytes: &[u8]) -> String {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, bytes).unwrap();
    rel.to_string()
}

/// A synthetic callsign-shaped token, assembled so this file never spells one.
fn fake_call(lower: bool) -> String {
    let c = format!("{}{}{}", "W", 1, "ZQ");
    if lower {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

#[test]
fn shape_scan_sees_bad_utf8_and_huge_files() {
    let d = scratch("shape");
    let call = fake_call(false);
    let mut bad_utf8 = b"R 12 0 \xff\xfe raw serial noise ".to_vec();
    bad_utf8.extend_from_slice(format!("see {call} here\n").as_bytes());
    let a = put(&d, "results/raw.log", &bad_utf8);
    // Over 8 MiB with the token on the last line.
    let mut big = Vec::new();
    for _ in 0..(9 * 1024) {
        big.extend_from_slice(&[b'x'; 1023]);
        big.push(b'\n');
    }
    big.extend_from_slice(format!("licence_id={call}\n").as_bytes());
    let b = put(&d, "results/big.log", &big);
    let found = shape_findings(&d, &[a.clone(), b.clone()]);
    assert_eq!(found.len(), 3, "{found:?}"); // raw.log token, big.log key value and token
    assert!(found[0].starts_with(&a) && found[1].starts_with(&b));
}

#[test]
fn shape_scan_is_case_insensitive_and_reads_json() {
    let d = scratch("case");
    let low = fake_call(true);
    let f = put(
        &d,
        "notes.txt",
        format!("worked {low} on 33cm\n").as_bytes(),
    );
    assert_eq!(shape_findings(&d, &[f]).len(), 1);
    let j = put(
        &d,
        "summary.json",
        br#"{"mode":"licensed","licence_id":"[redacted]","operator":"Pat Quinn"}"#,
    );
    assert_eq!(shape_findings(&d, &[j]).len(), 1, "name in JSON");
    let ok = put(
        &d,
        "ok.json",
        br#"{"licence_id":"[redacted]","operator":"[redacted]","operator_name":"N0CALL"}"#,
    );
    assert!(shape_findings(&d, &[ok]).is_empty());
    // Ordinary words and hex must not trip the shape.
    let prose = put(
        &d,
        "prose.md",
        b"the quick brown fox a1b2 c3d4e5 deadbeef 0x1f2e\n",
    );
    assert!(shape_findings(&d, &[prose]).is_empty());
}

#[test]
fn binary_is_skipped_for_shapes_and_gzip_is_read() {
    let d = scratch("binary");
    let mut blob = vec![0u8, 1, 2];
    blob.extend_from_slice(format!(" {} ", fake_call(false)).as_bytes());
    let a = put(&d, "media/x.bin", &blob);
    assert!(shape_findings(&d, &[a]).is_empty());
    // The same text, compressed: found.
    let txt = put(
        &d,
        "results/x.log",
        format!("id {}\n", fake_call(true)).as_bytes(),
    );
    let st = Command::new("gzip")
        .arg("-f")
        .arg(d.join(&txt))
        .status()
        .unwrap();
    assert!(st.success());
    let gz = "results/x.log.gz".to_string();
    assert_eq!(shape_findings(&d, std::slice::from_ref(&gz)).len(), 1);
    assert_eq!(
        private_hits(&d, std::slice::from_ref(&gz), Some(&fake_call(false)), None),
        vec![gz]
    );
}

#[test]
fn private_values_found_in_any_case_json_and_bad_utf8() {
    let d = scratch("private");
    let id = fake_call(false);
    let name = "Zoë \"Q\" Ríos";
    let mut raw = vec![0xffu8, 0xfe];
    raw.extend_from_slice(format!(" id {} ", id.to_ascii_lowercase()).as_bytes());
    let by_id = put(&d, "a.log", &raw);
    let escaped = put(&d, "b.json", br#"{"operator":"Zo\u00eb \"Q\" R\u00edos"}"#);
    let plain = put(&d, "c.md", "operator:  zoë   \"q\" ríos\n".as_bytes());
    let clean = put(&d, "d.md", b"nothing here\n");
    let hits = private_hits(
        &d,
        &[by_id.clone(), escaped.clone(), plain.clone(), clean],
        Some(&id),
        Some(name),
    );
    assert_eq!(hits, vec![by_id, escaped, plain]);
}

#[test]
fn listing_fails_closed_without_git() {
    let d = scratch("nogit");
    assert!(tracked_with("moq-subghz-no-such-git-binary", &d).is_err());
    // Present but not a repository.
    if let Ok(v) = tracked_with("git", &d) {
        // Only possible if the scratch dir sits inside a work tree that tracks nothing under it.
        assert!(!v.is_empty());
    }
}

#[test]
fn unreadable_file_is_not_skipped() {
    let d = scratch("unreadable");
    // A directory where a file is expected is a read error other than NotFound.
    std::fs::create_dir_all(d.join("sub")).unwrap();
    let r = std::panic::catch_unwind(|| {
        lines_of(
            Path::new(env!("CARGO_TARGET_TMPDIR")),
            "leak_guard_unreadable/sub",
        )
    });
    assert!(r.is_err());
    assert!(lines_of(&d, "missing.txt").is_none());
}
