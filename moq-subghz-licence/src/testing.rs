//! Helpers for the integration tests of the tools (feature `testing`). The test values live
//! here, inside this crate, which the leak guard exempts.

use crate::{LicenceConfig, Region};
use std::path::{Path, PathBuf};

/// A US-shaped test ID.
pub const ID: &str = "K1AB";
/// A test operator name.
pub const NAME: &str = "Test Person";

/// The config the test file holds.
pub fn licence() -> LicenceConfig {
    LicenceConfig::from_parts(Region::Us, Some(ID), Some(NAME)).unwrap()
}

/// A fresh directory under the system temp dir. Removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let d = std::env::temp_dir().join(format!(
            "moq-subghz-test-{}-{}-{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&d).unwrap();
        TempDir(d)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Write the test licence file (mode 0600) into the directory and return its path.
    pub fn licence_file(&self) -> PathBuf {
        let p = self.0.join("licence.toml");
        std::fs::write(
            &p,
            format!("region = \"us\"\nlicence_id = \"{ID}\"\noperator_name = \"{NAME}\"\n"),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        p
    }

    /// Every file under `dir`, as lossy text, joined: for scanning produced logs and reports.
    pub fn read_tree(dir: &Path) -> String {
        let mut out = String::new();
        let Ok(rd) = std::fs::read_dir(dir) else {
            return out;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.push_str(&Self::read_tree(&p));
            } else if let Ok(b) = std::fs::read(&p) {
                out.push_str(&p.to_string_lossy());
                out.push('\n');
                out.push_str(&String::from_utf8_lossy(&b));
                out.push('\n');
            }
        }
        out
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // The directory this value created, by its literal path.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
