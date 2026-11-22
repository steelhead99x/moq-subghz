//! `factory-bench`: the factory-vs-factory picture test of `results/factory-firmware/`. The Pi
//! runs the stock Dragino app, once per text chunk of a JPEG, and the stock Electronic Cats
//! receiver on the stick prints what it hears.  The stick capture is the `stick-cap` subcommand.
//!
//! This transmits, so it needs `--i-have-an-antenna` and the serial port of the stick as `--dev`.

use crate::cell::{num, read_dict, round_to};
use crate::exec::{capture, run_ok, Plan};
use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::Args;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
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

/// LoRa airtime of a packet of `pl` bytes, in seconds (SX1276 datasheet formula, explicit header off).
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

#[derive(Args, Debug)]
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
    /// ssh host of the Pi
    #[arg(long, default_value = "pi")]
    pub pi: String,
    /// Serial device of the stick (the receiver sketch prints at 9600 baud). No default: it is opened.
    #[arg(long)]
    pub dev: PathBuf,
    /// This binary on the Pi, built for it (`factory-send` runs there)
    #[arg(long, default_value = "factory-bench/moq-subghz-bench")]
    pub pi_tool: String,
    /// Directory of the stock `dragino_lora_app` on the Pi
    #[arg(long, default_value = "factory-bench/dragino")]
    pub pi_app_dir: String,
    /// Required: the antennas are on both radios
    #[arg(long)]
    pub i_have_an_antenna: bool,
    /// Print the commands and stop
    #[arg(long)]
    pub dry_run: bool,
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

/// The commands one pass runs, in order: copy the chunk list to the Pi, send it there.
pub fn pass_plan(o: &Opts, pass: usize) -> [Plan; 2] {
    let local = o.out.join(&o.label).join(format!("pass{pass}.chunks.txt"));
    [
        Plan::new(
            "scp",
            [
                "-q".to_string(),
                local.display().to_string(),
                format!("{}:factory-bench/chunks.txt", o.pi),
            ],
        ),
        Plan::new(
            "ssh",
            [
                o.pi.clone(),
                format!(
                    "{} factory-send --i-have-an-antenna --app-dir {} factory-bench/chunks.txt",
                    shq(&o.pi_tool),
                    shq(&o.pi_app_dir)
                ),
            ],
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

pub fn run(o: &Opts) -> Result<()> {
    let exe = std::env::current_exe()?.display().to_string();
    let jpeg = fs::read(&o.jpeg).with_context(|| format!("read {}", o.jpeg.display()))?;
    if o.chunk == 0 || !o.id.is_ascii_alphabetic() {
        bail!("--chunk must be above 0 and --id a letter");
    }
    let chunks = make_chunks(&jpeg, o.id, o.chunk);
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
        for p in (1..=o.passes.min(2)).flat_map(|k| pass_plan(o, k)) {
            println!("{}", p.shell());
        }
        return Ok(());
    }
    if !o.i_have_an_antenna {
        bail!("refusing to transmit without --i-have-an-antenna");
    }
    let d = o.out.join(&o.label);
    fs::create_dir_all(&d)?;
    let pkt_len = chunks.iter().map(String::len).max().unwrap_or(0);
    let at = airtime(pkt_len, 7, 125e3, 1, 1, 0, 8.0);
    let cp = cap_plan(o, &exe);
    let mut cap = std::process::Command::new(&cp.prog)
        .args(&cp.args)
        .spawn()
        .with_context(|| format!("start {}", cp.shell()))?;
    let res = bench(o, &d, &jpeg, &chunks, pkt_len, at, &mut cap);
    // SAFETY: signalling our own child.
    unsafe { libc::kill(cap.id() as i32, libc::SIGTERM) };
    let _ = cap.wait();
    res
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
    let t_run = Instant::now();
    let mut re = Reassembly::new(o.id as u8, chunks.len(), o.limit);
    let mut passes = Vec::new();
    for ps in 1..=o.passes {
        let list: Vec<&str> = re.want.iter().map(|&i| chunks[i].as_str()).collect();
        let local = d.join(format!("pass{ps}.chunks.txt"));
        fs::write(&local, list.join("\n") + "\n")?;
        let before = read()?.len();
        let t0 = Instant::now();
        let plan = pass_plan(o, ps);
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
    if !re.complete() {
        bail!(
            "FAILED {}: the picture is not complete after {} passes",
            o.label,
            o.passes
        );
    }
    Ok(())
}

/// The Pi side: send each chunk line with the stock Dragino app, and print `<start> <end> <id:seq>`.
pub fn send_chunks(chunks: &Path, app_dir: &Path, app_timeout: Duration) -> Result<()> {
    use std::process::{Command, Stdio};
    let text = fs::read_to_string(chunks).with_context(|| format!("read {}", chunks.display()))?;
    let epoch = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    };
    for line in text.lines() {
        let s = epoch();
        let mut child = Command::new("./dragino_lora_app")
            .args(["sender", line])
            .current_dir(app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("start ./dragino_lora_app in {}", app_dir.display()))?;
        // The stock app does not exit on its own after the send: the same cut-off as `timeout 1.3`.
        let t0 = Instant::now();
        while child.try_wait()?.is_none() {
            if t0.elapsed() >= app_timeout {
                child.kill()?;
                child.wait()?;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let head: Vec<&str> = line.splitn(3, ':').take(2).collect();
        println!("{s:.9} {:.9} {}", epoch(), head.join(":"));
    }
    Ok(())
}

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
            pi_app_dir: "factory-bench/dragino".into(),
            i_have_an_antenna: false,
            dry_run: true,
        };
        let p = pass_plan(&o, 1);
        assert_eq!(
            p[0].shell(),
            "scp -q out/x/pass1.chunks.txt pi:factory-bench/chunks.txt"
        );
        assert!(p[1].shell().starts_with(
            "ssh pi 'factory-bench/moq-subghz-bench factory-send --i-have-an-antenna"
        ));
        assert_eq!(
            cap_plan(&o, "bench").shell(),
            "bench stick-cap --dev /dev/ttyACM0 --out out/x/stick"
        );
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
                    .join("results/lora-2015/frames")
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
}
