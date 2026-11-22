//! Helpers for the tests: the repository root and a scratch directory that cleans up after itself.

use std::path::{Path, PathBuf};

/// This repository's root.
pub fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// A fresh directory under the temp dir, removed (by its exact path) on drop.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("moq-subghz-bench-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("create scratch dir");
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Copy a directory tree.
pub fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create dir");
    for e in std::fs::read_dir(from).expect("read dir") {
        let e = e.expect("entry");
        let dst = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).expect("copy");
        }
    }
}

/// A tiny deterministic generator (xorshift64*), so the fuzz-style tests are the same on every run.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    /// `src` with a few random edits: flipped, deleted or inserted bytes, or a chunk replaced by
    /// an awkward token (`nan`, `inf`, a huge number, an empty field, a quote, a newline).
    pub fn mutate(&mut self, src: &[u8]) -> Vec<u8> {
        const TOKENS: [&str; 14] = [
            "nan",
            "NaN",
            "inf",
            "-inf",
            "1e999",
            "-1e999",
            "9223372036854775807",
            "-9223372036854775808",
            "99999999999999999999",
            "0",
            "-1",
            "",
            "\"",
            "\r\n,\n,",
        ];
        let mut v = src.to_vec();
        for _ in 0..1 + self.below(4) {
            if v.is_empty() {
                v.push(self.next() as u8);
                continue;
            }
            let at = self.below(v.len());
            match self.below(5) {
                0 => v[at] ^= 1 << self.below(8),
                1 => {
                    v.remove(at);
                }
                2 => v.insert(at, self.next() as u8),
                3 => v.truncate(at),
                _ => {
                    let t = TOKENS[self.below(TOKENS.len())];
                    let end = (at + 1 + self.below(8)).min(v.len());
                    v.splice(at..end, t.bytes());
                }
            }
        }
        v
    }
}
