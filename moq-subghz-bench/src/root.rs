//! Where the repository's reference inputs (test pictures, factory results) are found.
//!
//! In order: `--repo-root` (or `MOQ_SUBGHZ_ROOT`) as given; the release bundle this binary sits in
//! (`<bundle>/bin/moq-subghz-bench`, inputs under `<bundle>/results` and `<bundle>/media`); the
//! source checkout the binary was built from. The first that has the input wins, so a bundle
//! never reaches for the build machine's path, and a build in a checkout still finds its tree.

use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// The test pictures of `css2015-table`, `css2015-run` and the bench runs.
pub const FRAMES: &str = "results/css-2015/frames";
/// The source picture `css2015-frames` encodes from.
pub const FRAME_SOURCE: &str = "media/frame-f350-rind-120s.png";

/// The source checkout this binary was built from (absent on any other machine).
fn build_checkout() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The places looked in, given the path of the running binary, in order.
fn candidates(exe: Option<&Path>) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Some(dir) = exe.and_then(Path::parent) {
        // <bundle>/bin/<exe>, and a bundle laid out flat
        if let Some(up) = dir.parent() {
            v.push(up.to_path_buf());
        }
        v.push(dir.to_path_buf());
    }
    v.push(build_checkout());
    v
}

/// The root to use: `given` as it is, else the first candidate holding `need` (a path relative to
/// the root). An error names every place looked in and how to point at the inputs.
pub fn resolve(given: Option<PathBuf>, need: &str) -> Result<PathBuf> {
    if let Some(g) = given {
        return Ok(g);
    }
    let exe = std::env::current_exe().ok();
    pick(&candidates(exe.as_deref()), need)
}

fn pick(cands: &[PathBuf], need: &str) -> Result<PathBuf> {
    for c in cands {
        if c.join(need).exists() {
            return Ok(c.canonicalize().unwrap_or_else(|_| c.clone()));
        }
    }
    let tried: Vec<String> = cands
        .iter()
        .map(|c| format!("  {}", c.join(need).display()))
        .collect();
    bail!(
        "{need} not found. Looked in:\n{}\nUse the release archive as unpacked (it ships results/ and \
         media/ beside bin/), or pass --repo-root (or set MOQ_SUBGHZ_ROOT) to a checkout of the \
         repository",
        tried.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;
    use std::fs;

    #[test]
    fn a_bundle_finds_its_own_inputs_before_the_build_checkout() {
        let s = Scratch::new("root-bundle");
        let bundle = s.0.join("moq-subghz-v0.1.0-linux-x86_64");
        fs::create_dir_all(bundle.join("bin")).unwrap();
        fs::create_dir_all(bundle.join(FRAMES)).unwrap();
        let exe = bundle.join("bin/moq-subghz-bench");
        let got = pick(&candidates(Some(&exe)), FRAMES).unwrap();
        assert_eq!(got, bundle.canonicalize().unwrap());
    }

    #[test]
    fn without_a_bundle_the_build_checkout_is_used() {
        let s = Scratch::new("root-none");
        let exe = s.0.join("bin/moq-subghz-bench");
        let got = pick(&candidates(Some(&exe)), FRAMES).unwrap();
        assert_eq!(got, build_checkout().canonicalize().unwrap());
    }

    #[test]
    fn nothing_found_names_the_places_and_the_fix() {
        let s = Scratch::new("root-miss");
        let e = pick(std::slice::from_ref(&s.0), FRAMES)
            .unwrap_err()
            .to_string();
        assert!(e.contains(&s.0.join(FRAMES).display().to_string()), "{e}");
        assert!(
            e.contains("--repo-root") && e.contains("MOQ_SUBGHZ_ROOT"),
            "{e}"
        );
    }

    #[test]
    fn a_given_root_is_used_as_it_is() {
        let p = PathBuf::from("/somewhere/else");
        assert_eq!(resolve(Some(p.clone()), FRAMES).unwrap(), p);
    }
}
