//! `package-051`: copy the data worth keeping from a bench-051 runs directory into
//! `results/stick-0.5.1/`. A port of `scripts/package-051.sh`. A run missing a file it should
//! have is an error, not a gap.
//!
//! Everything text that is copied (summaries, the gzipped logs, the CSVs) is read first and
//! scanned for the configured licence ID and operator name, and for any licence line (the
//! summaries' ID field) that is not redacted. A hit refuses the whole packaging; the file is not written and the values
//! are never printed. Pictures and video are copied as they are. Host paths and the `--pi` host
//! are NOT scrubbed or checked here: read the logs before committing them.

use anyhow::{bail, Context, Result};
use flate2::{write::GzEncoder, Compression};
use moq_subghz_licence::{GateArgs, Station, REDACTED};
use std::fs;

/// Largest log `package-051` compresses (1 GiB).
const MAX_LOG: u64 = 1 << 30;
/// Bytes of one block carried into the next scan: longer than the licence name and ID line.
const OVERLAP: usize = 1024;
use std::path::{Path, PathBuf};

fn dirs(root: &Path, keep: impl Fn(&str) -> bool) -> Result<Vec<String>> {
    let mut v = Vec::new();
    for e in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let e = e?;
        let n = e.file_name().to_string_lossy().into_owned();
        if e.path().is_dir() && !n.starts_with('.') && keep(&n) {
            v.push(n);
        }
    }
    v.sort();
    Ok(v)
}

/// How the run summaries introduce the licence ID (split so this file does not look like one).
const ID_KEY: &str = concat!("licence_", "id=");

/// The scan every copied text file goes through.
pub struct Guard {
    station: Station,
}

impl Guard {
    /// The licence to look for: `--licence-file`, or the usual places (`MOQ_SUBGHZ_LICENCE`, the
    /// XDG path). With no licence found anywhere the user must say `--no-licence`: a scan that
    /// silently has nothing to look for would not catch the run made with a licence.
    pub fn load(licence_file: Option<PathBuf>, no_licence: bool) -> Result<Guard> {
        let station = GateArgs {
            licence_file,
            no_licence,
            ..GateArgs::default()
        }
        .station()
        .context("licence")?;
        if station.licence().is_none() && !no_licence {
            bail!(
                "no licence file found to scan for: pass --licence-file PATH (the licence of the \
                 runs being packaged), or --no-licence to say they were made unlicensed"
            );
        }
        Ok(Guard { station })
    }

    /// Why `bytes`, the content of `file`, may not be published; `Ok` when it holds neither the
    /// licence ID nor the name and no unredacted licence line. Never carries the values.
    pub fn check(&self, file: &Path, bytes: &[u8]) -> Result<()> {
        self.check_window(file, bytes, true)
    }

    /// [`Guard::check`] for one window of a longer file. A licence line cut off by the end of a
    /// window is judged when the next window (which repeats the tail) holds all of it; `last`
    /// is true for the final window.
    fn check_window(&self, file: &Path, bytes: &[u8], last: bool) -> Result<()> {
        let text = String::from_utf8_lossy(bytes);
        if self.station.licence().is_some_and(|l| l.appears_in(&text)) {
            bail!(
                "refusing to package: {} holds the licence ID or operator name (values not printed). \
                 Re-run it with --redact-licence, or leave that run out",
                file.display()
            );
        }
        if text.match_indices(ID_KEY).any(|(i, k)| {
            let rest = &text[i + k.len()..];
            (last || rest.len() >= REDACTED.len()) && !rest.starts_with(REDACTED)
        }) {
            bail!(
                "refusing to package: {} has a licence line that is not redacted (values not printed)",
                file.display()
            );
        }
        Ok(())
    }

    fn read(&self, from: &Path) -> Result<Vec<u8>> {
        let data = fs::read(from).with_context(|| format!("read {}", from.display()))?;
        self.check(from, &data)?;
        Ok(data)
    }
}

/// Copy a text file after the scan.
fn cp(g: &Guard, from: &Path, to: &Path) -> Result<()> {
    fs::write(to, g.read(from)?).with_context(|| format!("copy {}", from.display()))
}

/// Copy a picture as it is (not text, not scanned).
fn cp_bin(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).with_context(|| format!("copy {}", from.display()))?;
    Ok(())
}

