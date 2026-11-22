//! `factory-bench`: the factory-vs-factory picture test of `results/factory-firmware/`. The board
//! runs the stock the HAT maker app, once per text chunk of a JPEG, and the stock the board maker
//! receiver on the stick prints what it hears.  The stick capture is the `stick-cap` subcommand.
//!
//! This transmits, so it needs `--i-have-an-antenna`, `--transmit`, the serial port of the stick
//! as `--dev`, and the licence gate's permit for the carrier and power the stock app is built for.
//! The stock app cannot be queried, so the user declares them (`--freq-mhz`, `--power-dbm`) and
//! the gate checks the declaration: the stock the HAT maker build (868.1 MHz, up to +17 dBm) is
//! refused, the project's patched build (915.0 MHz, +5 dBm) is not. The gate runs before
//! anything else: before a file is read, a serial port opened, or ssh started.

use crate::cell::{num, read_dict, round_to};
use crate::exec::{capture, run_ok, Plan};
use crate::licence::{LicenceOpts, Side};
use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::Args;
use moq_subghz_licence::{Auth, Encryption, Profile, Refusal, Station, Tuning, TxPermit};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A chunk line `<id>:<seq>/<total>:<hex>` as the stick printed it.
#[derive(Debug, PartialEq)]
pub struct Chunk<'a> {
    pub id: u8,
    pub seq: i64,
    pub total: i64,
    pub hex: &'a [u8],
}

impl Chunk<'_> {
    /// The payload bytes, when the hex is whole bytes.
    pub fn bytes(&self) -> Option<Vec<u8>> {
        if !self.hex.len().is_multiple_of(2) {
            return None;
        }
        let v = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
        self.hex
            .chunks(2)
            .map(|p| Some(v(p[0])? << 4 | v(p[1])?))
            .collect()
    }
}

/// Parse a whole packet as a chunk line. `id` is the one letter to accept, or any letter.
pub fn parse_chunk(pl: &[u8], id: Option<u8>) -> Option<Chunk<'_>> {
    let digits = |s: &[u8]| s.len() == 3 && s.iter().all(u8::is_ascii_digit);
    if pl.len() < 10 || pl[1] != b':' || pl[5] != b'/' || pl[9] != b':' {
        return None;
    }
    let ok = match id {
        Some(i) => pl[0] == i,
        None => pl[0].is_ascii_alphabetic(),
    };
    let hex = &pl[10..];
    if !ok
        || !digits(&pl[2..5])
        || !digits(&pl[6..9])
        || !hex.iter().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    let n = |s: &[u8]| std::str::from_utf8(s).ok()?.parse().ok();
    Some(Chunk {
        id: pl[0],
        seq: n(&pl[2..5])?,
        total: n(&pl[6..9])?,
        hex,
    })
}

/// Text chunks `<id>:<seq>/<total>:<hex>` of a file, `size` bytes each.
pub fn make_chunks(jpeg: &[u8], id: char, size: usize) -> Vec<String> {
    let n = jpeg.len().div_ceil(size);
    (0..n)
        .map(|i| {
            let hex: String = jpeg[i * size..((i + 1) * size).min(jpeg.len())]
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            format!("{id}:{i:03}/{n:03}:{hex}")
        })
        .collect()
}

/// CSS mode airtime of a packet of `pl` bytes, in seconds (SX1276 datasheet formula, explicit header off).
pub fn airtime(pl: usize, sf: u32, bw: f64, cr: u32, crc: u32, h: u32, pre: f64) -> f64 {
    let ts = 2f64.powi(sf as i32) / bw;
    let num = 8.0 * pl as f64 - 4.0 * sf as f64 + 28.0 + 16.0 * crc as f64 - 20.0 * h as f64;
    let npay = 8.0 + ((num / (4.0 * sf as f64)).ceil() * (cr + 4) as f64).max(0.0);
    (pre + 4.25 + npay) * ts
}

/// One packet the stick printed: when, how strong, and the text.
#[derive(Clone, Debug)]
pub struct Packet {
    pub t_mono_s: f64,
    pub rssi_dbm: i64,
    pub payload: Vec<u8>,
}

/// Read the stick capture `<stem>.packets.csv`.
pub fn read_packets(path: &Path) -> Result<Vec<Packet>> {
    let mut out = Vec::new();
    for r in read_dict(path)? {
        let get = |k: &str| {
            r.get(k)
                .with_context(|| format!("{}: no column {k}", path.display()))
        };
        out.push(Packet {
            t_mono_s: num(get("t_mono_s")?)
                .with_context(|| format!("{}: bad t_mono_s", path.display()))?,
            rssi_dbm: get("rssi_dbm")?
                .trim()
                .parse()
                .with_context(|| format!("{}: bad rssi_dbm", path.display()))?,
            payload: base64::engine::general_purpose::STANDARD
                .decode(get("payload_b64")?)
                .with_context(|| format!("{}: bad base64", path.display()))?,
        });
    }
    Ok(out)
}

/// What one pass of the picture did on the air.
#[derive(Debug, PartialEq)]
pub struct PassStat {
    pub pass_no: usize,
    pub sent: usize,
    pub received_packets: usize,
    pub received_unique: usize,
    pub malformed: usize,
    pub loss_pct: f64,
    pub tx_wall_s: f64,
    pub rssi: Option<(i64, i64, f64)>,
}

impl PassStat {
    fn json(&self) -> Value {
        let (lo, hi, mean) = match self.rssi {
            Some((a, b, m)) => (json!(a), json!(b), json!(m)),
            None => (Value::Null, Value::Null, Value::Null),
        };
        json!({"pass_no": self.pass_no, "sent": self.sent, "received_packets": self.received_packets,
            "received_unique": self.received_unique, "malformed": self.malformed, "loss_pct": self.loss_pct,
            "tx_wall_s": self.tx_wall_s, "rssi_min": lo, "rssi_max": hi, "rssi_mean": mean})
    }
}

/// The picture so far: the chunks held and what the next pass has to send.
pub struct Reassembly {
    pub id: u8,
    pub n: i64,
    /// Chunk indexes to try on the next pass.
    pub want: Vec<usize>,
    pub got: BTreeMap<i64, Vec<u8>>,
    limit: usize,
}

impl Reassembly {
    pub fn new(id: u8, n: usize, limit: usize) -> Reassembly {
        Reassembly {
            id,
            n: n as i64,
            want: (0..if limit > 0 { limit } else { n }).collect(),
            got: BTreeMap::new(),
            limit,
        }
    }

    /// Fold in one pass: `rows` is everything the stick heard so far, `before` how many of
    /// those came before this pass. Returns the pass statistics and updates `want`.
    pub fn pass(&mut self, no: usize, rows: &[Packet], before: usize, tx_wall_s: f64) -> PassStat {
        for p in rows {
            if let Some(c) = parse_chunk(&p.payload, Some(self.id)) {
                if c.total == self.n {
                    if let Some(b) = c.bytes() {
                        self.got.entry(c.seq).or_insert(b);
                    }
                }
            }
        }
        let (mut this, mut rssi, mut bad) = (Vec::new(), Vec::new(), 0);
        for p in &rows[before.min(rows.len())..] {
            match parse_chunk(&p.payload, Some(self.id)) {
                Some(c) => {
                    this.push(c.seq);
                    rssi.push(p.rssi_dbm);
                }
                None => bad += 1,
            }
        }
        let sent = self.want.len();
        let uniq = this
            .iter()
            .filter(|&&s| self.want.contains(&(s as usize)))
            .collect::<BTreeSet<_>>()
            .len();
        let stat = PassStat {
            pass_no: no,
            sent,
            received_packets: this.len(),
            received_unique: uniq,
            malformed: bad,
            loss_pct: round_to(100.0 * (sent as f64 - uniq as f64) / sent as f64, 2),
            tx_wall_s,
            rssi: (!rssi.is_empty()).then(|| {
                (
                    *rssi.iter().min().unwrap(),
                    *rssi.iter().max().unwrap(),
                    round_to(rssi.iter().sum::<i64>() as f64 / rssi.len() as f64, 1),
                )
            }),
        };
        let upto = if self.limit > 0 {
            self.limit
        } else {
            self.n as usize
        };
        self.want = (0..upto)
            .filter(|i| !self.got.contains_key(&(*i as i64)))
            .collect();
        stat
    }

