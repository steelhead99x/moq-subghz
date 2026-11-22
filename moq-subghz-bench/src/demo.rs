//! `demo-051`: the 6 minute guided demo of the RP2040-based device.
//! All over the air: a preflight of the stick and the Pi, 45 s of live FSK video, one small
//! picture three 2015 ways at SF7/500, and the chunked picture at SF7/125.

use crate::cell::Cell;
use crate::exec::{read, Host, Plan};
use crate::orch::{css, forward, Cfg, Css, Run};
use crate::serial::{self, Baud};
use anyhow::{bail, ensure, Context, Result};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// The stick's `I` banner from what it printed: the last `I rp2040-sx1276-fw` line, if it is the
/// expected radio (`sx_version=0x12`).
pub fn banner(text: &str) -> Result<String> {
    let line = text
        .lines()
        .rfind(|l| {
            l.strip_prefix("I ")
                .and_then(|r| r.split(' ').next())
                .is_some_and(|n| n.ends_with("-fw"))
        })
        .context("no banner: wrong port, or not rp2040-sx1276-fw")?;
    ensure!(
        line.contains("sx_version=0x12"),
        "{line}\nthe radio is not an SX1276 (sx_version=0x12)"
    );
    Ok(line.to_string())
}

/// Ask the stick for its banner on `port` (it sends nothing else, and does not transmit).
fn ask(port: &Path) -> Result<String> {
    let mut f = serial::open(port, Baud::B115200)?;
    f.write_all(b"I\n")?;
    let (end, mut out) = (Instant::now() + Duration::from_millis(1500), Vec::new());
    while Instant::now() < end {
        if serial::readable(&f, 100) {
            let mut b = [0u8; 4096];
            match f.read(&mut b) {
                Ok(n) => out.extend_from_slice(&b[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e).with_context(|| format!("read {}", port.display())),
            }
        }
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// The one line the demo prints about the live run.
pub fn live_line(summary: &str) -> Result<String> {
    let s: serde_json::Value = serde_json::from_str(summary).context("summary.json")?;
    let f = |k: &str| {
        s.get(k)
            .and_then(|v| v.as_f64())
            .with_context(|| format!("summary.json lacks {k}"))
    };
    let raw = |a: &str, b: &str| -> Result<String> {
        Cell::from_json(
            s.get(a)
                .and_then(|o| o.get(b))
                .with_context(|| format!("summary.json lacks {a}.{b}"))?,
        )
        .map(|c| c.text())
    };
    let video_loss = s
        .get("video_frame_loss")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let rssi = Cell::from_json(s.get("rssi_mean").context("summary.json lacks rssi_mean")?)?.text();
    Ok(format!(
        "radio frame loss {:.2}%  video frame loss {:.1}%  glass to glass p50/p95 {}/{} ms  video {:.0} kbit/s  RSSI {rssi} dBm",
        100.0 * f("radio_frame_loss")?,
        100.0 * video_loss,
        raw("g2g_lat_ms", "p50")?,
        raw("g2g_lat_ms", "p95")?,
        f("delivered_video_bps")? / 1000.0
    ))
}

/// Columns 2, 7, 10, 17 and 18 of the first data row of an `images.csv`.
pub fn image_line(csv: &str) -> Result<String> {
    let row = csv.lines().nth(1).context("images.csv has no data row")?;
    let f: Vec<&str> = row.split(',').collect();
    ensure!(f.len() >= 18, "images.csv row is short: {row}");
    Ok([f[1], f[6], f[9], f[16], f[17]].join(","))
}

fn ok(h: &mut dyn Host, p: Plan) -> Result<()> {
    let d = h.run(&p)?;
    ensure!(d.code == 0, "{} exited with {}", p.shell(), d.code);
    Ok(())
}

pub fn run(h: &mut dyn Host, c: &Cfg, out: &Path, play: bool) -> Result<()> {
    // The transmit gate comes first: nothing below may touch the stick or the Pi (the preflight
    // writes to the serial port) on a run that is going to be refused.
    c.gate(h, &[])?;
    let say = |s: &str| println!("\n== {s}");
    let c = Cfg {
        runs: out.to_path_buf(),
        ..c.clone()
    };
    if !h.dry() {
        std::fs::create_dir_all(out)?;
    }
    say("1. preflight");
    let port = c
        .port
        .clone()
        .context("--port (or PORT) is needed: no serial device is assumed")?;
    if h.dry() {
        println!("(would send `I` on {} at 115200 baud and expect an `I rp2040-sx1276-fw` banner with sx_version=0x12)", port.display());
    } else {
        println!("{}", banner(&ask(&port)?)?);
    }
    let remote = format!("test -x ~/{d}/moq-subghz-send && test -x ~/css2015/css2015-send && echo pi: senders present", d = crate::factory::shq(&c.pi_dir));
    ok(h, c.ssh(remote))?;
    say("2. live FSK video, 45 s, FEC k=8");
    let args = ["--fec-k".to_string(), "8".into()];
    forward(
        h,
        &c,
        &Run {
            name: "fsk-live",
            profile: "300000:100000:6",
            secs: 45,
            rx_key: None,
            rx: "",
            play,
            args: &args,
        },
    )?;
    if !h.dry() {
        println!("{}", live_line(&read(&out.join("fsk-live/summary.json"))?)?);
    }
    let (rec, mp4, still) = (
        out.join("fsk-live/rec.h264"),
        out.join("fsk-live.mp4"),
        out.join("fsk-live-still.png"),
    );
    let s = |p: &Path| p.display().to_string();
    ok(
        h,
        Plan::new(
            "ffmpeg",
            [
                "-v",
                "error",
                "-y",
                "-framerate",
                "15",
                "-i",
                &s(&rec),
                "-c",
                "copy",
                &s(&mp4),
            ],
        ),
    )?;
    ok(
        h,
        Plan::new(
            "ffmpeg",
            [
                "-v",
                "error",
                "-y",
                "-ss",
                "20",
                "-i",
                &s(&mp4),
                "-frames:v",
                "1",
                &s(&still),
            ],
        ),
    )?;
    say("3. one small picture, three 2015 ways, SF7/500, 2 passes");
    for mode in ["chunk", "ssdv", "framed"] {
        let file = if mode == "ssdv" {
            "f160x112-q20.ssdv"
        } else {
            "f160x120-q20.jpg"
        };
        let name = format!("demo-{mode}-sf7-500");
        css(
            h,
            &c,
            &Css {
                name: &name,
                mode,
                file,
                profile: "sf7-500",
                passes: 2,
                args: &[],
            },
        )?;
        if !h.dry() {
            println!(
                "{mode}: {}   (complete, seconds, loss %, RSSI, SNR)",
                image_line(&read(&out.join(&name).join("exact/images.csv"))?)?
            );
        }
    }
    say("4. chunked picture at SF7/125, 2 passes");
    css(
        h,
        &c,
        &Css {
            name: "demo-chunk-sf7-125",
            mode: "chunk",
            file: "f160x120-q20.jpg",
            profile: "sf7-125",
            passes: 2,
            args: &[],
        },
    )?;
    if !h.dry() {
        println!(
            "chunk SF7/125: {}",
            image_line(&read(&out.join("demo-chunk-sf7-125/exact/images.csv"))?)?
        );
    }
    say(&format!(
        "done: {} (fsk-live.mp4, fsk-live-still.png, demo-*/image*.jpg)",
        out.display()
    ));
    if h.dry() {
        return Ok(());
    }
    if !still.exists() {
        bail!("the demo left no still");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_has_to_be_the_sx1276_stick() {
        let ok = "noise\nI rp2040-sx1276-fw 0.5.1 sx_version=0x12 x\nstuff\n";
        assert_eq!(
            banner(ok).unwrap(),
            "I rp2040-sx1276-fw 0.5.1 sx_version=0x12 x"
        );
        assert!(banner("nothing")
            .unwrap_err()
            .to_string()
            .contains("no banner"));
        assert!(banner("I rp2040-sx1276-fw 0.5.1 sx_version=0x22").is_err());
    }

    fn cfg(antenna: bool, transmit: bool) -> Cfg {
        Cfg {
            pi: "pi".into(),
            pi_dir: "d".into(),
            bin: "bin".into(),
            runs: "/nonexistent/runs".into(),
            // opening this would fail with "open /nonexistent/...", not with a refusal
            port: Some("/nonexistent/ttyACM9".into()),
            frames: "f".into(),
            antenna,
            transmit,
            lic: Default::default(),
        }
    }

    #[test]
    fn a_refused_run_touches_neither_the_stick_nor_the_pi_nor_the_disk() {
        use crate::orch::fake::Fake;
        for (c, want) in [
            (cfg(true, false), "--transmit"),
            (cfg(false, true), "--i-have-an-antenna"),
            (cfg(false, false), "--i-have-an-antenna"),
        ] {
            let mut f = Fake::default();
            let out = std::path::Path::new("/nonexistent/out");
            let e = run(&mut f, &c, out, false).unwrap_err().to_string();
            // the gate's refusal, not "open /nonexistent/ttyACM9" (the preflight's write of `I`)
            // and not a failure to create the output directory
            assert!(
                e.contains("refusing to transmit") && e.contains(want),
                "{e}"
            );
            assert!(f.log.is_empty(), "ssh ran before the gate: {:?}", f.log);
        }
    }

    #[test]
    fn the_live_line() {
        let s = r#"{"radio_frame_loss":0.0123,"video_frame_loss":null,"g2g_lat_ms":{"p50":420,"p95":512.5},"delivered_video_bps":251000,"rssi_mean":-37.2}"#;
        assert_eq!(live_line(s).unwrap(), "radio frame loss 1.23%  video frame loss 0.0%  glass to glass p50/p95 420/512.5 ms  video 251 kbit/s  RSSI -37.2 dBm");
        assert!(live_line("{}").is_err());
    }

    #[test]
    fn the_image_line_picks_five_columns() {
        let csv = "image,complete,frames_expected,frames_unique,frames_good,repeats,seconds_to_image,effective_fps,loss_est_pct,loss_exact_pct,image_bytes,air_bytes_unique,image_bytes_per_frame,air_toa_s_calc,calc_fps_if_no_loss,effective_image_bps,rssi_mean_dbm,snr_mean_db,matches_reference\n0,1,35,35,105,70,13.674,0.0731,0.0,0.0,8618,8688,253,13.638,0.0733,5042,-24.9,9.87,1\n";
        assert_eq!(image_line(csv).unwrap(), "1,13.674,0.0,-24.9,9.87");
        assert!(image_line("a,b\n").is_err());
    }
}
