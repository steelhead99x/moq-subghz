//! `analyze`: turn the raw runs of the stick 0.5.1 bench into the tables and charts of
//! `results/stick-0.5.1/`. The committed files were written by a Python
//! analyzer, and this reproduces them byte for byte. Everything computed from files is "measured"; columns ending `_calc` are the
//! datasheet formula.

use crate::cell::{field, mean, num, read_dict, rng, round_to, write_csv, Cell};
use anyhow::{Context, Result};
use base64::Engine;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the analysis reads its reference data from.
pub struct Inputs {
    pub frames: PathBuf,
    pub factory_csv: PathBuf,
    pub factory_data: PathBuf,
}

impl Inputs {
    /// The layout of this repository under `root`.
    pub fn under(root: &Path) -> Inputs {
        let r = root.join("results");
        Inputs {
            frames: r.join("lora-2015/frames"),
            factory_csv: r.join("factory-firmware/results.csv"),
            factory_data: r.join("factory-firmware/data"),
        }
    }
}

/// Peak signal to noise ratio of a rebuilt picture against its reference, in dB, or `None` when
/// the pictures match exactly or ffmpeg printed no average.
pub type Psnr<'a> = &'a dyn Fn(&Path, &Path, Option<&str>) -> Result<Option<f64>>;

/// The PSNR from ffmpeg's psnr filter. Fails when ffmpeg cannot run.
pub fn ffmpeg_psnr(rebuilt: &Path, reference: &Path, crop: Option<&str>) -> Result<Option<f64>> {
    let flt = match crop {
        None => "[0:v][1:v]psnr".to_string(),
        Some(c) => format!("[1:v]crop={c}[r];[0:v][r]psnr"),
    };
    let out = Command::new("ffmpeg")
        .args(["-v", "info", "-i"])
        .arg(rebuilt)
        .arg("-i")
        .arg(reference)
        .args(["-lavfi", &flt, "-f", "null", "-"])
        .output()
        .context("run ffmpeg (needed for the SSDV PSNR column)")?;
    let err = String::from_utf8_lossy(&out.stderr);
    let Some(i) = err.find("average:") else {
        return Ok(None);
    };
    let tok = err[i + 8..].split_whitespace().next().unwrap_or("");
    if tok == "inf" {
        return Ok(None);
    }
    Ok(tok.parse::<f64>().ok().map(|v| round_to(v, 1)))
}

fn get<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&Value::Null)
}

fn req<'a>(v: &'a Value, k: &str, run: &str) -> Result<&'a Value> {
    v.get(k)
        .with_context(|| format!("run {run}: summary lacks {k}"))
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn as_f(v: &Value, what: &str) -> Result<f64> {
    v.as_f64()
        .with_context(|| format!("{what} is not a number: {v}"))
}

/// `round(k * v, nd)`: an integer stays an integer, as in Python.
fn scaled_round(v: &Value, k: i64, nd: usize, what: &str) -> Result<Cell> {
    if let Some(i) = v.as_i64() {
        return Ok(Cell::Int(k * i));
    }
    Ok(Cell::Float(round_to(k as f64 * as_f(v, what)?, nd)))
}

fn div_round(v: &Value, d: f64, nd: usize, what: &str) -> Result<Cell> {
    Ok(Cell::Float(round_to(as_f(v, what)? / d, nd)))
}

fn names(dir: &Path, keep: impl Fn(&str, &Path) -> bool) -> Result<Vec<String>> {
    let mut v = Vec::new();
    for e in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let e = e?;
        let n = e.file_name().to_string_lossy().into_owned();
        if !n.starts_with('.') && keep(&n, &e.path()) {
            v.push(n);
        }
    }
    v.sort();
    Ok(v)
}

const FSK_COLS: [&str; 28] = [
    "run",
    "fec_k",
    "power_dbm",
    "cipher",
    "span_s",
    "sender_radio_busy_pct",
    "sender_air_kbit_s",
    "radio_frames_ok",
    "radio_frames_missing",
    "radio_frames_bad_crc",
    "radio_frame_loss_pct",
    "fec_recovered",
    "objects_lost",
    "groups_cut",
    "video_frame_loss_pct",
    "delivered_video_kbit_s",
    "radio_goodput_kbit_s",
    "obj_lat_p50_ms",
    "obj_lat_p95_ms",
    "g2g_p50_ms",
    "g2g_p95_ms",
    "g2g_max_ms",
    "stalls_over_250ms",
    "worst_gap_ms",
    "decoded_frames",
    "rssi_mean_dbm",
    "clock_offset_ms",
    "auth_fail",
];