    pub fn complete(&self) -> bool {
        self.got.len() as i64 == self.n && self.want.is_empty()
    }

    /// The picture, when every chunk is held.
    pub fn image(&self) -> Option<Vec<u8>> {
        (self.got.len() as i64 == self.n).then(|| self.got.values().flatten().copied().collect())
    }
}

/// First chunk heard to the chunk that completes the picture, plus one packet of airtime.
pub fn first_to_complete(rows: &[Packet], id: u8, n: i64, airtime_s: f64) -> Option<f64> {
    let (mut first, mut held) = (None, BTreeSet::new());
    for p in rows {
        let Some(c) = parse_chunk(&p.payload, Some(id)) else {
            continue;
        };
        if c.total != n {
            continue;
        }
        let f = *first.get_or_insert(p.t_mono_s);
        held.insert(c.seq);
        if held.len() as i64 == n {
            return Some(round_to(p.t_mono_s - f + airtime_s, 2));
        }
    }
    None
}

/// The `summary.json` of a finished run.
pub struct Summary<'a> {
    pub label: &'a str,
    pub jpeg_name: &'a str,
    pub jpeg: &'a [u8],
    pub chunks_total: usize,
    pub pkt_len: usize,
    pub at: f64,
    pub passes: &'a [PassStat],
    pub re: &'a Reassembly,
    pub end_to_end_s: f64,
    pub window: Option<f64>,
}

impl Summary<'_> {
    pub fn json(&self) -> Value {
        let img = self.re.image();
        let ok = img.as_deref() == Some(self.jpeg);
        let complete = self.re.complete();
        let tx_wall: f64 = self.passes.iter().map(|p| p.tx_wall_s).sum();
        let total_sent: usize = self.passes.iter().map(|p| p.sent).sum();
        let mut m = Map::new();
        let mut put = |k: &str, v: Value| m.insert(k.to_string(), v);
        put("label", json!(self.label));
        put("jpeg", json!(self.jpeg_name));
        put("jpeg_bytes", json!(self.jpeg.len()));
        put(
            "jpeg_sha256",
            json!(Sha256::digest(self.jpeg)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()),
        );
        put("chunks_total", json!(self.chunks_total));
        put("chunks_attempted_pass1", json!(self.passes[0].sent));
        put("payload_bytes_per_packet_max", json!(self.pkt_len));
        put(
            "calc_airtime_per_packet_ms",
            json!(round_to(self.at * 1000.0, 1)),
        );
        put(
            "passes",
            Value::Array(self.passes.iter().map(PassStat::json).collect()),
        );
        put("total_packets_sent", json!(total_sent));
        put("pass1_loss_pct", json!(self.passes[0].loss_pct));
        put("reassembled", json!(complete));
        put("matches_original", json!(ok));
        put("end_to_end_s", json!(round_to(self.end_to_end_s, 2)));
        put(
            "first_heard_to_complete_s",
            self.window.map_or(Value::Null, |w| json!(w)),
        );
        put("tx_wall_total_s", json!(round_to(tx_wall, 2)));
        put(
            "goodput_bit_s",
            if complete {
                json!(round_to(
                    self.jpeg.len() as f64 * 8.0 / self.end_to_end_s,
                    1
                ))
            } else {
                Value::Null
            },
        );
        put(
            "onair_text_rate_bit_s",
            json!(round_to(
                total_sent as f64 * self.pkt_len as f64 * 8.0 / tx_wall,
                1
            )),
        );
        put(
            "calc_airtime_total_s",
            json!(round_to(self.at * total_sent as f64, 2)),
        );
        Value::Object(m)
    }
}

/// Pretty JSON with one space of indent, like Python's `json.dump(indent=1)`.
pub fn pretty(v: &Value) -> Result<String> {
    use serde::Serialize;
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut buf,
        serde_json::ser::PrettyFormatter::with_indent(b" "),
    );
    v.serialize(&mut ser)?;
    Ok(String::from_utf8(buf)?)
}

#[derive(Args, Clone, Debug)]
pub struct Opts {
    /// Name of the run, and its folder under --out
    #[arg(long)]
    pub label: String,
    /// The JPEG to send
    #[arg(long)]
    pub jpeg: PathBuf,
    /// Letter that starts every chunk
    #[arg(long, default_value = "A")]
    pub id: char,
    /// Most passes: each re-sends only the chunks still missing
    #[arg(long, default_value_t = 3)]
    pub passes: usize,
    /// JPEG bytes per chunk
    #[arg(long, default_value_t = 120)]
    pub chunk: usize,
    /// Send only the first N chunks (smoke test)
    #[arg(long, default_value_t = 0)]
    pub limit: usize,
    #[arg(long, default_value = "results/factory-firmware/data")]
    pub out: PathBuf,
    /// ssh host of the board
    #[arg(long, default_value = "pi")]
    pub pi: String,
    /// Serial device of the stick (the receiver sketch prints at 9600 baud). No default: it is opened.
    #[arg(long)]
    pub dev: PathBuf,
    /// This binary on the board, built for it (`factory-send` runs there)
    #[arg(long, default_value = "factory-bench/moq-subghz-bench")]
    pub pi_tool: String,
    /// Directory of the stock `hat_app` on the board
    #[arg(long, default_value = "factory-bench/gps-hat")]
    pub pi_app_dir: String,
    /// Required: the antennas are on both radios
    #[arg(long)]
    pub i_have_an_antenna: bool,
    /// Required to transmit (the licence gate). Forwarded to the board's `factory-send`
    #[arg(long)]
    pub transmit: bool,
    /// The carrier the stock app on the board is built for, in MHz. No default: the app cannot be
    /// queried, so you declare it and the gate checks it (the stock build's 868.1 is refused)
    #[arg(long)]
    pub freq_mhz: f64,
    /// The power the stock app on the board is built for, in dBm (the project allows 2 to 5)
    #[arg(long)]
    pub power_dbm: i8,
    #[command(flatten)]
    pub licence: LicenceOpts,
    /// Print the commands and stop
    #[arg(long)]
    pub dry_run: bool,
}

/// What the user says the stock app is built for. It cannot be read back from the app.
#[derive(Clone, Copy, Debug)]
pub struct Declared {
    pub freq_mhz: f64,
    pub power_dbm: i8,
}

/// The stock app's CSS mode bandwidth (125 kHz), the width of the span the gate checks.
const APP_WIDTH_HZ: u64 = 125_000;

