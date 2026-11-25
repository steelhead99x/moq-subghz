//! moq-subghz-report DIR...  ->  markdown table on stdout (one row per run directory).
//!
//! Each DIR holds what a run left behind: `summary.json` (receiver), `send_summary.json`
//! (sender's last line), `decoded.jsonl[.gz]` and `objects.jsonl[.gz]`. Decoded-frame
//! stalls and glass-to-glass latency are recomputed here with the end-of-run decoder
//! flush removed (the CLI decoder holds one frame until EOF; that frame shows up ~9 s
//! late when the receiver closes it, which is not a stall).
//!
//! `--clock-log FILE` (lines `{"t_us": <laptop unix µs>, "c": {"remote_minus_local_ms": Pi-minus-laptop}}`,
//! written every few seconds by `moq-subghz-bench clocklog` during the runs) re-bases every latency
//! sample on the offset interpolated at that moment instead of the single offset measured
//! before the run. Needed because the board's timesyncd can step its clock by tens of ms mid-run.
use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

fn open_lines(dir: &Path, name: &str) -> Result<Vec<Value>> {
    let plain = dir.join(name);
    let gz = dir.join(format!("{name}.gz"));
    let r: Box<dyn Read> = if plain.exists() {
        Box::new(std::fs::File::open(&plain)?)
    } else {
        Box::new(flate2::read::GzDecoder::new(
            std::fs::File::open(&gz).with_context(|| format!("{}", gz.display()))?,
        ))
    };
    Ok(BufReader::new(r)
        .lines()
        .map_while(|l| l.ok())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect())
}