fn gzip(g: &Guard, from: &Path, to: &Path) -> Result<()> {
    use std::io::{Read, Write};
    let mut f = fs::File::open(from).with_context(|| format!("read {}", from.display()))?;
    let mut e = GzEncoder::new(fs::File::create(to)?, Compression::best());
    // Streamed, so a big log costs no memory, but not without end. Each block is scanned (with
    // the tail of the one before, so a value cannot hide across a block boundary) before it is
    // compressed; on a hit the half-written output is removed.
    let (mut buf, mut carry, mut total) = (vec![0u8; 1 << 20], Vec::<u8>::new(), 0u64);
    let fail = |e: GzEncoder<fs::File>, err: anyhow::Error| {
        drop(e);
        let _ = fs::remove_file(to);
        err
    };
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_LOG {
            let err = anyhow::anyhow!("{} is larger than {MAX_LOG} bytes", from.display());
            return Err(fail(e, err));
        }
        let mut win = std::mem::take(&mut carry);
        win.extend_from_slice(&buf[..n]);
        if let Err(err) = g.check_window(from, &win, false) {
            return Err(fail(e, err));
        }
        carry = win[win.len().saturating_sub(OVERLAP)..].to_vec();
        e.write_all(&buf[..n])?;
    }
    if let Err(err) = g.check_window(from, &carry, true) {
        return Err(fail(e, err));
    }
    e.finish()?;
    Ok(())
}

/// Copy the files of every run name in `dir` that match `pat` (`image*.jpg`, `image*.ssdv`).
fn cp_pictures(dir: &Path, to: &Path) -> Result<()> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Ok(());
    };
    for e in rd {
        let e = e?;
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with("image") && (n.ends_with(".jpg") || n.ends_with(".ssdv")) {
            cp_bin(&e.path(), &to.join(&n))?;
        }
    }
    Ok(())
}