/// Why the stock the HAT maker build cannot be benched through this tool, and the way that can.
const STOCK_868: &str = "the stock the HAT maker app transmits 868.1 MHz at up to +17 dBm, which is outside the \
project's US ISM rules, and the gate has no 868 MHz profile to check it against (the EU region is not \
enabled; region \"other\" is blocked). To bench legitimately, apply firmware/vendor/sx1276-gps-hat/gps-hat-bench.patch \
(915.0 MHz, +5 dBm), rebuild the app on the board and declare --freq-mhz 915.0 --power-dbm 5";

/// The licence gate for the stock app on the declared carrier and power. The result is the proof
/// that [`send_chunks`] needs, so nothing is keyed without it. Refuses outside 902-928 MHz (not
/// within 0.3 MHz of 925.0) and outside 2 to 5 dBm, needs `--transmit`, and never permits a
/// run that would need station identification frames (the stock app cannot send them).
pub fn gate(station: &Station, d: Declared) -> Result<TxPermit> {
    if !d.freq_mhz.is_finite() || !(1.0..=7000.0).contains(&d.freq_mhz) {
        bail!("--freq-mhz {} is not a frequency in MHz", d.freq_mhz);
    }
    let hz = (d.freq_mhz * 1e6).round() as u64;
    let permit = station
        .check_transmit_without_station_id(
            Tuning::occupied(hz, APP_WIDTH_HZ),
            Profile::ISM,
            Encryption::None,
            Auth::None,
        )
        .map_err(|e| match e {
            Refusal::OutOfBand => anyhow::anyhow!("refusing to transmit: {e}. {STOCK_868}"),
            e => anyhow::anyhow!("refusing to transmit: {e}"),
        })?;
    // The project's own rule on top of the gate's table (which also lists 2.4 and 5.8 GHz): the
    // stock app is a sub-GHz CSS mode build, and this repo operates inside 902-928 MHz only.
    if !(902.0..=928.0).contains(&d.freq_mhz) {
        bail!(
            "refusing to transmit: --freq-mhz {} is outside the project's 902-928 MHz. {STOCK_868}",
            d.freq_mhz
        );
    }
    if !(2..=5).contains(&d.power_dbm) {
        bail!(
            "refusing to transmit: --power-dbm {} is outside the project's 2 to 5 dBm. {STOCK_868}",
            d.power_dbm
        );
    }
    Ok(permit)
}

/// The stick capture, started once before the first pass.
pub fn cap_plan(o: &Opts, exe: &str) -> Plan {
    let stem = o.out.join(&o.label).join("stick");
    Plan::new(
        exe,
        [
            "stick-cap".to_string(),
            "--dev".into(),
            o.dev.display().to_string(),
            "--out".into(),
            stem.display().to_string(),
        ],
    )
}

/// The remote scratch directory of one run: private (`mktemp -d`, mode 0700) and its own, so two
/// runs on one board never share a chunk file.
pub fn scratch_plan(o: &Opts) -> Plan {
    crate::orch::ssh_plan(&o.pi, "mktemp -d /tmp/moq-bench.XXXXXXXXXX".to_string())
}

/// Removes the run's scratch directory, that exact directory only.
pub fn cleanup_plan(o: &Opts, dir: &str) -> Plan {
    crate::orch::ssh_plan(&o.pi, format!("rm -rf {}", shq(dir)))
}

/// The local run directory `out/label`, made exclusively (mode 0700): a second run with the same
/// label and out dir fails instead of sharing its chunk files, stick capture and summary.
pub fn reserve_run_dir(o: &Opts) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let d = o.out.join(&o.label);
    fs::create_dir_all(&o.out).with_context(|| format!("create {}", o.out.display()))?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&d)
        .with_context(|| {
            format!(
                "the run directory {} exists or cannot be made: use another --label or --out, or remove it",
                d.display()
            )
        })?;
    Ok(d)
}

/// The run's remote scratch directory, removed when this drops (on success, error or early return).
struct RemoteDir<'a> {
    o: &'a Opts,
    path: String,
}

impl RemoteDir<'_> {
    fn make(o: &Opts) -> Result<RemoteDir<'_>> {
        let made = capture(&scratch_plan(o)).context("make the scratch directory on the board")?;
        let path = made.trim().to_string();
        crate::orch::valid_scratch(&path)?;
        Ok(RemoteDir { o, path })
    }
}

impl Drop for RemoteDir<'_> {
    fn drop(&mut self) {
        if let Err(e) = run_ok(&cleanup_plan(self.o, &self.path)) {
            eprintln!(
                "warning: could not remove {} on {}: {e:#}",
                self.path, self.o.pi
            );
        }
    }
}

/// The commands one pass runs, in order: copy the chunk list into the run's scratch directory `dir`
/// on the board (its own file per pass), send it there.
pub fn pass_plan(o: &Opts, pass: usize, dir: &str) -> [Plan; 2] {
    let lic = o.licence.resolve().flags(Side::Pi, &[]);
    let lic: String = lic.iter().map(|f| format!("{} ", shq(f))).collect();
    let local = o.out.join(&o.label).join(format!("pass{pass}.chunks.txt"));
    let remote = format!("{dir}/pass{pass}.chunks.txt");
    [
        crate::orch::scp_plan(
            local.display().to_string(),
            format!("{}:{remote}", crate::orch::scp_host(&o.pi)),
        ),
        crate::orch::ssh_plan(
            &o.pi,
            format!(
                "{} factory-send --i-have-an-antenna --transmit --freq-mhz {} --power-dbm {} {lic}--app-dir {} {}",
                shq(&o.pi_tool),
                o.freq_mhz,
                o.power_dbm,
                shq(&o.pi_app_dir),
                shq(&remote)
            ),
        ),
    ]
}