fn json(p: &Path) -> Result<Value> {
    Ok(serde_json::from_str(
        &std::fs::read_to_string(p).with_context(|| format!("{}", p.display()))?,
    )?)
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn stalls(t: &[u64], thr_ms: f64) -> (u32, f64) {
    let mut n = 0;
    let mut worst = 0f64;
    for w in t.windows(2) {
        let g = w[1].saturating_sub(w[0]) as f64 / 1000.0;
        worst = worst.max(g);
        if g > thr_ms {
            n += 1;
        }
    }
    (n, worst)
}

/// Laptop-time -> (Pi minus laptop) offset in ms, linear between samples.
struct ClockLog(Vec<(u64, f64)>);

impl ClockLog {
    fn load(p: &str) -> Result<Self> {
        let txt = std::fs::read_to_string(p)?;
        let mut v: Vec<(u64, f64, f64)> = txt
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter_map(|j| {
                Some((
                    j["t_us"].as_u64()?,
                    j["c"]["remote_minus_local_ms"].as_f64()?,
                    j["c"]["best_rtt_ms"].as_f64()?,
                ))
            })
            .collect();
        // drop samples whose best round trip was slow (asymmetric delay is likelier)
        let min_rtt = v.iter().map(|x| x.2).fold(f64::INFINITY, f64::min);
        v.retain(|x| x.2 <= min_rtt * 1.5 + 2.0);
        v.sort_by_key(|x| x.0);
        Ok(ClockLog(v.into_iter().map(|x| (x.0, x.1)).collect()))
    }
    fn at(&self, t_us: u64) -> Option<f64> {
        let v = &self.0;
        let i = v.partition_point(|x| x.0 <= t_us);
        if i == 0 || i == v.len() {
            return None; // outside the logged span: don't extrapolate
        }
        let (a, b) = (v[i - 1], v[i]);
        if b.0 == a.0 {
            return Some(a.1); // duplicate sample: the old code divided by zero
        }
        if b.0 < a.0 || b.0 - a.0 > 30_000_000 {
            return None; // out of order, or a gap in the log
        }
        Some(a.1 + (b.1 - a.1) * (t_us - a.0) as f64 / (b.0 - a.0) as f64)
    }
}

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let clock = match args.iter().position(|x| x == "--clock-log") {
        Some(i) => {
            if i + 1 >= args.len() {
                anyhow::bail!("--clock-log needs a path");
            }
            let p = args.remove(i + 1);
            args.remove(i);
            Some(ClockLog::load(&p)?)
        }
        None => None,
    };
    let dirs = args;
    println!("| run | dir | FSK | rind | video kbit/s | radio goodput kbit/s | radio frame loss | obj lost | video frame loss | obj lat p50/p95 ms | g2g p50/p95 ms | stalls >250 ms | Pi CPU % | bytes/obj: AEAD / total air overhead | seal / open µs |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    for d in dirs {
        let dir = Path::new(&d);
        let r = json(&dir.join("summary.json"))?;
        let s = json(&dir.join("send_summary.json"))?;
        let objs = open_lines(dir, "objects.jsonl")?;
        let dec = open_lines(dir, "decoded.jsonl")?;
        let last_obj = objs
            .iter()
            .filter_map(|o| o["t_us"].as_u64())
            .max()
            .unwrap_or(u64::MAX);
        let cutoff = last_obj.saturating_add(1_000_000);
        // The CPU guess is only for summaries without `link`, such as the published runs
        // in results/video; it labels a low-CPU forward run (the hardware encoder) reverse.
        let reverse = match s["link"].as_str() {
            Some(l) => l == "rp2040",
            None => {
                r["cpu_pct_recv"].is_number() && s["cpu_pct_total"].as_f64().unwrap_or(0.0) < 30.0
            }
        };
        let used = r["clock_offset_ms"].as_f64().unwrap_or(0.0);
        let clock_unverified = dir.join("clock_unverified").try_exists()?;
        // re-base one latency sample taken at receiver time t_us; None if the log doesn't cover it
        let rebase = |lat: f64, t_us: u64| -> Option<f64> {
            let Some(c) = &clock else { return Some(lat) };
            if reverse {
                // receiver = Pi; used = -(Pi-laptop) at start; true = -(Pi-laptop)(t)
                // laptop time = Pi time - (Pi - laptop) = t_us + used, in signed math:
                // `used` is positive when the board is behind.
                let laptop_us = i64::try_from(t_us)
                    .ok()?
                    .checked_add((used * 1000.0) as i64)?;
                let x = c.at(u64::try_from(laptop_us).ok()?)?;
                Some(lat - used - x)
            } else {
                Some(lat - used + c.at(t_us)?)
            }
        };
        let mut olat: Vec<f64> = objs
            .iter()
            .filter_map(|o| rebase(o["lat_ms"].as_f64()?, o["t_us"].as_u64()?))
            .collect();
        let covered = clock.is_none() || olat.len() * 10 >= objs.len() * 9;
        let mut dt: Vec<u64> = dec
            .iter()
            .filter_map(|x| x["t_us"].as_u64())
            .filter(|&t| t <= cutoff)
            .collect();
        dt.sort();
        let mut g2g: Vec<f64> = dec
            .iter()
            .filter(|x| x["t_us"].as_u64().unwrap_or(0) <= cutoff)
            .filter_map(|x| rebase(x["lat_ms"].as_f64()?, x["t_us"].as_u64()?))
            .collect();
        let (dst, dworst) = stalls(&dt, 250.0);
        let f = |v: &Value, k: &str| v[k].as_f64().unwrap_or(f64::NAN);
        let (dirn, cpu) = if reverse {
            (
                "Mac→Pi",
                format!(
                    "{:.0} (rx {:.0} + dec {:.0})",
                    f(&r, "cpu_pct_recv") + f(&r, "cpu_pct_children"),
                    f(&r, "cpu_pct_recv"),
                    f(&r, "cpu_pct_children")
                ),
            )
        } else {
            (
                "Pi→Mac",
                format!(
                    "{:.0} (tx {:.0} + enc {:.0})",
                    f(&s, "cpu_pct_total"),
                    f(&s, "cpu_pct_sender"),
                    f(&s, "cpu_pct_ffmpeg")
                ),
            )
        };
        let ov = &s["overhead_per_object"];
        let prof = r["profile"].as_str().unwrap_or("?");
        let br = prof
            .split(':')
            .next()
            .unwrap_or("?")
            .parse::<f64>()
            .unwrap_or(0.0)
            / 1000.0;
        let loss = if r["radio_frames_ok"].as_u64() == Some(0)
            && s["frames_tx"].as_u64().is_some_and(|n| n > 0)
        {
            100.0
        } else {
            100.0 * f(&r, "radio_frame_loss")
        };
        let (object_latency, glass_to_glass) = if clock_unverified {
            ("unverified".to_string(), "unverified".to_string())
        } else {
            (
                format!(
                    "{:.0} / {:.0}{}",
                    pct(&mut olat.clone(), 0.5),
                    pct(&mut olat, 0.95),
                    if covered { "" } else { " (clock log gap)" }
                ),
                format!(
                    "{:.0} / {:.0}",
                    pct(&mut g2g.clone(), 0.5),
                    pct(&mut g2g, 0.95)
                ),
            )
        };
        println!(
            "| {} | {dirn} | {br:.0}k{} | {} | {:.1} | {:.1} | {loss:.3}% | {} | {:.2}% | {object_latency} | {glass_to_glass} | {} obj, {} dec (worst {:.0} ms) | {cpu} | {} / {:.0} | {:.1} / {:.1} |",
            dir.file_name().unwrap_or_default().to_string_lossy(),
            if prof.ends_with(":x") { " (x)" } else { "" },
            s["cipher"].as_str().unwrap_or("none"),
            f(&r, "delivered_video_bps") / 1000.0,
            f(&r, "radio_goodput_bps") / 1000.0,
            r["objects_lost"],
            100.0 * f(&r, "video_frame_loss"),
            r["stalls_obj"], dst, dworst.max(f(&r, "worst_gap_obj_ms")),
            ov["aead"], f(ov, "total_air"),
            f(&s, "seal_us_per_object"), f(&r, "open_us_per_object"),
        );
    }
    Ok(())
}