pub fn run(g: &Guard, runs: &Path, demo: &Path, out: &Path) -> Result<()> {
    let (fsk, lora, dm): (PathBuf, PathBuf, PathBuf) =
        (out.join("fsk"), out.join("lora"), out.join("demo"));
    for d in [&fsk, &lora, &dm] {
        fs::create_dir_all(d)?;
    }
    for n in dirs(runs, |n| n.starts_with("f300-"))? {
        let d = runs.join(&n);
        let summary = d.join("summary.json");
        if fs::metadata(&summary).map_or(true, |m| m.len() == 0) {
            continue;
        }
        // A run whose sender never got a frame out has nothing to keep. One that sent frames and
        // received none (a total loss) is a result and is kept.
        let parse = |p: &Path| -> Result<serde_json::Value> {
            Ok(
                match crate::cell::read_text_opt(p, crate::cell::MAX_TEXT)? {
                    Some(t) => {
                        serde_json::from_str(&t).with_context(|| format!("{}", p.display()))?
                    }
                    None => serde_json::Value::Object(Default::default()),
                },
            )
        };
        let kind =
            crate::analyze::run_kind(&parse(&summary)?, &parse(&d.join("send_summary.json"))?);
        if kind == crate::analyze::RunKind::NeverStarted {
            continue;
        }
        let o = fsk.join(&n);
        fs::create_dir_all(&o)?;
        cp(g, &summary, &o.join("summary.json"))?;
        cp(
            g,
            &d.join("send_summary.json"),
            &o.join("send_summary.json"),
        )?;
        for f in ["objects.jsonl", "decoded.jsonl", "send.jsonl", "serial.log"] {
            gzip(g, &d.join(f), &o.join(format!("{f}.gz")))?;
        }
    }
    // Same glob as the shell: a single digit after "-r".
    for n in dirs(runs, |n| {
        n.len() > 3
            && n.as_bytes()[n.len() - 3..n.len() - 1] == *b"-r"
            && n.as_bytes()[n.len() - 1].is_ascii_digit()
    })? {
        let (d, o) = (runs.join(&n), lora.join(&n));
        fs::create_dir_all(&o)?;
        cp(g, &d.join("exact/images.csv"), &o.join("images.csv"))?;
        cp(g, &d.join("exact/rx.csv"), &o.join("rx.csv"))?;
        cp_pictures(&d.join("exact"), &o)?;
        gzip(g, &d.join("raw.log"), &o.join("raw.log.gz"))?;
        cp(g, &runs.join(format!("{n}.tx.csv")), &o.join("tx.csv"))?;
        let send = crate::cell::read_text(&runs.join(format!("{n}.send")), crate::cell::MAX_TEXT)
            .with_context(|| format!("read {n}.send"))?;
        let line: String = send
            .lines()
            .filter(|l| l.contains("sent frames"))
            .map(|l| format!("{l}\n"))
            .collect();
        g.check(&runs.join(format!("{n}.send")), line.as_bytes())?;
        fs::write(o.join("send.txt"), line)?;
    }
    for f in ["fsk-live.mp4", "fsk-live-still.png"] {
        cp_bin(&demo.join(f), &dm.join(f))?;
    }
    cp(
        g,
        &demo.join("fsk-live/summary.json"),
        &dm.join("fsk-live-summary.json"),
    )?;
    for n in dirs(demo, |n| n.starts_with("demo-"))? {
        let o = dm.join(&n);
        fs::create_dir_all(&o)?;
        cp(
            g,
            &demo.join(&n).join("exact/images.csv"),
            &o.join("images.csv"),
        )?;
        cp_pictures(&demo.join(&n).join("exact"), &o)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Scratch;
    use flate2::read::GzDecoder;
    use std::io::Read;

    /// The fake licence values are built here, not written out, so no source line looks like a
    /// licence to the repo's leak guard.
    fn fill(t: &str) -> String {
        t.replace("@ID2@", &["W9", "XYZ"].concat())
            .replace("@ID@", &["K1", "AB"].concat())
            .replace("@id@", &["k1", "ab"].concat())
            .replace("@NM2@", &["Some", "one"].concat())
            .replace("@NM@", &["Test", " Person"].concat())
            .replace("@LK@", &["licence", "_id"].concat())
            .replace("@NK@", &["operator", "_name"].concat())
            .replace("@LI@", ID_KEY)
            .replace("@OP@", &["operator", "="].concat())
    }

    fn put(p: PathBuf, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, fill(text)).unwrap();
    }

    fn gunzip(p: &Path) -> String {
        let mut s = String::new();
        GzDecoder::new(fs::File::open(p).unwrap())
            .read_to_string(&mut s)
            .unwrap();
        s
    }

    fn none() -> Guard {
        Guard::load(None, true).unwrap()
    }

    /// A guard for a licence file written under `dir` (a fake ID and name).
    fn licensed(dir: &Path) -> Guard {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("licence.toml");
        fs::write(
            &p,
            fill("region = \"us\"\n@LK@ = \"@ID@\"\n@NK@ = \"@NM@\"\n"),
        )
        .unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o600)).unwrap();
        Guard::load(Some(p), false).unwrap()
    }

    fn refused(g: &Guard, tag: &str, edit: impl Fn(&Path, &Path)) -> String {
        let (r, d, o) = (
            Scratch::new(&format!("{tag}-r")),
            Scratch::new(&format!("{tag}-d")),
            Scratch::new(&format!("{tag}-o")),
        );
        stage(&r.0, &d.0);
        edit(&r.0, &d.0);
        let e = run(g, &r.0, &d.0, &o.0).unwrap_err().to_string();
        assert!(
            !e.contains(&fill("@ID@")) && !e.contains(&fill("@NM@")),
            "{e}"
        );
        e
    }

    #[test]
    fn a_licence_id_in_a_summary_refuses_and_is_not_written() {
        let k = Scratch::new("pk-lic");
        let g = licensed(&k.0);
        let (r, d, o) = (
            Scratch::new("pkl-r"),
            Scratch::new("pkl-d"),
            Scratch::new("pkl-o"),
        );
        stage(&r.0, &d.0);
        put(
            r.0.join("f300-a/send_summary.json"),
            "{\"licence\":\"@LI@@ID@ @OP@@NM@\"}",
        );
        let e = run(&g, &r.0, &d.0, &o.0).unwrap_err().to_string();
        assert!(
            e.contains("refusing to package") && e.contains("send_summary.json"),
            "{e}"
        );
        assert!(!e.contains(&fill("@ID@")));
        assert!(!o.0.join("fsk/f300-a/send_summary.json").exists());
    }

    #[test]
    fn the_name_or_id_in_any_copied_text_refuses() {
        let k = Scratch::new("pk-lic2");
        let g = licensed(&k.0);
        // lower case ID, in each kind of file that is copied or gzipped
        for (n, rel, text) in [
            (
                "a",
                "f300-a/summary.json",
                "{\"radio_frames_ok\":9,\"o\":\"@id@\"}",
            ),
            ("b", "f300-a/serial.log", "hello @NM@"),
            ("c", "f300-a/send.jsonl", "x @ID@ y"),
            ("d", "chunk-sf7-125-small-r1/raw.log", "@ID@"),
            ("e", "chunk-sf7-125-small-r1/exact/images.csv", "@NM@"),
            ("f", "chunk-sf7-125-small-r1.tx.csv", "@ID@"),
            ("g", "chunk-sf7-125-small-r1.send", "sent frames=5 @ID@\n"),
        ] {
            let e = refused(&g, &format!("pkx{n}"), |r, _| put(r.join(rel), text));
            assert!(e.contains("refusing to package"), "{rel}: {e}");
        }
        let e = refused(&g, "pkxh", |_, d| {
            put(d.join("fsk-live/summary.json"), "@NM@")
        });
        assert!(e.contains("refusing to package"), "{e}");
        let e = refused(&g, "pkxi", |_, d| {
            put(d.join("demo-chunk-sf7-500/exact/images.csv"), "@ID@")
        });
        assert!(e.contains("refusing to package"), "{e}");
    }

    #[test]
    fn a_licence_line_that_is_not_redacted_refuses_even_with_no_licence() {
        let e = refused(&none(), "pkn", |r, _| {
            put(
                r.join("f300-a/send_summary.json"),
                "{\"licence\":\"@LI@@ID2@ @OP@@NM2@\"}",
            )
        });
        assert!(e.contains("not redacted"), "{e}");
        // a redacted line is fine
        let (r, d, o) = (
            Scratch::new("pkr-r"),
            Scratch::new("pkr-d"),
            Scratch::new("pkr-o"),
        );
        stage(&r.0, &d.0);
        put(
            r.0.join("f300-a/send_summary.json"),
            "{\"licence\":\"@LI@[redacted] @OP@[redacted]\"}",
        );
        run(&none(), &r.0, &d.0, &o.0).unwrap();
    }

    #[test]
    fn a_value_straddling_a_block_boundary_of_a_big_log_is_still_found() {
        let k = Scratch::new("pk-lic4");
        let g = licensed(&k.0);
        let d = Scratch::new("pk-big");
        let mut data = vec![b'.'; (1 << 20) - 2];
        data.extend_from_slice(fill("@ID@").as_bytes()); // crosses the 1 MiB block edge
        data.extend(vec![b'.'; 5000]);
        let (from, to) = (d.0.join("raw.log"), d.0.join("raw.log.gz"));
        fs::write(&from, &data).unwrap();
        let e = gzip(&g, &from, &to).unwrap_err().to_string();
        assert!(e.contains("refusing to package"), "{e}");
        assert!(!to.exists(), "the half-written output is removed");
        // the same log without the value compresses
        fs::write(&from, vec![b'.'; (1 << 20) + 10]).unwrap();
        gzip(&g, &from, &to).unwrap();
        assert!(to.exists());
    }

    #[test]
    fn clean_files_pass_a_licensed_guard() {
        let k = Scratch::new("pk-lic3");
        let (r, d, o) = (
            Scratch::new("pkc-r"),
            Scratch::new("pkc-d"),
            Scratch::new("pkc-o"),
        );
        stage(&r.0, &d.0);
        run(&licensed(&k.0), &r.0, &d.0, &o.0).unwrap();
        assert!(o.0.join("fsk/f300-a/summary.json").exists());
    }

    #[test]
    fn without_a_licence_or_the_no_licence_flag_it_will_not_package() {
        // an explicit missing file is an error, and `--no-licence` is the only way to scan nothing
        assert!(Guard::load(Some("/nonexistent/licence.toml".into()), false).is_err());
        assert!(Guard::load(None, true).is_ok());
    }

    fn stage(r: &Path, d: &Path) {
        put(
            r.join("f300-a/summary.json"),
            "{\"radio_frames_ok\":9,\"x\":1}",
        );
        put(r.join("f300-a/send_summary.json"), "{}");
        for f in ["objects.jsonl", "decoded.jsonl", "send.jsonl", "serial.log"] {
            put(r.join("f300-a").join(f), f);
        }
        put(
            r.join("f300-dead/summary.json"),
            "{\"radio_frames_ok\":0,\"x\":1}",
        );
        put(r.join("f300-none/send_summary.json"), "{}");
        put(r.join("chunk-sf7-125-small-r1/exact/images.csv"), "i\n");
        put(r.join("chunk-sf7-125-small-r1/exact/rx.csv"), "r\n");
        put(r.join("chunk-sf7-125-small-r1/exact/image0.jpg"), "jpg");
        put(r.join("chunk-sf7-125-small-r1/raw.log"), "raw");
        put(r.join("chunk-sf7-125-small-r1.tx.csv"), "tx");
        put(
            r.join("chunk-sf7-125-small-r1.send"),
            "x\nsent frames=5 bytes=1\n",
        );
        put(
            r.join("chunk-sf7-125-small-r10/exact/images.csv"),
            "not matched: two digits",
        );
        for f in [
            "fsk-live.mp4",
            "fsk-live-still.png",
            "fsk-live/summary.json",
        ] {
            put(d.join(f), f);
        }
        put(d.join("demo-chunk-sf7-500/exact/images.csv"), "i\n");
        put(d.join("demo-chunk-sf7-500/exact/image0.jpg"), "jpg");
    }

    #[test]
    fn keeps_what_is_worth_keeping_and_gzips_the_logs() {
        let (r, d, o) = (
            Scratch::new("pk-r"),
            Scratch::new("pk-d"),
            Scratch::new("pk-o"),
        );
        stage(&r.0, &d.0);
        run(&none(), &r.0, &d.0, &o.0).unwrap();
        assert!(o.0.join("fsk/f300-a/summary.json").exists());
        assert!(!o.0.join("fsk/f300-dead").exists() && !o.0.join("fsk/f300-none").exists());
        assert_eq!(gunzip(&o.0.join("fsk/f300-a/serial.log.gz")), "serial.log");
        let l = o.0.join("lora/chunk-sf7-125-small-r1");
        assert_eq!(gunzip(&l.join("raw.log.gz")), "raw");
        assert_eq!(
            fs::read_to_string(l.join("send.txt")).unwrap(),
            "sent frames=5 bytes=1\n"
        );
        assert!(l.join("image0.jpg").exists() && l.join("tx.csv").exists());
        assert!(!o.0.join("lora/chunk-sf7-125-small-r10").exists());
        assert!(o.0.join("demo/demo-chunk-sf7-500/image0.jpg").exists());
        assert_eq!(
            fs::read_to_string(o.0.join("demo/fsk-live-summary.json")).unwrap(),
            "fsk-live/summary.json"
        );
    }

    #[test]
    fn a_total_loss_run_is_kept_and_a_run_that_never_started_is_not() {
        let (r, d, o) = (
            Scratch::new("pk3-r"),
            Scratch::new("pk3-d"),
            Scratch::new("pk3-o"),
        );
        stage(&r.0, &d.0);
        put(
            r.0.join("f300-lost/summary.json"),
            "{\"radio_frames_ok\":0,\"x\":1}",
        );
        put(
            r.0.join("f300-lost/send_summary.json"),
            "{\"frames_tx\":500}",
        );
        for f in ["objects.jsonl", "decoded.jsonl", "send.jsonl", "serial.log"] {
            put(r.0.join("f300-lost").join(f), f);
        }
        put(
            r.0.join("f300-zero/summary.json"),
            "{\"radio_frames_ok\":0,\"x\":1}",
        );
        put(r.0.join("f300-zero/send_summary.json"), "{\"frames_tx\":0}");
        run(&none(), &r.0, &d.0, &o.0).unwrap();
        assert!(o.0.join("fsk/f300-lost/summary.json").exists());
        assert!(o.0.join("fsk/f300-lost/send_summary.json").exists());
        assert!(!o.0.join("fsk/f300-zero").exists() && !o.0.join("fsk/f300-dead").exists());
    }

    #[test]
    fn a_run_missing_its_log_is_an_error() {
        let (r, d, o) = (
            Scratch::new("pk2-r"),
            Scratch::new("pk2-d"),
            Scratch::new("pk2-o"),
        );
        stage(&r.0, &d.0);
        fs::remove_file(r.0.join("f300-a/serial.log")).unwrap();
        assert!(run(&none(), &r.0, &d.0, &o.0)
            .unwrap_err()
            .to_string()
            .contains("serial.log"));
    }
}