/// Quote one word for the remote POSIX shell.
pub fn shq(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-./=:,+@%".contains(&b))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Most chunks of a picture: the counters are three digits (`007/123`).
pub const MAX_CHUNKS: usize = 999;
/// Longest packet the CSS mode radio takes.
pub const MAX_PACKET: usize = 255;

/// Refuse a picture whose chunks the receiver cannot parse or the radio cannot send, before
/// anything is transmitted.
pub fn check_chunks(chunks: &[String]) -> Result<()> {
    let n = chunks.len();
    if n == 0 {
        bail!("the picture is empty: nothing to send");
    }
    if n > MAX_CHUNKS {
        bail!("{n} chunks is above the {MAX_CHUNKS} the three digit counters allow: use a larger --chunk or a smaller picture");
    }
    let longest = chunks.iter().map(String::len).max().unwrap_or(0);
    if longest > MAX_PACKET {
        bail!("a packet of {longest} bytes is above the {MAX_PACKET} the radio takes: use a smaller --chunk");
    }
    Ok(())
}

pub fn run(o: &Opts) -> Result<()> {
    // The gate is the first thing: before a file is read, a port opened or ssh started.
    let station = o
        .licence
        .gate_args(o.transmit)
        .station()
        .context("licence")?;
    let permit = gate(
        &station,
        Declared {
            freq_mhz: o.freq_mhz,
            power_dbm: o.power_dbm,
        },
    );
    println!("{}", o.licence.resolve().describe());
    match permit {
        Err(e) if o.dry_run => println!("gate: a real run would be REFUSED: {e:#}"),
        Err(e) => return Err(e),
        Ok(_) => eprintln!(
            "gate passed for the DECLARED {} MHz at +{} dBm (the stock app cannot be queried: you attest that the build on the board uses these)",
            o.freq_mhz, o.power_dbm
        ),
    }
    crate::orch::valid_name(&o.label).context("--label")?;
    crate::orch::validate_host(&o.pi)?;
    let exe = std::env::current_exe()?.display().to_string();
    let jpeg = crate::cell::read_capped(&o.jpeg, crate::cell::MAX_PICTURE)?;
    if o.chunk == 0 || !o.id.is_ascii_alphabetic() {
        bail!("--chunk must be above 0 and --id a letter");
    }
    let chunks = make_chunks(&jpeg, o.id, o.chunk);
    check_chunks(&chunks)?;
    let n = chunks.len();
    if o.limit > n {
        bail!("--limit {} is above the {n} chunks of the picture", o.limit);
    }
    if o.dry_run {
        println!(
            "{n} chunks of up to {} bytes, {} passes at most",
            o.chunk, o.passes
        );
        println!("{}", cap_plan(o, &exe).shell());
        println!("{}", scratch_plan(o).shell());
        let dir = "/tmp/moq-bench.XXXXXXXXXX";
        for p in (1..=o.passes.min(2)).flat_map(|k| pass_plan(o, k, dir)) {
            println!("{}", p.shell());
        }
        println!("{}", cleanup_plan(o, dir).shell());
        return Ok(());
    }
    if !o.i_have_an_antenna {
        bail!("refusing to transmit without --i-have-an-antenna");
    }
    let d = reserve_run_dir(o)?;
    println!("run directory: {}", d.display());
    let pkt_len = chunks.iter().map(String::len).max().unwrap_or(0);
    let at = airtime(pkt_len, 7, 125e3, 1, 1, 0, 8.0);
    let cp = cap_plan(o, &exe);
    let mut cap = StopOnDrop(
        std::process::Command::new(&cp.prog)
            .args(&cp.args)
            .spawn()
            .with_context(|| format!("start {}", cp.shell()))?,
    );
    bench(o, &d, &jpeg, &chunks, pkt_len, at, &mut cap.0)
}

/// The stick capture, ended and reaped when this drops (any return, or a panic), so the serial
/// device is free for a retry.
struct StopOnDrop(std::process::Child);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        // SAFETY: signalling our own child.
        unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) };
        let _ = self.0.wait();
    }
}

fn bench(
    o: &Opts,
    d: &Path,
    jpeg: &[u8],
    chunks: &[String],
    pkt_len: usize,
    at: f64,
    cap: &mut std::process::Child,
) -> Result<()> {
    let packets = d.join("stick.packets.csv");
    let read = || -> Result<Vec<Packet>> {
        if packets.exists() {
            read_packets(&packets)
        } else {
            Ok(Vec::new())
        }
    };
    std::thread::sleep(Duration::from_millis(1500));
    if let Some(st) = cap.try_wait()? {
        bail!("stick-cap exited early with {st}: check --dev");
    }
    let remote = RemoteDir::make(o)?;
    let t_run = Instant::now();
    let mut re = Reassembly::new(o.id as u8, chunks.len(), o.limit);
    let mut passes = Vec::new();
    for ps in 1..=o.passes {
        let list: Vec<&str> = re.want.iter().map(|&i| chunks[i].as_str()).collect();
        let local = d.join(format!("pass{ps}.chunks.txt"));
        fs::write(&local, list.join("\n") + "\n")?;
        let before = read()?.len();
        let t0 = Instant::now();
        let plan = pass_plan(o, ps, &remote.path);
        run_ok(&plan[0])?;
        let tx = capture(&plan[1])?;
        let wall = t0.elapsed().as_secs_f64();
        std::thread::sleep(Duration::from_secs(2));
        fs::write(d.join(format!("pass{ps}.pi-tx.log")), &tx)?;
        let st = re.pass(ps, &read()?, before, round_to(wall, 2));
        println!("{}", serde_json::to_string(&st.json())?);
        passes.push(st);
        if re.want.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    let end = t_run.elapsed().as_secs_f64();
    match re.image() {
        Some(img) => fs::write(d.join("reassembled.jpg"), img)?,
        None if !re.got.is_empty() => fs::write(
            d.join("partial.bin"),
            re.got
                .values()
                .flatten()
                .copied()
                .collect::<Vec<u8>>()
                .as_slice(),
        )?,
        None => {}
    }
    let s = Summary {
        label: &o.label,
        jpeg_name: o.jpeg.file_name().and_then(|n| n.to_str()).unwrap_or(""),
        jpeg,
        chunks_total: chunks.len(),
        pkt_len,
        at,
        passes: &passes,
        re: &re,
        end_to_end_s: end,
        window: first_to_complete(&read()?, o.id as u8, chunks.len() as i64, at),
    }
    .json();
    let text = pretty(&s)?;
    fs::write(d.join("summary.json"), &text)?;
    println!("{text}");
    verdict(&o.label, o.passes, &re, jpeg)
}

/// Success is the byte comparison, not the chunk count: a picture with every chunk present but
/// different bytes from the original (a corrupted chunk that passed the stick's CRC) fails.
pub fn verdict(label: &str, passes: usize, re: &Reassembly, jpeg: &[u8]) -> Result<()> {
    if !re.complete() {
        bail!("FAILED {label}: the picture is not complete after {passes} passes");
    }
    if re.image().as_deref() != Some(jpeg) {
        bail!("FAILED {label}: every chunk arrived but the picture does not match the original (matches_original is false)");
    }
    Ok(())
}

/// The board side: send each chunk line with the stock HAT maker app, and write `<start> <end> <id:seq>`
/// for each to `out`.
///
/// The app does not exit on its own after a send, so it is cut off at `app_timeout` (the expected
/// end). One that exits zero sooner is reported on stderr and its row ends in `early-exit`; one that
/// exits non-zero sooner is a failed transmit: no row for it, and the whole run fails with its
/// status and the tail of its stderr.
///
/// `_permit` is the gate's answer ([`gate`]): the stock app is not started without one.
pub fn send_chunks(
    _permit: &TxPermit,
    chunks: &Path,
    app_dir: &Path,
    app_timeout: Duration,
    cancel: &AtomicBool,
    out: &mut impl std::io::Write,
) -> Result<()> {
    use std::io::Read;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let text = crate::cell::read_text(chunks, crate::cell::MAX_TEXT)?;
    let epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    };
    for line in text.lines() {
        // No chunk starts after a cancellation.
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let s = epoch();
        let mut child = Command::new("./hat_app")
            .args(["sender", line])
            .current_dir(app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map(TxGroup::new)
            .with_context(|| format!("start ./hat_app in {}", app_dir.display()))?;
        // Read stderr as it comes into a shared tail, so a chatty app never blocks on a full pipe
        // and a drain that outlasts the deadline still reports what it read.
        let mut err = child.0.stderr.take().context("no stderr pipe")?;
        let tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let shared = tail.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = err.read(&mut buf) {
                if let Ok(mut keep) = shared.lock() {
                    keep.extend_from_slice(&buf[..n]);
                    let cut = keep.len().saturating_sub(STDERR_TAIL);
                    keep.drain(..cut);
                }
            }
            let _ = done_tx.send(());
        });
        // The same cut-off as `timeout 1.3`.
        let t0 = Instant::now();
        let early = loop {
            if cancel.load(Ordering::Relaxed) {
                // `child` drops here: the whole group is ended and reaped.
                return Err(Cancelled.into());
            }
            if let Some(st) = child.0.try_wait()? {
                break Some(st);
            }
            if t0.elapsed() >= app_timeout {
                child.end_now();
                break None;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        // Anything the app started stays in its process group: end it, so nothing outlives the
        // chunk, and bound the stderr drain by the same deadline (a grandchild may hold the pipe).
        let drain_by = t0 + app_timeout;
        let drained = loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Cancelled.into());
            }
            let left = drain_by.saturating_duration_since(Instant::now());
            match done_rx.recv_timeout(left.min(Duration::from_millis(20))) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) if !left.is_zero() => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break false,
                _ => break true,
            }
        };
        child.end_now();
        if !drained {
            let _ = done_rx.recv_timeout(Duration::from_millis(50));
        }
        let tail = String::from_utf8_lossy(&tail.lock().map(|t| t.clone()).unwrap_or_default())
            .into_owned();
        if cancel.load(Ordering::Relaxed) {
            return Err(Cancelled.into());
        }
        let head: Vec<&str> = line.splitn(3, ':').take(2).collect();
        let id = head.join(":");
        let mut note = "";
        if let Some(st) = early {
            if !st.success() {
                bail!(
                    "FAILED {id}: ./hat_app exited early with {st}; stderr tail: {:?}",
                    tail.trim_end()
                );
            }
            eprintln!("warning: {id}: ./hat_app exited 0 before the timeout, which it should not");
            note = " early-exit";
        }
        writeln!(out, "{s:.9} {:.9} {id}{note}", epoch())?;
    }
    Ok(())
}

