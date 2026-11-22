//! `lora2015-table` and `lora2015-frames`, ports of `scripts/lora2015-table.sh` and
//! `scripts/lora2015-frames.sh`. Neither touches a radio.

use crate::exec::{Host, Plan};
use anyhow::{bail, ensure, Context, Result};
use std::path::Path;

fn field(line: &str, key: &str) -> Result<f64> {
    let i = line
        .find(key)
        .with_context(|| format!("no {key} in {line:?}"))?
        + key.len();
    let s: String = line[i..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    s.parse().with_context(|| format!("bad {key} in {line:?}"))
}

/// One row of the airtime table, from the sender's `--dry-run` summary line.
pub fn row(label: &str, mode: &str, profile: &str, dry_run_output: &str) -> Result<String> {
    let s = dry_run_output
        .lines()
        .find_map(|l| l.strip_prefix("summary "))
        .with_context(|| {
            format!("no summary line from the sender's --dry-run for {label} {mode} {profile}")
        })?;
    let (frames, bytes, toa) = (
        field(s, "frames=")?,
        field(s, "air_bytes=")?,
        field(s, "toa_s=")?,
    );
    ensure!(toa > 0.0, "toa_s is not positive in {s:?}");
    Ok(format!(
        "| {label} | {mode} | {profile} | {frames:.0} | {bytes:.0} | {toa:.2} | {:.3} | {} |",
        1.0 / toa,
        (30.0 / toa) as i64
    ))
}

/// The calculated airtime table of `results/lora-2015/README.md`.
pub fn table(h: &mut dyn Host, sender: &str, frames: &Path) -> Result<String> {
    let mut out = vec![
        "| image | mode | setting | frames | bytes on air | airtime per image (s) | best fps | images per 30 s |".to_string(),
        "|---|---|---|---|---|---|---|---|".to_string(),
    ];
    for p in ["sf7-125", "sf7-250", "sf7-500", "sf8-500"] {
        for (label, mode, file) in [
            ("320x240 q10 JPEG", "chunk", "f320x240-q10.jpg"),
            ("320x240 q10 JPEG", "lorawan", "f320x240-q10.jpg"),
            ("320x240 q10 SSDV", "ssdv", "f320x240-q10.ssdv"),
            ("160x120 q20 JPEG", "chunk", "f160x120-q20.jpg"),
            ("160x120 q20 JPEG", "lorawan", "f160x120-q20.jpg"),
            ("160x112 q20 SSDV", "ssdv", "f160x112-q20.ssdv"),
        ] {
            let plan = Plan::new(
                sender,
                [
                    "--mode",
                    mode,
                    "--file",
                    &frames.join(file).display().to_string(),
                    "--profile",
                    p,
                    "--dry-run",
                ],
            )
            .local();
            let d = h.run(&plan)?;
            ensure!(d.code == 0, "{} exited with {}", plan.shell(), d.code);
            out.push(row(label, mode, p, &d.stdout)?);
        }
    }
    Ok(out.join("\n"))
}

/// Rebuild the test pictures of `results/lora-2015/frames/` from the frame in `media/`.
pub fn make_frames(h: &mut dyn Host, root: &Path, have_ssdv: bool) -> Result<()> {
    let src = root
        .join("media/frame-f350-rind-120s.png")
        .display()
        .to_string();
    let out = root.join("results/lora-2015/frames");
    if !h.dry() {
        std::fs::create_dir_all(&out)?;
    }
    let o = |f: &str| out.join(f).display().to_string();
    // -force_duplicated_matrix writes the quantisation table twice, which ssdv needs.
    let enc = |h: &mut dyn Host, vf: &str, q: &str, to: String| -> Result<()> {
        let p = Plan::new(
            "ffmpeg",
            [
                "-v",
                "error",
                "-y",
                "-i",
                &src,
                "-vf",
                vf,
                "-pix_fmt",
                "yuvj420p",
                "-q:v",
                q,
                "-force_duplicated_matrix",
                "1",
                "-frames:v",
                "1",
                &to,
            ],
        );
        let d = h.run(&p)?;
        ensure!(d.code == 0, "{} exited with {}", p.shell(), d.code);
        Ok(())
    };
    enc(h, "scale=320:240", "10", o("f320x240-q10.jpg"))?;
    enc(h, "scale=160:120", "20", o("f160x120-q20.jpg"))?;
    // SSDV wants width and height in multiples of 16, so its small image is 160x112.
    enc(
        h,
        "scale=160:120,crop=160:112:0:0",
        "20",
        o("f160x112-q20.jpg"),
    )?;
    if !have_ssdv {
        eprintln!("ssdv not on PATH: skipped the .ssdv files");
        return Ok(());
    }
    for (id, jpg, ssdv) in [
        ("1", "f320x240-q10.jpg", "f320x240-q10.ssdv"),
        ("2", "f160x112-q20.jpg", "f160x112-q20.ssdv"),
    ] {
        let p = Plan::new(
            "ssdv",
            ["-e", "-n", "-c", "TEST01", "-i", id, &o(jpg), &o(ssdv)],
        )
        .err("/dev/null");
        let d = h.run(&p)?;
        if d.code != 0 {
            bail!("{} exited with {}", p.shell(), d.code);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orch::fake::Fake;

    #[test]
    fn a_row_is_the_awk_printf() {
        let r = row(
            "320x240 q10 JPEG",
            "chunk",
            "sf7-125",
            "summary mode=Chunk frames=35 air_bytes=8688 toa_s=13.638 pass_s=14.000\n",
        )
        .unwrap();
        assert_eq!(
            r,
            "| 320x240 q10 JPEG | chunk | sf7-125 | 35 | 8688 | 13.64 | 0.073 | 2 |"
        );
        assert!(row("a", "b", "c", "frames=1").is_err());
    }

    #[test]
    fn the_table_has_24_rows_and_stops_on_a_failed_sender() {
        let mut f = Fake::default();
        f.rule(
            "--dry-run",
            0,
            "summary mode=Chunk frames=35 air_bytes=8688 toa_s=13.638 pass_s=14.000\n",
            vec![],
        );
        let t = table(&mut f, "s", Path::new("fr")).unwrap();
        assert_eq!(t.lines().count(), 2 + 24);
        assert!(f.log[0]
            .starts_with("s --mode chunk --file fr/f320x240-q10.jpg --profile sf7-125 --dry-run"));
        f.rules[0].code = 2;
        assert!(table(&mut f, "s", Path::new("fr")).is_err());
    }

    #[test]
    fn frames_commands_and_the_missing_ssdv_tool() {
        let root = crate::testutil::Scratch::new("frames");
        let r = root.0.display();
        let mut f = Fake::default();
        make_frames(&mut f, &root.0, true).unwrap();
        assert_eq!(f.log.len(), 5);
        assert_eq!(f.log[0], format!("ffmpeg -v error -y -i {r}/media/frame-f350-rind-120s.png -vf scale=320:240 -pix_fmt yuvj420p -q:v 10 -force_duplicated_matrix 1 -frames:v 1 {r}/results/lora-2015/frames/f320x240-q10.jpg"));
        assert!(
            f.log[4].starts_with("ssdv -e -n -c TEST01 -i 2 ")
                && f.log[4].ends_with(" 2> /dev/null")
        );
        let mut g = Fake::default();
        make_frames(&mut g, &root.0, false).unwrap();
        assert_eq!(g.log.len(), 3);
        g.rule("ffmpeg", 1, "", vec![]);
        assert!(make_frames(&mut g, &root.0, false).is_err());
    }
}