/// The rows of `fsk.csv`: one per `f300-*` run that put radio frames on the air.
pub fn fsk_rows(runs: &Path) -> Result<Vec<Vec<Cell>>> {
    let mut rows = Vec::new();
    for n in names(runs, |n, p| n.starts_with("f300-") && p.is_dir())? {
        let dir = runs.join(&n);
        let s: Value = match fs::read_to_string(dir.join("summary.json")) {
            Ok(t) => serde_json::from_str(&t).with_context(|| format!("{n}/summary.json"))?,
            // No summary: the receiver never wrote one, the run did not happen.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("{n}/summary.json")),
        };
        if !truthy(get(&s, "radio_frames_ok")) {
            continue; // the run did not happen (sender failed)
        }
        let t: Value = match fs::read_to_string(dir.join("send_summary.json")) {
            Ok(t) => serde_json::from_str(&t).with_context(|| format!("{n}/send_summary.json"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Object(Default::default()),
            Err(e) => return Err(e).with_context(|| format!("{n}/send_summary.json")),
        };
        let r = |k| req(&s, k, &n);
        let c = |v: &Value| Cell::from_json(v);
        let lat = |obj: &str, k: &str| -> Result<Cell> { c(req(req(&s, obj, &n)?, k, &n)?) };
        let vloss = r("video_frame_loss")?;
        let air = get(&t, "air_bps");
        rows.push(vec![
            Cell::Str(n.clone()),
            c(get(&t, "fec_k"))?,
            c(get(&t, "power_dbm"))?,
            c(get(&s, "cipher"))?,
            c(r("span_s")?)?,
            c(get(&t, "radio_busy_pct"))?,
            if truthy(air) {
                div_round(air, 1000.0, 1, "air_bps")?
            } else {
                Cell::None
            },
            c(r("radio_frames_ok")?)?,
            c(r("radio_frames_missing")?)?,
            c(r("radio_frames_bad")?)?,
            scaled_round(r("radio_frame_loss")?, 100, 3, "radio_frame_loss")?,
            c(r("fec_recovered")?)?,
            c(r("objects_lost")?)?,
            c(r("groups_cut")?)?,
            if vloss.is_null() {
                Cell::None
            } else {
                scaled_round(vloss, 100, 2, "video_frame_loss")?
            },
            div_round(r("delivered_video_bps")?, 1000.0, 1, "delivered_video_bps")?,
            div_round(r("radio_goodput_bps")?, 1000.0, 1, "radio_goodput_bps")?,
            lat("obj_lat_ms", "p50")?,
            lat("obj_lat_ms", "p95")?,
            lat("g2g_lat_ms", "p50")?,
            lat("g2g_lat_ms", "p95")?,
            lat("g2g_lat_ms", "max")?,
            c(r("stalls_decoded")?)?,
            c(r("worst_gap_decoded_ms")?)?,
            c(r("decoded_frames")?)?,
            c(r("rssi_mean")?)?,
            c(r("clock_offset_ms")?)?,
            c(get(&s, "auth_fail"))?,
        ]);
    }
    Ok(rows)
}

/// Split received chunk indexes into passes: a pass always sends 0, 1, 2 ... so an index that
/// does not go up starts the next one.
pub fn passes(indexes: &[i64]) -> Vec<Vec<i64>> {
    let mut out: Vec<Vec<i64>> = Vec::new();
    let mut cur: Vec<i64> = Vec::new();
    for &i in indexes {
        if cur.last().is_some_and(|&l| i <= l) {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(i);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// One measured lora2015 run, with every column of `lora.csv`.
pub struct LoraRun {
    pub mode: String,
    pub profile: String,
    pub size: String,
    pub frames_expected: i64,
    pub loss_exact_pct: Option<f64>,
    pub pass1_loss_pct: f64,
    pub complete: i64,
    pub seconds_to_image: Option<f64>,
    pub goodput_bit_s: Option<i64>,
    pub airtime_calc: Option<f64>,
    pub rssi_mean: Option<f64>,
    pub snr_mean: Option<f64>,
    pub psnr_db: Option<f64>,
    pub cells: Vec<Cell>,
}

/// `chunk-sf7-125-small-r2` into (mode, profile, size, rep).
fn parse_run_name(n: &str) -> Option<(String, String, String, i64)> {
    let (mode, rest) = n.split_once('-')?;
    if !["chunk", "ssdv", "lorawan"].contains(&mode) {
        return None;
    }
    let (profile, rest) = {
        let mut it = rest.splitn(3, '-');
        let (a, b) = (it.next()?, it.next()?);
        (format!("{a}-{b}"), it.next()?)
    };
    let (sf, bw) = profile.split_once('-')?;
    let digit_sf = sf
        .strip_prefix("sf")
        .filter(|d| d.len() == 1 && d.bytes().all(|b| b.is_ascii_digit()));
    if digit_sf.is_none() || bw.is_empty() || !bw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (size, rep) = rest.split_once('-')?;
    if !["small", "large"].contains(&size) {
        return None;
    }
    let rep = rep.strip_prefix('r')?;
    if rep.is_empty() || !rep.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((mode.into(), profile, size.into(), rep.parse().ok()?))
}

/// The digits after the first `key` that has any.
fn after<'a>(text: &'a str, key: &str, chars: impl Fn(char) -> bool) -> Option<&'a str> {
    text.match_indices(key).find_map(|(i, _)| {
        let rest = &text[i + key.len()..];
        let end = rest.find(|c| !chars(c)).unwrap_or(rest.len());
        (end > 0).then(|| &rest[..end])
    })
}

fn int(s: &str, what: &str, run: &str) -> Result<i64> {
    s.trim()
        .parse()
        .with_context(|| format!("run {run}: {what} is not an integer: {s:?}"))
}

fn lora_run(runs: &Path, n: &str, inp: &Inputs, psnr: Psnr) -> Result<Option<LoraRun>> {
    let Some((mode, profile, size, rep)) = parse_run_name(n) else {
        return Ok(None);
    };
    let d = runs.join(n);
    let images = d.join("exact/images.csv");
    if !images.exists() {
        return Ok(None);
    }
    let im = read_dict(&images)?;
    let Some(im) = im.first() else {
        return Ok(None);
    };
    let rxp = d.join("exact/rx.csv");
    let all = read_dict(&rxp)?;
    let (rx, bad): (Vec<_>, Vec<_>) = all
        .iter()
        .partition(|r| matches!(r.get("status").map(String::as_str), Some("new" | "dup")));
    let sendp = runs.join(format!("{n}.send"));
    let send = fs::read_to_string(&sendp).with_context(|| format!("read {}", sendp.display()))?;
    let sent = int(
        after(&send, "sent frames=", |c| c.is_ascii_digit())
            .with_context(|| format!("{}: no `sent frames=`", sendp.display()))?,
        "sent frames",
        n,
    )?;
    let wall: f64 = after(&send, "wall_s=", |c| c.is_ascii_digit() || c == '.')
        .with_context(|| format!("{}: no `wall_s=`", sendp.display()))?
        .parse()
        .with_context(|| format!("{}: bad wall_s", sendp.display()))?;
    let expect = int(field(im, "frames_expected", &images)?, "frames_expected", n)?;
    let mut idx = Vec::new();
    for r in &rx {
        num(field(r, "host_s", &rxp)?)
            .with_context(|| format!("{}: host_s is not a number", rxp.display()))?;
        idx.push(int(field(r, "index", &rxp)?, "index", n)?);
    }
    let ps = passes(&idx);
    let p1 = ps
        .first()
        .map_or(0, |p| p.iter().collect::<BTreeSet<_>>().len() as i64);
    let mut seen = BTreeSet::new();
    let mut done_at = None;
    for (k, p) in ps.iter().enumerate() {
        seen.extend(p.iter().copied());
        if seen.len() as i64 >= expect && done_at.is_none() {
            done_at = Some(k as i64 + 1);
        }
    }
    let col = |key: &str| -> Result<Vec<Option<f64>>> {
        rx.iter().map(|r| Ok(num(field(r, key, &rxp)?))).collect()
    };
    let rssi = col("rssi_dbm")?;
    let snr = col("snr_db")?;
    let jpgs = names(&d.join("exact"), |n, _| {
        n.starts_with("image") && n.ends_with(".jpg")
    })?;
    let jpg = jpgs.first().map(|j| d.join("exact").join(j));
    let jpg_bytes = match &jpg {
        Some(p) => Some(fs::metadata(p)?.len() as f64),
        None => None,
    };
    let secs = num(field(im, "seconds_to_image", &images)?);
    let ps_db = match (&jpg, mode.as_str()) {
        (Some(j), "ssdv") if size == "large" => {
            psnr(j, &inp.frames.join("f320x240-q10.jpg"), None)?
        }
        (Some(j), "ssdv") => psnr(j, &inp.frames.join("f160x120-q20.jpg"), Some("160:112:0:0"))?,
        _ => None,
    };
    let goodput = match (jpg_bytes, secs) {
        (Some(b), Some(s)) if b != 0.0 && s != 0.0 => Some((8.0 * b / s).round_ties_even() as i64),
        _ => None,
    };
    let air = num(field(im, "air_toa_s_calc", &images)?);
    let best = match (jpg_bytes, air) {
        (Some(b), Some(a)) if b != 0.0 && a != 0.0 => Some((8.0 * b / a).round_ties_even() as i64),
        _ => None,
    };
    let loss_exact = num(field(im, "loss_exact_pct", &images)?);
    let pass1_loss = round_to(100.0 * (expect - p1) as f64 / expect as f64, 1);
    let complete = int(field(im, "complete", &images)?, "complete", n)?;
    let (rssi_lo, rssi_hi) = rng(&rssi);
    let (snr_lo, snr_hi) = rng(&snr);
    let opt_i = |v: Option<i64>| v.map_or(Cell::None, Cell::Int);
    let cells = vec![
        Cell::Str(n.into()),
        Cell::Str(mode.clone()),
        Cell::Str(profile.clone()),
        Cell::Str(size.clone()),
        Cell::Int(rep),
        Cell::Int(sent),
        Cell::Int(expect),
        Cell::Int(rx.len() as i64),
        Cell::Int(bad.len() as i64),
        loss_exact.into(),
        Cell::Int(p1),
        Cell::Float(pass1_loss),
        opt_i(done_at),
        Cell::Int(complete),
        if mode != "ssdv" {
            Cell::Str(field(im, "matches_reference", &images)?.into())
        } else {
            Cell::Str(String::new())
        },
        ps_db.into(),
        jpg_bytes.map_or(Cell::None, |b| Cell::Int(b as i64)),
        secs.into(),
        opt_i(goodput),
        air.into(),
        opt_i(best),
        Cell::Float(wall),
        mean(&rssi).into(),
        rssi_lo.into(),
        rssi_hi.into(),
        mean(&snr).into(),
        snr_lo.into(),
        snr_hi.into(),
    ];
    Ok(Some(LoraRun {
        mode,
        profile,
        size,
        frames_expected: expect,
        loss_exact_pct: loss_exact,
        pass1_loss_pct: pass1_loss,
        complete,
        seconds_to_image: secs,
        goodput_bit_s: goodput,
        airtime_calc: air,
        rssi_mean: mean(&rssi),
        snr_mean: mean(&snr),
        psnr_db: ps_db,
        cells,
    }))
}

const LORA_HEADER: [&str; 28] = [
    "run",
    "mode",
    "profile",
    "size",
    "rep",
    "frames_sent",
    "frames_expected",
    "frames_heard",
    "frames_bad",
    "loss_exact_pct",
    "pass1_heard",
    "pass1_loss_pct",
    "passes_to_complete",
    "complete",
    "byte_identical",
    "ssdv_psnr_db",
    "picture_bytes",
    "seconds_to_image",
    "goodput_bit_s",
    "airtime_per_picture_s_calc",
    "best_case_goodput_bit_s_calc",
    "sender_wall_s",
    "rssi_mean_dbm",
    "rssi_min_dbm",
    "rssi_max_dbm",
    "snr_mean_db",
    "snr_min_db",
    "snr_max_db",
];

/// Every lora2015 run in `runs` (`<mode>-sfN-<bw>-<size>-rN` with an `exact/` replay).
pub fn lora_runs(runs: &Path, inp: &Inputs, psnr: Psnr) -> Result<Vec<LoraRun>> {
    let mut out = Vec::new();
    // Same glob as the Python: a single digit after "-r".
    for n in names(runs, |n, _| {
        n.len() > 2
            && n[n.len() - 3..].starts_with("-r")
            && n.as_bytes()[n.len() - 1].is_ascii_digit()
    })? {
        out.extend(lora_run(runs, &n, inp, psnr)?);
    }
    Ok(out)
}

const SUMMARY_HEADER: [&str; 16] = [
    "size",
    "profile",
    "mode",
    "runs",
    "complete",
    "frames_per_picture",
    "loss_exact_pct_mean",
    "pass1_loss_pct_mean",
    "seconds_to_image_mean",
    "seconds_to_image_min",
    "seconds_to_image_max",
    "goodput_bit_s_mean",
    "airtime_per_picture_s_calc",
    "rssi_mean_dbm",
    "snr_mean_db",
    "ssdv_psnr_db",
];

/// The runs of one (size, profile, mode) cell of the matrix.
pub struct Group<'a> {
    pub size: &'a str,
    pub profile: &'a str,
    pub mode: &'a str,
    pub runs: Vec<&'a LoraRun>,
}

fn profile_order(p: &str) -> Result<usize> {
    ["sf7-125", "sf7-250", "sf7-500", "sf8-500"]
        .iter()
        .position(|x| *x == p)
        .with_context(|| format!("unknown profile {p}"))
}

/// Group runs by (size, profile, mode), in the order of `lora-summary.csv`.
pub fn groups(l: &[LoraRun]) -> Result<Vec<Group<'_>>> {
    let mut g: Vec<Group> = Vec::new();
    for r in l {
        match g
            .iter_mut()
            .find(|x| x.size == r.size && x.profile == r.profile && x.mode == r.mode)
        {
            Some(x) => x.runs.push(r),
            None => g.push(Group {
                size: &r.size,
                profile: &r.profile,
                mode: &r.mode,
                runs: vec![r],
            }),
        }
    }
    let mut keyed = Vec::new();
    for x in g {
        keyed.push((
            (
                x.size != "small",
                profile_order(x.profile)?,
                x.mode.to_string(),
            ),
            x,
        ));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(keyed.into_iter().map(|(_, x)| x).collect())
}

fn summary_row(g: &Group) -> Vec<Cell> {
    let v = |f: &dyn Fn(&LoraRun) -> Option<f64>| g.runs.iter().map(|r| f(r)).collect::<Vec<_>>();
    let secs = v(&|r| r.seconds_to_image);
    vec![
        Cell::Str(g.size.into()),
        Cell::Str(g.profile.into()),
        Cell::Str(g.mode.into()),
        Cell::Int(g.runs.len() as i64),
        Cell::Int(g.runs.iter().map(|r| r.complete).sum()),
        Cell::Int(g.runs[0].frames_expected),
        mean(&v(&|r| r.loss_exact_pct)).into(),
        mean(&v(&|r| Some(r.pass1_loss_pct))).into(),
        mean(&secs).into(),
        rng(&secs).0.into(),
        rng(&secs).1.into(),
        mean(&v(&|r| r.goodput_bit_s.map(|x| x as f64))).into(),
        g.runs[0].airtime_calc.into(),
        mean(&v(&|r| r.rssi_mean)).into(),
        mean(&v(&|r| r.snr_mean)).into(),
        mean(&v(&|r| r.psnr_db)).into(),
    ]
}

/// First packet heard to the packet that completes the picture, plus one packet of airtime,
/// from the factory run's stick capture. `None` when the picture never completed.
pub fn factory_window(data: &Path, run: &str, airtime_s: f64) -> Result<Option<f64>> {
    let path = data.join(run).join("stick.packets.csv");
    let mut held = BTreeSet::new();
    let (mut first, mut key): (Option<f64>, Option<(u8, i64)>) = (None, None);
    for r in read_dict(&path)? {
        let b64 = field(&r, "payload_b64", &path)?;
        let pl = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .with_context(|| format!("{}: bad base64", path.display()))?;
        let Some(p) = crate::factory::parse_chunk(&pl, None) else {
            continue;
        };
        let k = (p.id, p.total);
        let key = *key.get_or_insert(k);
        if k != key {
            continue;
        }
        let t: f64 = num(field(&r, "t_mono_s", &path)?)
            .with_context(|| format!("{}: bad t_mono_s", path.display()))?;
        let first = *first.get_or_insert(t);
        held.insert(p.seq);
        if held.len() as i64 == key.1 {
            return Ok(Some(round_to(t - first + airtime_s, 2)));
        }
    }
    Ok(None)
}

const COMPARE_HEADER: [&str; 13] = [
    "picture",
    "stack",
    "runs",
    "all_complete",
    "pass1_loss_pct_mean",
    "packets_per_picture",
    "seconds_to_picture_mean",
    "seconds_min",
    "seconds_max",
    "goodput_bit_s_mean",
    "rssi_mean_dbm",
    "snr_mean_db",
    "factory_end_to_end_s_mean",
];

/// A row of `compare.csv`, plus the label and value its chart bar needs.
pub struct CompareRow {
    pub picture: &'static str,
    pub stack: String,
    pub seconds_mean: Option<f64>,
    pub cells: Vec<Cell>,
}

pub fn compare_rows(l: &[LoraRun], inp: &Inputs) -> Result<Vec<CompareRow>> {
    let fac = read_dict(&inp.factory_csv)?;
    let mut win = Vec::new();
    for r in &fac {
        let run = field(r, "run", &inp.factory_csv)?;
        let at = num(field(r, "calc_airtime_per_packet_ms", &inp.factory_csv)?)
            .context("calc_airtime_per_packet_ms")?
            / 1000.0;
        win.push(factory_window(&inp.factory_data, run, at)?);
    }
    let gs = groups(l)?;
    let mut out = Vec::new();
    for (size, tag) in [("small", "small-160x120"), ("large", "large-320x240")] {
        let idx: Vec<usize> = (0..fac.len())
            .filter(|&i| fac[i]["run"].starts_with(tag))
            .collect();
        if let Some(&f0) = idx.first() {
            let c = |k: &str| -> Result<Vec<Option<f64>>> {
                idx.iter()
                    .map(|&i| Ok(num(field(&fac[i], k, &inp.factory_csv)?)))
                    .collect()
            };
            let w: Vec<Option<f64>> = idx.iter().map(|&i| win[i]).collect();
            let mut goodput = Vec::new();
            for (&i, w) in idx.iter().zip(&w) {
                if let Some(w) = w.filter(|w| *w != 0.0) {
                    goodput.push(
                        num(field(&fac[i], "jpeg_bytes", &inp.factory_csv)?).map(|b| 8.0 * b / w),
                    );
                }
            }
            let (lo, hi) = (c("rssi_min_dbm")?, c("rssi_max_dbm")?);
            let mut rssi = Vec::new();
            for (a, b) in lo.iter().zip(&hi) {
                rssi.push(Some(
                    (a.context("rssi_min_dbm is not a number")?
                        + b.context("rssi_max_dbm is not a number")?)
                        / 2.0,
                ));
            }
            let all = idx
                .iter()
                .all(|&i| fac[i].get("byte_identical").map(String::as_str) == Some("True"));
            out.push(CompareRow {
                picture: size,
                stack: "factory (Dragino app + stock stick)".into(),
                seconds_mean: mean(&w),
                cells: vec![
                    Cell::Str(size.into()),
                    Cell::Str("factory (Dragino app + stock stick)".into()),
                    Cell::Int(idx.len() as i64),
                    Cell::Bool(all),
                    mean(&c("pass1_loss_pct")?).into(),
                    Cell::Str(field(&fac[f0], "chunks", &inp.factory_csv)?.into()),
                    mean(&w).into(),
                    rng(&w).0.into(),
                    rng(&w).1.into(),
                    mean(&goodput).into(),
                    mean(&rssi).into(),
                    Cell::Str(String::new()),
                    mean(&c("end_to_end_s")?).into(),
                ],
            });
        }
        for mode in ["chunk", "lorawan", "ssdv"] {
            let Some(g) = gs
                .iter()
                .find(|g| g.size == size && g.profile == "sf7-125" && g.mode == mode)
            else {
                continue;
            };
            let v = |f: &dyn Fn(&LoraRun) -> Option<f64>| {
                g.runs.iter().map(|r| f(r)).collect::<Vec<_>>()
            };
            let secs = v(&|r| r.seconds_to_image);
            out.push(CompareRow {
                picture: size,
                stack: format!("moq-subghz 0.5.1 {mode}"),
                seconds_mean: mean(&secs),
                cells: vec![
                    Cell::Str(size.into()),
                    Cell::Str(format!("moq-subghz 0.5.1 {mode}")),
                    Cell::Int(g.runs.len() as i64),
                    Cell::Bool(g.runs.iter().all(|r| r.complete != 0)),
                    mean(&v(&|r| Some(r.pass1_loss_pct))).into(),
                    Cell::Int(g.runs[0].frames_expected),
                    mean(&secs).into(),
                    rng(&secs).0.into(),
                    rng(&secs).1.into(),
                    mean(&v(&|r| r.goodput_bit_s.map(|x| x as f64))).into(),
                    mean(&v(&|r| r.rssi_mean)).into(),
                    mean(&v(&|r| r.snr_mean)).into(),
                    Cell::Str(String::new()),
                ],
            });
        }
    }
    Ok(out)
}

/// A horizontal bar chart as plain SVG, the layout of the committed charts.
pub fn bars(title: &str, items: &[(String, f64, &str)], unit: &str) -> String {
    use crate::cell::fmt_g;
    let (w_total, h, left) = (760.0_f64, 40 + 30 * items.len(), 330.0_f64);
    let mx = items.iter().map(|i| i.1).fold(f64::NEG_INFINITY, f64::max);
    let mx = if mx == 0.0 { 1.0 } else { mx };
    let mut o = vec![
        format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"760\" height=\"{h}\" font-family=\"sans-serif\" font-size=\"13\">"),
        format!("<rect width=\"760\" height=\"{h}\" fill=\"white\"/>"),
        format!("<text x=\"10\" y=\"22\" font-size=\"15\" font-weight=\"bold\">{title}</text>"),
    ];
    for (k, (lab, v, col)) in items.iter().enumerate() {
        let y = 40 + 30 * k;
        let w = (w_total - left - 90.0) * v / mx;
        o.push(format!(
            "<text x=\"{}\" y=\"{}\" text-anchor=\"end\">{lab}</text>",
            left - 8.0,
            y + 15
        ));
        o.push(format!(
            "<rect x=\"{left}\" y=\"{y}\" width=\"{:.1}\" height=\"20\" fill=\"{col}\"/>",
            w.max(1.0)
        ));
        o.push(format!(
            "<text x=\"{:.1}\" y=\"{}\">{} {unit}</text>",
            left + w + 6.0,
            y + 15,
            fmt_g(*v)
        ));
    }
    o.push("</svg>".into());
    o.join("\n")
}

/// Write `fsk.csv`, `lora.csv`, `lora-summary.csv`, `compare.csv` and the two charts into `out`.
pub fn run(runs: &Path, out: &Path, inp: &Inputs, psnr: Psnr) -> Result<()> {
    fs::create_dir_all(out)?;
    let fsk = fsk_rows(runs)?;
    if !fsk.is_empty() {
        write_csv(&out.join("fsk.csv"), &FSK_COLS, &fsk)?;
    }
    let lora = lora_runs(runs, inp, psnr)?;
    if !lora.is_empty() {
        write_csv(
            &out.join("lora.csv"),
            &LORA_HEADER,
            &lora.iter().map(|r| r.cells.clone()).collect::<Vec<_>>(),
        )?;
        let gs = groups(&lora)?;
        let rows: Vec<Vec<Cell>> = gs.iter().map(summary_row).collect();
        write_csv(&out.join("lora-summary.csv"), &SUMMARY_HEADER, &rows)?;
        let cmp = compare_rows(&lora, inp)?;
        write_csv(
            &out.join("compare.csv"),
            &COMPARE_HEADER,
            &cmp.iter().map(|r| r.cells.clone()).collect::<Vec<_>>(),
        )?;
        for (size, pic) in [
            ("small", "160x120 JPEG (2346 bytes)"),
            ("large", "320x240 JPEG (8618 bytes)"),
        ] {
            let mut items: Vec<(String, f64, &str)> = Vec::new();
            for r in cmp.iter().filter(|r| r.picture == size) {
                let col = if r.stack.starts_with("factory") {
                    "#c0504d"
                } else {
                    "#4f81bd"
                };
                let v = r
                    .seconds_mean
                    .with_context(|| format!("no time to picture for {}", r.stack))?;
                items.push((
                    format!(
                        "{} SF7/125",
                        r.stack.replace("moq-subghz 0.5.1 ", "moq-subghz ")
                    ),
                    v,
                    col,
                ));
            }
            for g in gs
                .iter()
                .filter(|g| g.size == size && g.mode == "chunk" && g.profile != "sf7-125")
            {
                let v = mean(
                    &g.runs
                        .iter()
                        .map(|r| r.seconds_to_image)
                        .collect::<Vec<_>>(),
                )
                .context("no seconds_to_image")?;
                items.push((
                    format!(
                        "moq-subghz chunk {}",
                        g.profile.to_uppercase().replace('-', "/")
                    ),
                    v,
                    "#9bbb59",
                ));
            }
            let title = format!("Seconds from first packet to a complete {pic}, measured, mean");
            fs::write(
                out.join(format!("time-to-picture-{size}.svg")),
                bars(&title, &items, "s"),
            )?;
        }
    }
    println!("fsk runs {}, lora runs {}", fsk.len(), lora.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{copy_tree, repo, Scratch};

    /// The committed `results/stick-0.5.1/` data laid out as the bench writes its runs.
    fn stage() -> Scratch {
        let s = Scratch::new("analyze");
        let r = repo().join("results/stick-0.5.1");
        for e in fs::read_dir(r.join("fsk")).unwrap() {
            let e = e.unwrap();
            let d = s.0.join(e.file_name());
            fs::create_dir_all(&d).unwrap();
            for f in ["summary.json", "send_summary.json"] {
                fs::copy(e.path().join(f), d.join(f)).unwrap();
            }
        }
        for e in fs::read_dir(r.join("lora")).unwrap() {
            let e = e.unwrap();
            let n = e.file_name();
            let x = s.0.join(&n).join("exact");
            fs::create_dir_all(&x).unwrap();
            for f in fs::read_dir(e.path()).unwrap() {
                let f = f.unwrap();
                let name = f.file_name().to_string_lossy().into_owned();
                if name == "images.csv" || name == "rx.csv" || name.starts_with("image") {
                    fs::copy(f.path(), x.join(&name)).unwrap();
                }
            }
            fs::copy(
                e.path().join("send.txt"),
                s.0.join(format!("{}.send", n.to_string_lossy())),
            )
            .unwrap();
        }
        s
    }

    /// The CSV text with the `ssdv_psnr_db` column emptied, for runs without ffmpeg.
    fn blank_psnr(text: &str) -> String {
        let mut lines = text.split("\r\n");
        let head = lines.next().unwrap_or("");
        let col = head.split(',').position(|c| c == "ssdv_psnr_db");
        let mut out = vec![head.to_string()];
        for l in lines {
            let mut f: Vec<&str> = l.split(',').collect();
            if let (Some(c), true) = (col, f.len() > 1) {
                f[c] = "";
            }
            out.push(f.join(","));
        }
        out.join("\r\n")
    }

    fn ffmpeg() -> bool {
        Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Parity with the Python analyzer: the committed CSVs and charts were written by it, and
    /// the Rust output has to match them byte for byte (the PSNR column needs ffmpeg).
    #[test]
    fn matches_the_committed_results_written_by_the_python_analyzer() {
        let runs = stage();
        let out = Scratch::new("analyze-out");
        let with_ffmpeg = ffmpeg();
        let none = |_: &Path, _: &Path, _: Option<&str>| Ok(None);
        let inp = Inputs::under(&repo());
        if with_ffmpeg {
            run(&runs.0, &out.0, &inp, &ffmpeg_psnr).unwrap();
        } else {
            eprintln!("ffmpeg not on PATH: the SSDV PSNR column is not compared");
            run(&runs.0, &out.0, &inp, &none).unwrap();
        }
        let want = repo().join("results/stick-0.5.1");
        for f in [
            "fsk.csv",
            "lora.csv",
            "lora-summary.csv",
            "compare.csv",
            "time-to-picture-small.svg",
            "time-to-picture-large.svg",
        ] {
            let (a, b) = (
                fs::read_to_string(out.0.join(f)).unwrap(),
                fs::read_to_string(want.join(f)).unwrap(),
            );
            if with_ffmpeg || !f.starts_with("lora") {
                assert_eq!(a, b, "{f}");
            } else {
                assert_eq!(blank_psnr(&a), blank_psnr(&b), "{f}");
            }
        }
    }

    #[test]
    fn a_run_that_never_transmitted_is_left_out_and_a_bad_summary_is_an_error() {
        let runs = Scratch::new("fsk-skip");
        let d = runs.0.join("f300-x");
        fs::create_dir_all(&d).unwrap();
        assert!(
            fsk_rows(&runs.0).unwrap().is_empty(),
            "no summary.json: the run did not happen"
        );
        fs::write(d.join("summary.json"), r#"{"radio_frames_ok":0}"#).unwrap();
        assert!(
            fsk_rows(&runs.0).unwrap().is_empty(),
            "zero frames ok: the sender failed"
        );
        fs::write(d.join("summary.json"), r#"{"radio_frames_ok":5}"#).unwrap();
        let e = fsk_rows(&runs.0).unwrap_err().to_string();
        assert!(e.contains("lacks"), "{e}");
        fs::write(d.join("summary.json"), "{not json").unwrap();
        assert!(fsk_rows(&runs.0).is_err());
    }

    #[test]
    fn run_names() {
        assert_eq!(
            parse_run_name("lorawan-sf8-500-large-r12"),
            Some(("lorawan".into(), "sf8-500".into(), "large".into(), 12))
        );
        for bad in [
            "chunk-sf7-125-small",
            "foo-sf7-125-small-r1",
            "chunk-sf10-125-small-r1",
            "chunk-sf7-125-medium-r1",
            "f300-fec8-p5-1",
        ] {
            assert_eq!(parse_run_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn passes_split_where_the_index_stops_rising() {
        assert_eq!(
            passes(&[0, 1, 2, 0, 2, 3, 1]),
            vec![vec![0, 1, 2], vec![0, 2, 3], vec![1]]
        );
        assert!(passes(&[]).is_empty());
    }

    #[test]
    fn a_missing_sender_line_is_an_error() {
        let runs = Scratch::new("nosend");
        copy_tree(
            &repo().join("results/stick-0.5.1/lora/chunk-sf7-125-small-r1"),
            &runs.0.join("chunk-sf7-125-small-r1/exact"),
        );
        fs::write(runs.0.join("chunk-sf7-125-small-r1.send"), "nothing useful").unwrap();
        let e = lora_runs(&runs.0, &Inputs::under(&repo()), &|_, _, _| Ok(None))
            .err()
            .expect("error")
            .to_string();
        assert!(e.contains("sent frames"), "{e}");
    }

    #[test]
    fn svg_bars_match_python_formatting() {
        let svg = bars(
            "t",
            &[("a".into(), 12.5, "#000"), ("b".into(), 100.0, "#111")],
            "s",
        );
        assert!(svg.contains("<text x=\"322\" y=\"55\" text-anchor=\"end\">a</text>"));
        assert!(
            svg.contains("width=\"42.5\"")
                && svg.contains("width=\"340.0\"")
                && svg.contains("12.5 s</text>")
                && svg.contains("100 s</text>")
        );
    }
}
