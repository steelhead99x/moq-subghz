//! `package-051`: copy the data worth keeping from a bench-051 runs directory into
//! `results/stick-0.5.1/`, with no logs that hold host paths. A run missing a file it should have is an error, not a gap.

use anyhow::{ensure, Context, Result};
use flate2::{write::GzEncoder, Compression};
use std::fs;

/// Largest log `package-051` compresses (1 GiB).
const MAX_LOG: u64 = 1 << 30;
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

fn cp(from: &Path, to: &Path) -> Result<()> {
    fs::copy(from, to).with_context(|| format!("copy {}", from.display()))?;
    Ok(())
}

fn gzip(from: &Path, to: &Path) -> Result<()> {
    use std::io::Read;
    let f = fs::File::open(from).with_context(|| format!("read {}", from.display()))?;
    let mut e = GzEncoder::new(fs::File::create(to)?, Compression::best());
    // Streamed, so a big log costs no memory, but not without end.
    let n = std::io::copy(&mut f.take(MAX_LOG + 1), &mut e)?;
    ensure!(
        n <= MAX_LOG,
        "{} is larger than {MAX_LOG} bytes",
        from.display()
    );
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
            cp(&e.path(), &to.join(&n))?;
        }
    }
    Ok(())
}

pub fn run(runs: &Path, demo: &Path, out: &Path) -> Result<()> {
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
        cp(&summary, &o.join("summary.json"))?;
        cp(&d.join("send_summary.json"), &o.join("send_summary.json"))?;
        for f in ["objects.jsonl", "decoded.jsonl", "send.jsonl", "serial.log"] {
            gzip(&d.join(f), &o.join(format!("{f}.gz")))?;
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
        cp(&d.join("exact/images.csv"), &o.join("images.csv"))?;
        cp(&d.join("exact/rx.csv"), &o.join("rx.csv"))?;
        cp_pictures(&d.join("exact"), &o)?;
        gzip(&d.join("raw.log"), &o.join("raw.log.gz"))?;
        cp(&runs.join(format!("{n}.tx.csv")), &o.join("tx.csv"))?;
        let send = crate::cell::read_text(&runs.join(format!("{n}.send")), crate::cell::MAX_TEXT)
            .with_context(|| format!("read {n}.send"))?;
        let line: String = send
            .lines()
            .filter(|l| l.contains("sent frames"))
            .map(|l| format!("{l}\n"))
            .collect();
        fs::write(o.join("send.txt"), line)?;
    }
    for f in ["fsk-live.mp4", "fsk-live-still.png"] {
        cp(&demo.join(f), &dm.join(f))?;
    }
    cp(
        &demo.join("fsk-live/summary.json"),
        &dm.join("fsk-live-summary.json"),
    )?;
    for n in dirs(demo, |n| n.starts_with("demo-"))? {
        let o = dm.join(&n);
        fs::create_dir_all(&o)?;
        cp(
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

    fn put(p: PathBuf, text: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn gunzip(p: &Path) -> String {
        let mut s = String::new();
        GzDecoder::new(fs::File::open(p).unwrap())
            .read_to_string(&mut s)
            .unwrap();
        s
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
        run(&r.0, &d.0, &o.0).unwrap();
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
        run(&r.0, &d.0, &o.0).unwrap();
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
        assert!(run(&r.0, &d.0, &o.0)
            .unwrap_err()
            .to_string()
            .contains("serial.log"));
    }
}
