//! Safe output files for the offline tools: refuse an output that is the input, and write
//! through a temporary file that replaces the output only after the run succeeded, so a failure
//! never destroys an existing file.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Do two paths name the same file (same device and inode), through symlinks and hard links?
/// A path that does not exist is not the same as anything.
pub fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    let (ma, mb) = match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => (ma, mb),
        (Err(e), _) | (_, Err(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        (Err(e), _) | (_, Err(e)) => return Err(e),
    };
    Ok(ma.dev() == mb.dev() && ma.ino() == mb.ino())
}

/// Where to write so that `final_path` is replaced atomically on [`Pending::commit`].
#[derive(Debug)]
pub struct Pending {
    /// Write the output here.
    pub write_path: PathBuf,
    /// Replaced by `write_path` on commit. `None` when the output is not a regular file (a
    /// device such as /dev/null, a pipe) and is written in place.
    final_path: Option<PathBuf>,
    done: bool,
}

/// Plan an output at `out`. With `input`, refuse an output that is the same file (also via a
/// symlink or a hard link). An existing regular output is replaced only on commit; the
/// temporary file lives in the same directory so the rename cannot cross file systems.
pub fn plan(out: &Path, input: Option<&Path>) -> io::Result<Pending> {
    if let Some(inp) = input {
        if same_file(out, inp)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "--out {} is the same file as --in {}: refusing to overwrite the input",
                    out.display(),
                    inp.display()
                ),
            ));
        }
    }
    let final_path = match std::fs::symlink_metadata(out) {
        Ok(m) if m.file_type().is_symlink() => {
            // Replace what the link points at, not the link; a dangling link has no target.
            match std::fs::canonicalize(out) {
                Ok(p) => p,
                Err(e) => {
                    return Err(io::Error::new(
                        e.kind(),
                        format!("--out {} is a symlink I cannot resolve: {e}", out.display()),
                    ))
                }
            }
        }
        Ok(_) => std::fs::canonicalize(out)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let name = out
                .file_name()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "--out has no name"))?;
            let dir = match out.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            std::fs::canonicalize(dir)?.join(name)
        }
        Err(e) => return Err(e),
    };
    if let Some(inp) = input {
        // the resolved target may be the input even when `out` itself looked different
        if same_file(&final_path, inp)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "--out {} resolves to the input {}: refusing to overwrite it",
                    out.display(),
                    inp.display()
                ),
            ));
        }
    }
    if let Ok(m) = std::fs::metadata(&final_path) {
        if !m.is_file() {
            return Ok(Pending {
                write_path: final_path,
                final_path: None,
                done: true,
            });
        }
    }
    let name = final_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = final_path.with_file_name(format!(".{name}.tmp.{}.{nanos}", std::process::id()));
    Ok(Pending {
        write_path: tmp,
        final_path: Some(final_path),
        done: false,
    })
}

impl Pending {
    /// Flush the temporary file to disk and rename it over the output.
    pub fn commit(mut self) -> io::Result<()> {
        if let Some(fin) = self.final_path.clone() {
            std::fs::File::open(&self.write_path)?.sync_all()?;
            if let Ok(m) = std::fs::metadata(&fin) {
                // keep the mode of the file being replaced
                let _ = std::fs::set_permissions(&self.write_path, m.permissions());
            }
            std::fs::rename(&self.write_path, &fin)?;
        }
        self.done = true;
        Ok(())
    }
}

impl Drop for Pending {
    /// An uncommitted temporary file is removed; the existing output is untouched.
    fn drop(&mut self) {
        if !self.done && self.final_path.is_some() {
            let _ = std::fs::remove_file(&self.write_path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moq-outfile-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn same_path_symlink_and_hard_link_are_refused() {
        let d = dir("same");
        let inp = d.join("cap.txt");
        std::fs::write(&inp, "R 1 0 0 aa\n").unwrap();
        let sym = d.join("sym.txt");
        std::os::unix::fs::symlink(&inp, &sym).unwrap();
        let hard = d.join("hard.txt");
        std::fs::hard_link(&inp, &hard).unwrap();
        let rel = d.join(".").join("cap.txt");
        for out in [&inp, &sym, &hard, &rel] {
            let e = plan(out, Some(&inp)).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{out:?}");
        }
        assert_eq!(std::fs::read(&inp).unwrap(), b"R 1 0 0 aa\n");
        // a different file is fine
        assert!(plan(&d.join("other.txt"), Some(&inp)).is_ok());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_dangling_symlink_is_refused() {
        let d = dir("dangle");
        let sym = d.join("sym.txt");
        std::os::unix::fs::symlink(d.join("nowhere"), &sym).unwrap();
        assert!(plan(&sym, None).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn commit_replaces_and_drop_keeps_the_old_file() {
        let d = dir("atomic");
        let out = d.join("out.txt");
        std::fs::write(&out, "old").unwrap();
        // a failed run: the temporary file is removed and the old output survives
        let p = plan(&out, None).unwrap();
        std::fs::write(&p.write_path, "partial").unwrap();
        let tmp = p.write_path.clone();
        assert_ne!(tmp, out);
        drop(p);
        assert!(!tmp.exists());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "old");
        // a good run replaces it
        let p = plan(&out, None).unwrap();
        std::fs::write(&p.write_path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "old");
        p.commit().unwrap();
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "new");
        // writing through a symlink replaces the target, not the link
        let sym = d.join("sym.txt");
        std::os::unix::fs::symlink(&out, &sym).unwrap();
        let p = plan(&sym, None).unwrap();
        std::fs::write(&p.write_path, "via").unwrap();
        p.commit().unwrap();
        assert!(std::fs::symlink_metadata(&sym)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read_to_string(&out).unwrap(), "via");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn devices_are_written_in_place() {
        let p = plan(Path::new("/dev/null"), None).unwrap();
        assert_eq!(p.write_path, Path::new("/dev/null"));
        p.commit().unwrap();
    }
}