/// The stock app and everything it started (its own process group). Dropping it, on any return or
/// a panic, ends the group with TERM, then KILL after a short grace, and reaps the app.
struct TxGroup(std::process::Child);

/// How long the group gets to leave on TERM before it is killed.
const TX_GRACE: Duration = Duration::from_millis(300);

impl TxGroup {
    fn new(c: std::process::Child) -> TxGroup {
        TxGroup(c)
    }

    fn signal(&self, sig: i32) {
        // SAFETY: signalling the process group made for this child (its pid is the group id).
        unsafe { libc::kill(-(self.0.id() as i32), sig) };
    }

    /// KILL the whole group now and reap the app.
    fn end_now(&mut self) {
        self.signal(libc::SIGKILL);
        let _ = self.0.wait();
    }
}

impl Drop for TxGroup {
    fn drop(&mut self) {
        self.signal(libc::SIGTERM);
        let t = Instant::now();
        while t.elapsed() < TX_GRACE && matches!(self.0.try_wait(), Ok(None)) {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.end_now();
    }
}

static CANCEL: AtomicBool = AtomicBool::new(false);

/// The worker was stopped by SIGINT, SIGTERM or SIGHUP: its group is ended and the process exits 130.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled by a signal: the stock app's process group was ended")
    }
}

impl std::error::Error for Cancelled {}

extern "C" fn on_signal(_: libc::c_int) {
    CANCEL.store(true, Ordering::Relaxed);
}

/// A flag that SIGINT, SIGTERM and SIGHUP set, for the worker to stop and clean up on.
pub fn cancel_flag() -> &'static AtomicBool {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only stores to an atomic; the sigaction struct is zeroed then filled.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            libc::sigaction(sig, &sa, std::ptr::null_mut());
        }
    }
    &CANCEL
}

/// Bytes of the stock app's stderr kept for a failure message.
const STDERR_TAIL: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    fn pk(t: f64, rssi: i64, s: &str) -> Packet {
        Packet {
            t_mono_s: t,
            rssi_dbm: rssi,
            payload: s.as_bytes().to_vec(),
        }
    }

    #[test]
    fn chunk_lines() {
        let c = make_chunks(&[1, 2, 255, 16], 'A', 3);
        assert_eq!(c, ["A:000/002:0102ff", "A:001/002:10"]);
        let p = parse_chunk(b"A:001/002:10", Some(b'A')).unwrap();
        assert_eq!((p.seq, p.total, p.bytes()), (1, 2, Some(vec![0x10])));
        assert!(parse_chunk(b"B:001/002:10", Some(b'A')).is_none());
        assert!(parse_chunk(b"A:001/002:1G", None).is_none());
        assert!(parse_chunk(b"A:01/002:10", None).is_none());
        assert_eq!(parse_chunk(b"A:001/002:1", None).unwrap().bytes(), None);
    }

    #[test]
    fn airtime_of_a_120_byte_chunk() {
        // 120 data bytes in hex plus the 10 byte header is 250 bytes: 389.4 ms, the committed figure.
        let at = airtime(250, 7, 125e3, 1, 1, 0, 8.0);
        assert_eq!(round_to(at * 1000.0, 1), 389.4);
    }

    #[test]
    fn passes_resend_only_what_is_missing() {
        let jpeg = [1u8, 2, 3, 4, 5, 6];
        let chunks = make_chunks(&jpeg, 'A', 2);
        let mut re = Reassembly::new(b'A', 3, 0);
        let rows = vec![
            pk(1.0, -40, &chunks[0]),
            pk(2.0, -38, &chunks[2]),
            pk(3.0, -39, "garbage"),
        ];
        let s = re.pass(1, &rows, 0, 3.0);
        assert_eq!(
            (s.sent, s.received_unique, s.malformed, s.loss_pct),
            (3, 2, 1, 33.33)
        );
        assert_eq!(s.rssi, Some((-40, -38, -39.0)));
        assert_eq!(re.want, vec![1]);
        assert!(!re.complete());
        let mut rows2 = rows.clone();
        rows2.push(pk(9.0, -41, &chunks[1]));
        let s = re.pass(2, &rows2, 3, 1.0);
        assert_eq!((s.sent, s.received_packets, s.loss_pct), (1, 1, 0.0));
        assert!(re.complete());
        assert_eq!(re.image().as_deref(), Some(&jpeg[..]));
        assert_eq!(first_to_complete(&rows2, b'A', 3, 0.5), Some(8.5));
    }

    #[test]
    fn a_complete_but_corrupted_picture_fails_the_run() {
        let jpeg = [1u8, 2, 3, 4, 5, 6];
        let chunks = make_chunks(&jpeg, 'A', 3);
        let mut re = Reassembly::new(b'A', 2, 0);
        let rows = [pk(1.0, -40, &chunks[0]), pk(2.0, -40, &chunks[1])];
        re.pass(1, &rows, 0, 1.0);
        assert!(verdict("t", 1, &re, &jpeg).is_ok());
        // same chunk count, one byte different from what was sent
        let bad = make_chunks(&[1u8, 2, 3, 4, 5, 7], 'A', 3);
        let mut re = Reassembly::new(b'A', 2, 0);
        re.pass(1, &[pk(1.0, -40, &bad[0]), pk(2.0, -40, &bad[1])], 0, 1.0);
        assert!(re.complete());
        let e = verdict("t", 1, &re, &jpeg).unwrap_err().to_string();
        assert!(
            e.starts_with("FAILED t:") && e.contains("does not match"),
            "{e}"
        );
        // missing chunks still fail the old way
        let mut re = Reassembly::new(b'A', 2, 0);
        re.pass(1, &[pk(1.0, -40, &chunks[0])], 0, 1.0);
        assert!(verdict("t", 1, &re, &jpeg)
            .unwrap_err()
            .to_string()
            .contains("not complete"));
    }

    /// A scripted stand-in for the stock app, in its own directory.
    fn fake_app(tag: &str, body: &str) -> (crate::testutil::Scratch, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let d = crate::testutil::Scratch::new(tag);
        let app = d.0.join("hat_app");
        fs::write(&app, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
        let list = d.0.join("chunks.txt");
        fs::write(&list, "A:000/002:aa\nA:001/002:bb\n").unwrap();
        (d, list)
    }

    /// One at a time: a fork in a sibling test while a script is open for writing makes exec fail
    /// with "text file busy".
    fn one_at_a_time() -> std::sync::MutexGuard<'static, ()> {
        static ONE: std::sync::Mutex<()> = std::sync::Mutex::new(());
        ONE.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The pid in `file` (written by a stand-in app), once it is there.
    fn pid_of(file: &Path) -> i32 {
        for _ in 0..200 {
            if let Some(p) = fs::read_to_string(file)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                return p;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("no pid in {}", file.display());
    }

    /// True while `pid` is a process that can still run (a zombie awaiting its reaper is not).
    fn runs(pid: i32) -> bool {
        fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z"))
    }

    fn send(tag: &str, body: &str, timeout_ms: u64) -> (Result<()>, String, Duration) {
        let _one = one_at_a_time();
        let (d, list) = fake_app(tag, body);
        let st = LicenceOpts::default().gate_args(true).station().unwrap();
        let permit = gate(
            &st,
            Declared {
                freq_mhz: 915.0,
                power_dbm: 5,
            },
        )
        .unwrap();
        let t = Instant::now(); // after the lock: waiting for a sibling is not the worker's time
        let mut out = Vec::new();
        let no = AtomicBool::new(false);
        let r = send_chunks(
            &permit,
            &list,
            &d.0,
            Duration::from_millis(timeout_ms),
            &no,
            &mut out,
        );
        (r, String::from_utf8(out).unwrap(), t.elapsed())
    }

    #[test]
    fn an_app_cut_off_by_the_timeout_is_the_normal_path() {
        let (r, out, _) = send("fs-timeout", "exec sleep 5", 150);
        r.unwrap();
        let rows: Vec<&str> = out.lines().collect();
        assert_eq!(rows.len(), 2, "{out}");
        assert!(
            rows[0].ends_with(" A:000/002") && rows[1].ends_with(" A:001/002"),
            "{out}"
        );
    }

    #[test]
    fn an_app_that_exits_zero_early_is_marked() {
        let (r, out, _) = send("fs-zero", "exit 0", 3000);
        r.unwrap();
        assert!(out.lines().all(|l| l.ends_with(" early-exit")), "{out}");
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn an_app_that_exits_nonzero_early_fails_the_run_with_no_row() {
        let (r, out, _) = send("fs-fail", "echo radio not found >&2; exit 1", 3000);
        let e = r.unwrap_err().to_string();
        assert!(e.starts_with("FAILED A:000"), "{e}");
        assert!(
            e.contains("exit status: 1") && e.contains("radio not found"),
            "{e}"
        );
        assert_eq!(
            out, "",
            "no timing row for the failed chunk, and the run stops"
        );
    }

    #[test]
    fn a_grandchild_holding_stderr_does_not_delay_the_failure_report() {
        let (r, out, took) = send("fs-grand", "sleep 20 & echo gone >&2; exit 1", 300);
        let e = r.unwrap_err().to_string();
        assert!(e.starts_with("FAILED A:000") && e.contains("gone"), "{e}");
        assert_eq!(out, "");
        assert!(took < Duration::from_millis(900), "{took:?}");
    }

    #[test]
    fn a_cancelled_worker_ends_and_reaps_the_whole_group_even_if_it_ignores_term() {
        let _one = one_at_a_time();
        let (d, list) = fake_app(
            "fs-cancel",
            "trap '' TERM; sleep 60 & echo $! > gpid; echo $$ > pid; while :; do sleep 1; done",
        );
        let st = LicenceOpts::default().gate_args(true).station().unwrap();
        let permit = gate(
            &st,
            Declared {
                freq_mhz: 915.0,
                power_dbm: 5,
            },
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let t = Instant::now();
        let mut out = Vec::new();
        let r = std::thread::scope(|sc| {
            sc.spawn(|| {
                pid_of(&d.0.join("gpid"));
                pid_of(&d.0.join("pid"));
                cancel.store(true, Ordering::Relaxed);
            });
            send_chunks(
                &permit,
                &list,
                &d.0,
                Duration::from_secs(60),
                &cancel,
                &mut out,
            )
        });
        assert!(r.unwrap_err().to_string().contains("cancelled"));
        assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
        let (app, grandchild) = (pid_of(&d.0.join("pid")), pid_of(&d.0.join("gpid")));
        // the app was reaped (no zombie: its /proc entry is gone) and so is what it started
        assert!(!Path::new(&format!("/proc/{app}")).exists());
        for _ in 0..200 {
            if !runs(grandchild) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!runs(grandchild));
    }

    /// Run the worker with `cancel` raised by `raise` once the stand-in app has started.
    fn cancelled_run(
        tag: &str,
        body: &str,
        preset: bool,
    ) -> (Result<()>, String, Duration, crate::testutil::Scratch) {
        let _one = one_at_a_time();
        let (d, list) = fake_app(tag, body);
        let st = LicenceOpts::default().gate_args(true).station().unwrap();
        let permit = gate(
            &st,
            Declared {
                freq_mhz: 915.0,
                power_dbm: 5,
            },
        )
        .unwrap();
        let cancel = AtomicBool::new(preset);
        let mut out = Vec::new();
        let t = Instant::now();
        let r = std::thread::scope(|sc| {
            if !preset {
                sc.spawn(|| {
                    pid_of(&d.0.join("started"));
                    std::thread::sleep(Duration::from_millis(100));
                    cancel.store(true, Ordering::Relaxed);
                });
            }
            send_chunks(
                &permit,
                &list,
                &d.0,
                Duration::from_secs(10),
                &cancel,
                &mut out,
            )
        });
        (r, String::from_utf8(out).unwrap(), t.elapsed(), d)
    }

    #[test]
    fn a_cancel_during_the_stderr_drain_is_nonzero_with_no_row_and_prompt() {
        // the app exits 0 at once but a grandchild keeps stderr open: the drain waits, and a
        // cancel then must not turn into a timing row and success
        let (r, out, took, _d) = cancelled_run(
            "fs-cancel-drain",
            "echo 1 > started; sleep 20 & exit 0",
            false,
        );
        let e = r.unwrap_err();
        assert!(e.downcast_ref::<Cancelled>().is_some(), "{e}");
        assert_eq!(out, "", "no timing row for the interrupted chunk");
        assert!(took < Duration::from_secs(2), "{took:?}");
    }

    #[test]
    fn a_cancel_before_a_chunk_stops_it_from_starting() {
        let (r, out, _, d) = cancelled_run("fs-cancel-pre", "echo 1 > started; exit 0", true);
        assert!(r.unwrap_err().downcast_ref::<Cancelled>().is_some());
        assert_eq!(out, "");
        assert!(
            !d.0.join("started").exists(),
            "a chunk started after the cancel"
        );
    }

    #[test]
    fn sigterm_sets_the_cancel_flag() {
        let f = cancel_flag();
        assert!(!f.load(std::sync::atomic::Ordering::Relaxed));
        // SAFETY: raising a signal whose handler we installed, which only sets the flag.
        unsafe { libc::raise(libc::SIGTERM) };
        assert!(f.load(std::sync::atomic::Ordering::Relaxed));
        f.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn two_runs_with_one_label_and_out_dir_never_share_a_chunk_path() {
        let d = crate::testutil::Scratch::new("fs-reserve");
        let o = Opts {
            label: "x".into(),
            jpeg: "p.jpg".into(),
            id: 'A',
            passes: 1,
            chunk: 120,
            limit: 0,
            out: d.0.join("out"),
            pi: "pi".into(),
            dev: "/dev/ttyACM0".into(),
            pi_tool: "t".into(),
            pi_app_dir: "a".into(),
            i_have_an_antenna: true,
            transmit: true,
            freq_mhz: 915.0,
            power_dbm: 5,
            licence: LicenceOpts::default(),
            dry_run: false,
        };
        let first = reserve_run_dir(&o).unwrap();
        fs::write(first.join("pass1.chunks.txt"), "one").unwrap();
        let e = reserve_run_dir(&o).unwrap_err().to_string();
        assert!(e.contains("exists"), "{e}");
        assert_eq!(
            fs::read_to_string(first.join("pass1.chunks.txt")).unwrap(),
            "one"
        );
        let mut other = o.clone();
        other.label = "y".into();
        assert_ne!(reserve_run_dir(&other).unwrap(), first);
    }

    #[test]
    fn plan_is_printed_without_running() {
        let o = Opts {
            label: "x".into(),
            jpeg: "p.jpg".into(),
            id: 'A',
            passes: 3,
            chunk: 120,
            limit: 0,
            out: "out".into(),
            pi: "pi".into(),
            dev: "/dev/ttyACM0".into(),
            pi_tool: "factory-bench/moq-subghz-bench".into(),
            pi_app_dir: "factory-bench/gps-hat".into(),
            i_have_an_antenna: false,
            transmit: true,
            freq_mhz: 915.0,
            power_dbm: 5,
            licence: LicenceOpts::default(),
            dry_run: true,
        };
        let p = pass_plan(&o, 1, "/tmp/moq-bench.AAA");
        assert_eq!(
            p[0].shell(),
            "scp -q -o BatchMode=yes -- out/x/pass1.chunks.txt pi:/tmp/moq-bench.AAA/pass1.chunks.txt"
        );
        assert!(p[1].shell().starts_with(
            "ssh -o BatchMode=yes -- pi 'factory-bench/moq-subghz-bench factory-send --i-have-an-antenna --transmit --freq-mhz 915 --power-dbm 5 --no-licence --redact-licence --app-dir factory-bench/gps-hat /tmp/moq-bench.AAA/pass1.chunks.txt"
        ), "{}", p[1].shell());
        // two runs on one board share no remote path, and each removes only its own directory
        let (a, b) = (
            pass_plan(&o, 1, "/tmp/moq-bench.AAA"),
            pass_plan(&o, 1, "/tmp/moq-bench.BBB"),
        );
        for (x, y) in a.iter().zip(&b) {
            assert_ne!(x.shell(), y.shell());
            assert!(!x.shell().contains("BBB") && !y.shell().contains("AAA"));
        }
        assert_ne!(
            pass_plan(&o, 2, "/tmp/moq-bench.AAA")[0].shell(),
            a[0].shell(),
            "a distinct chunk file per pass"
        );
        assert!(a
            .iter()
            .all(|p| !p.shell().contains("factory-bench/chunks.txt")));
        assert_eq!(
            cleanup_plan(&o, "/tmp/moq-bench.AAA").shell(),
            "ssh -o BatchMode=yes -- pi 'rm -rf /tmp/moq-bench.AAA'"
        );
        assert_eq!(
            scratch_plan(&o).shell(),
            "ssh -o BatchMode=yes -- pi 'mktemp -d /tmp/moq-bench.XXXXXXXXXX'"
        );
        assert_eq!(
            cap_plan(&o, "bench").shell(),
            "bench stick-cap --dev /dev/ttyACM0 --out out/x/stick"
        );
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    fn station(transmit: bool) -> Station {
        LicenceOpts::default()
            .gate_args(transmit)
            .station()
            .expect("--no-licence reads no file")
    }

    fn d(freq_mhz: f64, power_dbm: i8) -> Declared {
        Declared {
            freq_mhz,
            power_dbm,
        }
    }

    fn opts(transmit: bool, freq_mhz: f64, power_dbm: i8, dry: bool) -> Opts {
        Opts {
            label: "x".into(),
            // never read: the gate refuses first, and a read would say so in the error
            jpeg: "/nonexistent/gate-first.jpg".into(),
            id: 'A',
            passes: 1,
            chunk: 120,
            limit: 0,
            out: "/nonexistent/out".into(),
            pi: "pi".into(),
            dev: "/nonexistent/dev".into(),
            pi_tool: "t".into(),
            pi_app_dir: "a".into(),
            i_have_an_antenna: true,
            transmit,
            freq_mhz,
            power_dbm,
            licence: LicenceOpts::default(),
            dry_run: dry,
        }
    }

    #[test]
    fn the_patched_915_build_passes_with_transmit() {
        assert!(gate(&station(true), d(915.0, 5)).is_ok());
        assert!(gate(&station(true), d(915.0, 2)).is_ok());
    }

    #[test]
    fn the_stock_868_build_is_refused_and_the_message_says_why_and_what_to_do() {
        let e = gate(&station(true), d(868.1, 17)).unwrap_err().to_string();
        assert!(e.contains("refusing to transmit"), "{e}");
        assert!(e.contains("868.1 MHz at up to +17 dBm"), "{e}");
        assert!(e.contains("EU region is not enabled"), "{e}");
        assert!(e.contains("gps-hat-bench.patch"), "{e}");
        assert!(e.contains("--freq-mhz 915.0 --power-dbm 5"), "{e}");
        // the carrier alone is enough to refuse it, whatever power is declared
        assert!(gate(&station(true), d(868.1, 5)).is_err());
    }

    #[test]
    fn no_transmit_flag_no_permit() {
        let e = gate(&station(false), d(915.0, 5)).unwrap_err().to_string();
        assert!(e.contains("--transmit"), "{e}");
    }

    #[test]
    fn power_outside_the_projects_range_is_refused() {
        for p in [-1, 0, 1, 6, 17, 23] {
            let e = gate(&station(true), d(915.0, p)).unwrap_err().to_string();
            assert!(e.contains("2 to 5 dBm"), "{p}: {e}");
        }
    }

    #[test]
    fn bad_and_guarded_carriers_are_refused() {
        for f in [f64::NAN, f64::INFINITY, 0.0, -915.0, 1e9] {
            assert!(gate(&station(true), d(f, 5)).is_err(), "{f}");
        }
        // the project's 0.3 MHz no-go zone around 925.0, and the band edges with the 125 kHz span
        for f in [925.0, 925.2, 902.0, 928.0, 901.0, 929.0, 433.0, 2450.0] {
            assert!(gate(&station(true), d(f, 5)).is_err(), "{f}");
        }
    }

    #[test]
    fn a_real_run_is_refused_before_anything_is_read_or_started() {
        // Each refusal comes before the JPEG is read, the output directory made or ssh started:
        // otherwise the error would be about /nonexistent/gate-first.jpg.
        let e = run(&opts(false, 915.0, 5, false)).unwrap_err().to_string();
        assert!(e.contains("--transmit"), "{e}");
        let e = run(&opts(true, 868.1, 17, false)).unwrap_err().to_string();
        assert!(e.contains("868.1 MHz"), "{e}");
        let e = run(&opts(true, 915.0, 17, false)).unwrap_err().to_string();
        assert!(e.contains("2 to 5 dBm"), "{e}");
    }

    #[test]
    fn a_dry_run_still_says_it_would_be_refused() {
        // Dry runs read the JPEG (to count chunks), so this one fails there, after the gate line.
        let e = run(&opts(true, 868.1, 17, true)).unwrap_err().to_string();
        assert!(e.contains("gate-first.jpg"), "{e}");
    }
}

#[cfg(test)]
mod parity {
    use super::*;
    use crate::testutil::repo;

    /// Re-run the pass analysis over each committed capture, cutting it into passes at the counts
    /// the Python run recorded, and compare with the `summary.json` it wrote.
    #[test]
    fn reproduces_the_committed_factory_runs() {
        let data = repo().join("results/factory-firmware/data");
        let mut checked = 0;
        for e in fs::read_dir(&data).unwrap() {
            let dir = e.unwrap().path();
            let want: Value =
                serde_json::from_str(&fs::read_to_string(dir.join("summary.json")).unwrap())
                    .unwrap();
            let jpeg = fs::read(
                repo()
                    .join("results/css-2015/frames")
                    .join(want["jpeg"].as_str().unwrap()),
            )
            .unwrap();
            let rows = read_packets(&dir.join("stick.packets.csv")).unwrap();
            let chunks = make_chunks(&jpeg, 'A', 120);
            let (n, pkt_len) = (chunks.len(), chunks.iter().map(String::len).max().unwrap());
            assert_eq!(want["chunks_total"], n);
            assert_eq!(want["payload_bytes_per_packet_max"], pkt_len);
            let at = airtime(pkt_len, 7, 125e3, 1, 1, 0, 8.0);
            assert_eq!(want["calc_airtime_per_packet_ms"], round_to(at * 1000.0, 1));
            let limit = if want["chunks_attempted_pass1"] == n {
                0
            } else {
                want["chunks_attempted_pass1"].as_u64().unwrap() as usize
            };
            let mut re = Reassembly::new(b'A', n, limit);
            let (mut before, mut passes) = (0, Vec::new());
            for p in want["passes"].as_array().unwrap() {
                let wall = p["tx_wall_s"].as_f64().unwrap();
                let upto = before
                    + p["received_packets"].as_u64().unwrap() as usize
                    + p["malformed"].as_u64().unwrap() as usize;
                let st = re.pass(
                    p["pass_no"].as_u64().unwrap() as usize,
                    &rows[..upto],
                    before,
                    wall,
                );
                assert_eq!(st.json(), *p, "{}", dir.display());
                before = upto;
                passes.push(st);
            }
            let s = Summary {
                label: want["label"].as_str().unwrap(),
                jpeg_name: want["jpeg"].as_str().unwrap(),
                jpeg: &jpeg,
                chunks_total: n,
                pkt_len,
                at,
                passes: &passes,
                re: &re,
                end_to_end_s: want["end_to_end_s"].as_f64().unwrap(),
                window: first_to_complete(&rows, b'A', n as i64, at),
            }
            .json();
            // The committed files predate the first_heard_to_complete_s key: that one is checked
            // against the analyzer's own window (the committed compare.csv covers it), the rest key
            // for key and in the same order.
            let mut got = s.as_object().unwrap().clone();
            let win = got.shift_remove("first_heard_to_complete_s").unwrap();
            let run = dir.file_name().unwrap().to_str().unwrap();
            let again = crate::analyze::factory_window(&data, run, at)
                .unwrap()
                .map_or(Value::Null, |w| json!(w));
            assert_eq!(win, again, "{run}");
            // The goodput divides by the unrounded end-to-end time, which the file only holds rounded.
            let mut want = want.as_object().unwrap().clone();
            let (a, b) = (
                got.shift_remove("goodput_bit_s").unwrap(),
                want.shift_remove("goodput_bit_s").unwrap(),
            );
            assert!(
                a == b || (a.as_f64().unwrap() - b.as_f64().unwrap()).abs() <= 0.11,
                "{run}: {a} vs {b}"
            );
            assert_eq!(
                serde_json::to_string(&got).unwrap(),
                serde_json::to_string(&want).unwrap(),
                "{run}"
            );
            checked += 1;
        }
        assert!(checked >= 7, "{checked} runs checked");
    }

    #[test]
    fn pretty_json_uses_one_space_like_python() {
        assert_eq!(
            pretty(&json!({"a": [1, 2]})).unwrap(),
            "{\n \"a\": [\n  1,\n  2\n ]\n}"
        );
    }

    #[test]
    fn chunk_counts_over_999_or_packets_over_255_are_refused_before_transmitting() {
        let few = make_chunks(&vec![7u8; 999 * 120], 'A', 120);
        assert_eq!(few.len(), 999);
        check_chunks(&few).unwrap();
        let many = make_chunks(&vec![7u8; 999 * 120 + 1], 'A', 120);
        assert_eq!(many.len(), 1000);
        assert!(check_chunks(&many).unwrap_err().to_string().contains("999"));
        let one_byte = make_chunks(&vec![7u8; 1000], 'A', 1);
        assert!(check_chunks(&one_byte).is_err());
        let fat = make_chunks(&vec![7u8; 400], 'A', 200);
        assert!(check_chunks(&fat).unwrap_err().to_string().contains("255"));
        assert!(check_chunks(&make_chunks(&[], 'A', 120)).is_err());
        // The edge that is accepted: 255 bytes is `A:000/001:` plus 245 hex digits
        check_chunks(&["x".repeat(255)]).unwrap();
        assert!(check_chunks(&["x".repeat(256)]).is_err());
    }

    #[test]
    fn a_hostile_label_or_host_stops_run_before_it_reads_or_runs_anything() {
        let o = |label: &str, pi: &str| Opts {
            label: label.into(),
            jpeg: "/nonexistent.jpg".into(),
            id: 'A',
            passes: 3,
            chunk: 120,
            limit: 0,
            out: "/nonexistent/out".into(),
            pi: pi.into(),
            dev: "/dev/null".into(),
            pi_tool: "t".into(),
            pi_app_dir: "d".into(),
            i_have_an_antenna: true,
            transmit: true,
            freq_mhz: 915.0,
            power_dbm: 5,
            licence: LicenceOpts::default(),
            dry_run: true,
        };
        for label in ["../../x", "/abs", "a/b", "a..b", "", &"l".repeat(65)] {
            let e = run(&o(label, "pi")).unwrap_err().to_string();
            assert!(e.contains("--label"), "{label:?}: {e}");
        }
        for pi in ["-oProxyCommand=x", "a;b", "$(x)", "a\nb"] {
            let e = run(&o("ok", pi)).unwrap_err().to_string();
            assert!(e.contains("--pi"), "{pi:?}: {e}");
        }
    }

    #[test]
    fn parse_chunk_never_panics_on_random_bytes() {
        let mut rng = crate::testutil::Rng(42);
        let seed = b"A:000/002:0102aabb";
        for _ in 0..5000 {
            let v = if rng.below(2) == 0 {
                rng.mutate(seed)
            } else {
                (0..rng.below(40)).map(|_| rng.next() as u8).collect()
            };
            if let Some(c) = parse_chunk(&v, None) {
                let _ = c.bytes();
            }
        }
    }
}
