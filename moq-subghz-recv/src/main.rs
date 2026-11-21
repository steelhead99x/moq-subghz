//! moq-subghz-recv: CatWAN USB serial (`R <t> <rssi> <snr> <hex>` lines from the sx1276-rs
//! catwan-fw) -> reassembly + XOR FEC -> MoQ group gate -> H.264 Annex-B to
//! (a) ffplay for live display, (b) a recording, (c) an ffmpeg decoder whose output
//! frames are scanned for the burned-in barcode = capture->decoded latency, and
//! (d) optionally `moq-subghz-publish`, which republishes the objects as real MoQ.
#[cfg(target_os = "linux")]
mod radio;

use anyhow::{bail, Context, Result};

static RADIO_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
use clap::Parser;
use moq_subghz_wire::{
    parse_fsk_profile, parse_r_line, read_line_limited, seal, stamp, Event, FrameRange, GroupGate,
    Line, ObjHdr, Reassembler, MAX_SERIAL_LINE,
};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// CatWAN serial port (macOS: /dev/cu.usbmodemXXXX)
    #[arg(long)]
    port: Option<String>,
    /// Receive on the local SX1276 (Pi HAT) instead of a CatWAN port (reverse direction)
    #[arg(long)]
    radio: bool,
    /// Replay a file of `R` lines instead of a port (offline test)
    #[arg(long)]
    replay: Option<String>,
    /// With --replay: honour the line timestamps (real-time replay)
    #[arg(long)]
    paced: bool,
    /// FSK BITRATE:FDEV:PREAMBLE[:x]  (must match the sender)
    #[arg(long, default_value = "300000:100000:6")]
    profile: String,
    #[arg(long, default_value_t = 915.0)]
    freq: f64,
    #[arg(long, default_value_t = 330.0)]
    secs: f64,
    /// Output directory (rec.h264, objects.jsonl, decoded.jsonl, summary.json)
    #[arg(long, default_value = "run")]
    out: String,
    /// Live window with ffplay
    #[arg(long)]
    play: bool,
    /// Decode with ffmpeg and read the barcode (needs --size)
    #[arg(long)]
    decode: bool,
    #[arg(long, default_value = "320x240")]
    size: String,
    /// Sender clock minus receiver clock, ms, as printed by --clock-check (latency = t_recv - t_cap_sender + offset)
    #[arg(long, default_value_t = 0.0)]
    clock_offset_ms: f64,
    /// Feed every complete object to the decoder, even inside a broken group
    #[arg(long)]
    no_group_gate: bool,
    /// Measure the sender's clock offset over one persistent ssh session to HOST, then exit
    #[arg(long)]
    clock_check: Option<String>,
    /// Log every serial line (R lines as length only) with host timestamps to OUT/serial.log
    #[arg(long)]
    raw_log: bool,
    /// Decrypt objects with rind AEAD using the hex key in this file (must match the sender)
    #[arg(long)]
    key_file: Option<String>,
    /// chacha20poly1305 | aes128gcm | aes256gcm
    #[arg(long, default_value = "chacha20poly1305")]
    cipher: String,
    /// Stall threshold, ms (gap between consecutive displayed frames)
    #[arg(long, default_value_t = 250.0)]
    stall_ms: f64,
    /// Republish the delivered objects as a real MoQ broadcast on this relay URL, through
    /// `moq-subghz-publish` (see docs/MOQ.md). E.g. http://localhost:4443/anon. Give
    /// `--publish` with no URL to read it from MOQ_SUBGHZ_RELAY, which a token-bearing URL
    /// needs: argv is visible to every local user, the environment is not.
    #[arg(long, num_args = 0..=1, default_missing_value = "")]
    publish: Option<String>,
    /// Broadcast name for --publish
    #[arg(long, default_value = "moq-subghz")]
    publish_name: String,
    /// The moq-subghz-publish binary for --publish
    #[arg(long, default_value = "moq-subghz-publish")]
    publish_bin: String,
}

fn unix_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

fn open_serial(path: &str) -> Result<File> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(path)
        .with_context(|| format!("open {path}"))?;
    // SAFETY: plain termios calls on a valid fd.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(f.as_raw_fd(), &mut t) != 0 {
            bail!("tcgetattr failed");
        }
        libc::cfmakeraw(&mut t);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        libc::cfsetspeed(&mut t, libc::B115200);
        libc::tcsetattr(f.as_raw_fd(), libc::TCSANOW, &t);
        libc::tcflush(f.as_raw_fd(), libc::TCIOFLUSH);
    }
    Ok(f)
}

/// A child process fed through a channel, so a slow consumer never blocks the radio loop.
/// What it cannot take is dropped and counted, since a dropped AU breaks that consumer's
/// picture (and, for the decoder, its latency samples) until the next keyframe.
struct Sink {
    tx: Option<mpsc::SyncSender<Vec<u8>>>,
    th: Option<std::thread::JoinHandle<u64>>,
    child: Option<Child>,
    /// Sends refused because the channel was full or the writer had stopped.
    refused: std::cell::Cell<u64>,
}
impl Sink {
    fn new(mut stdin: ChildStdin, child: Option<Child>) -> Sink {
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(256);
        let th = std::thread::spawn(move || {
            let mut dropped = 0;
            for b in rx {
                if stdin.write_all(&b).and_then(|_| stdin.flush()).is_err() {
                    dropped += 1;
                }
            }
            dropped
        });
        Sink {
            tx: Some(tx),
            th: Some(th),
            child,
            refused: std::cell::Cell::new(0),
        }
    }
    fn send(&self, b: &[u8]) {
        if let Some(t) = &self.tx {
            if t.try_send(b.to_vec()).is_err() {
                self.refused.set(self.refused.get() + 1);
            }
        }
    }
    /// Close the input, wait for the child, and report how many AUs it never got (refused
    /// by a full channel, or failed to write once the child stopped reading) and whether
    /// the child exited cleanly on its own.
    fn close(&mut self, kill_after: Duration) -> Closed {
        // Close the input and give the writer `kill_after` to drain the backlog (up to 256
        // AUs for a slow decoder), then the child `kill_after` to finish, then kill it, and
        // only then join the writer: a child that stopped reading leaves the writer blocked
        // on a full pipe until the child is gone.
        self.tx.take();
        let t0 = Instant::now();
        while self.th.as_ref().is_some_and(|t| !t.is_finished()) && t0.elapsed() < kill_after {
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut exited_ok = true;
        if let Some(mut c) = self.child.take() {
            let t0 = Instant::now();
            exited_ok = loop {
                match c.try_wait() {
                    Ok(Some(st)) => break st.success(),
                    Ok(None) if t0.elapsed() < kill_after => {
                        std::thread::sleep(Duration::from_millis(50))
                    }
                    _ => {
                        let _ = c.kill();
                        let _ = c.wait();
                        break false;
                    }
                }
            };
        }
        let mut dropped = self.refused.get();
        if let Some(t) = self.th.take() {
            match t.join() {
                Ok(n) => dropped += n,
                Err(_) => {
                    eprintln!("a sink's writer thread panicked; its drop count is incomplete")
                }
            }
        }
        Closed { dropped, exited_ok }
    }
}

/// How a sink ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Closed {
    dropped: u64,
    exited_ok: bool,
}

const AUD: [u8; 6] = [0, 0, 0, 1, 0x09, 0xf0];

/// Bytes for a live decoder: the access unit minus its leading AUD, followed by the AUD
/// of the *next* access unit. ffmpeg's H.264 parser only closes a frame when it sees the
/// next start of frame, so this lets it decode each frame on arrival instead of one frame
/// (66 ms at 15 fps) later.
fn live_bytes(au: &[u8]) -> Vec<u8> {
    let n = moq_subghz_wire::annexb::nals(au);
    let body = match n.as_slice() {
        [(0, 9), (next, _), ..] => &au[*next..],
        [(0, 9)] => &[][..],
        _ => au,
    };
    [body, &AUD[..]].concat()
}

/// Escape a string for a JSON string literal. Measured stats lines have no
/// quotes, so this leaves them unchanged and only stops a hostile serial
/// peer from breaking `summary.json`.
fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            c if c.is_control() => o.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => o.push(c),
        }
    }
    o
}

/// A JSON number, or `null` when there is no value. An empty latency list gives NaN,
/// which JSON cannot carry (a run without `--decode` used to write invalid JSON).
fn jnum(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.1}")
    } else {
        "null".into()
    }
}

/// Like [`jnum`], with five decimals for a ratio.
fn jnum5(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.5}")
    } else {
        "null".into()
    }
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn stalls(times_us: &[u64], thr_ms: f64) -> (u32, f64) {
    let mut n = 0;
    let mut worst = 0f64;
    for w in times_us.windows(2) {
        let g = w[1].saturating_sub(w[0]) as f64 / 1000.0;
        worst = worst.max(g);
        if g > thr_ms {
            n += 1;
        }
    }
    (n, worst)
}

/// Ping-pong `date` over one ssh session; offset = remote - local midpoint of the
/// lowest-RTT exchange (NTP-style).
fn clock_check(host: &str) -> Result<()> {
    if host.is_empty() || host.starts_with('-') {
        bail!("--clock-check: {host:?} is not a host name");
    }
    let mut c = Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "--",
            host,
            "while read l; do date +%s%N; done",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut si = c.stdin.take().unwrap();
    let mut so = BufReader::new(c.stdout.take().unwrap());
    let mut best = (f64::MAX, 0f64);
    let mut l = String::new();
    for i in 0..40 {
        let t1 = unix_us() as f64;
        si.write_all(b"x\n")?;
        si.flush()?;
        l.clear();
        so.read_line(&mut l)?;
        let t2 = unix_us() as f64;
        let remote = l.trim().parse::<f64>()? / 1000.0;
        let (rtt, off) = ((t2 - t1) / 1000.0, (remote - (t1 + t2) / 2.0) / 1000.0);
        if i > 2 && rtt < best.0 {
            best = (rtt, off);
        }
    }
    drop(si);
    let _ = c.kill();
    let _ = c.wait();
    println!(
        "{{\"host\":\"{}\",\"best_rtt_ms\":{:.2},\"remote_minus_local_ms\":{:.2}}}",
        json_escape(host),
        best.0,
        best.1
    );
    Ok(())
}

fn main() -> Result<()> {
    let a = Args::parse();
    if let Some(h) = &a.clock_check {
        return clock_check(h);
    }
    if !(902.0..=928.0).contains(&a.freq) || (a.freq - 925.0).abs() < 0.3 {
        bail!("--freq must be inside 902-928 MHz and not 925.0");
    }
    let publish_url = match a.publish.as_deref() {
        None => None,
        Some("") => Some(
            std::env::var("MOQ_SUBGHZ_RELAY")
                .ok()
                .filter(|u| !u.is_empty())
                .context("--publish with no URL reads MOQ_SUBGHZ_RELAY, which is unset")?,
        ),
        Some(u) if u.contains('?') => bail!(
            "--publish URL carries a query (a token?); put it in MOQ_SUBGHZ_RELAY and pass --publish alone"
        ),
        Some(u) => Some(u.to_string()),
    };
    // Both are printed into summary.json as numbers; NaN or infinity would make it invalid.
    if !(a.clock_offset_ms.is_finite() && a.stall_ms.is_finite() && a.stall_ms > 0.0) {
        bail!("--clock-offset-ms must be finite and --stall-ms finite and positive");
    }
    let (w, h): (usize, usize) = {
        let (x, y) = a.size.split_once('x').context("--size WxH")?;
        (x.parse()?, y.parse()?)
    };
    // Only the decoder uses --size. read_barcode needs blocks at least 2 px wide and the
    // full bar height, and a zero size would spin the decode thread on empty reads.
    if a.decode && (w < 40 || h < stamp::BAR_ROWS) {
        bail!(
            "--size must be at least 40x{} with --decode",
            stamp::BAR_ROWS
        );
    }
    std::fs::create_dir_all(&a.out)?;
    let prof = parse_fsk_profile(&a.profile).map_err(|e| anyhow::anyhow!(e))?;
    let (br, fdev, pre, unchecked) = (prof.bitrate, prof.fdev, prof.preamble, prof.unchecked);

    // ---- line source
    let (ltx, lrx) = mpsc::channel::<(u64, String)>();
    let mut replay_th: Option<std::thread::JoinHandle<std::io::Result<()>>> = None;
    let mut port_w = None;
    if let Some(port) = &a.port {
        let f = open_serial(port)?;
        let mut wr = f.try_clone()?;
        let rd = f;
        std::thread::spawn(move || {
            let mut r = BufReader::with_capacity(1 << 16, rd);
            loop {
                match read_line_limited(&mut r, MAX_SERIAL_LINE) {
                    Ok(Some(Line::Data(l))) => {
                        let s = String::from_utf8_lossy(&l).trim().to_string();
                        if s.is_empty() {
                            continue;
                        }
                        if ltx.send((unix_us(), s)).is_err() {
                            break;
                        }
                    }
                    Ok(Some(Line::TooLong)) => {
                        let _ = ltx.send((unix_us(), "X line-too-long".into()));
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        });
        let cmd = |wr: &mut File, c: &str, want: &str| -> Result<String> {
            wr.write_all(format!("{c}\n").as_bytes())?;
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(3) {
                if let Ok((_, l)) = lrx.recv_timeout(Duration::from_millis(100)) {
                    if l.starts_with(want) {
                        return Ok(l);
                    }
                }
            }
            bail!("no {want:?} reply to {c:?}")
        };
        cmd(&mut wr, "S", "OK S")?;
        let freq_hz = (a.freq * 1e6).round() as u32;
        let r = cmd(
            &mut wr,
            &format!("F {freq_hz} {br} {fdev} 5 {pre} 1 {}", unchecked as u8),
            "OK F",
        )?;
        if !r.ends_with("None") {
            bail!("CatWAN config error: {r}");
        }
        cmd(&mut wr, "R", "OK R")?;
        eprintln!("CatWAN listening: {r}");
        port_w = Some(wr);
    } else if a.radio {
        #[cfg(target_os = "linux")]
        {
            let cfg = sx1276::fsk::FskConfig {
                bitrate_bps: br,
                fdev_hz: fdev,
                preamble_bytes: pre,
                unchecked,
                // fsk_poll_long drains the FIFO mid-packet: keep failed packets whole
                crc_auto_clear: false,
                ..Default::default()
            };
            cfg.validate()
                .map_err(|x| anyhow::anyhow!("fsk config: {x:?}"))?;
            let mut r = radio::Radio::open(8_000_000, (a.freq * 1e6).round() as u32, &cfg, 2)?;
            r.start_rx()?;
            let stall = 36 * 8_000_000 / br;
            eprintln!("Pi SX1276 listening (FSK {br} bit/s)");
            std::thread::spawn(move || {
                let t0 = Instant::now();
                let mut st = sx1276::FskLongRx::default();
                while !RADIO_STOP.load(std::sync::atomic::Ordering::Relaxed) {
                    let now = t0.elapsed().as_micros() as u32;
                    let line = match r.poll(&mut st, now, stall) {
                        Ok(Some((p, rssi))) => format!(
                            "R {now} {rssi} 0 {}",
                            p.iter().map(|b| format!("{b:02x}")).collect::<String>()
                        ),
                        Ok(None) => continue,
                        Err(x) => format!("X {now} {x}"),
                    };
                    if ltx.send((unix_us(), line)).is_err() {
                        break;
                    }
                }
                r.idle();
                RADIO_STOP.store(false, std::sync::atomic::Ordering::Relaxed);
            });
        }
        #[cfg(not(target_os = "linux"))]
        bail!("--radio needs Linux");
    } else if let Some(rp) = &a.replay {
        let mut f = BufReader::new(File::open(rp)?);
        let paced = a.paced;
        replay_th = Some(std::thread::spawn(move || -> std::io::Result<()> {
            let t0 = Instant::now();
            loop {
                let l = match read_line_limited(&mut f, MAX_SERIAL_LINE)? {
                    Some(Line::Data(buf)) => String::from_utf8_lossy(&buf).trim().to_string(),
                    Some(Line::TooLong) => "X line-too-long".to_string(),
                    None => return Ok(()),
                };
                if l.is_empty() {
                    continue;
                }
                if paced {
                    if let Some(us) = l.split(' ').nth(1).and_then(|x| x.parse::<u64>().ok()) {
                        let due = Duration::from_micros(us);
                        if let Some(w) = due.checked_sub(t0.elapsed()) {
                            std::thread::sleep(w);
                        }
                    }
                }
                if ltx.send((unix_us(), l)).is_err() {
                    return Ok(());
                }
            }
        }));
    } else {
        bail!("need --port or --replay");
    }

    // ---- sinks
    let mut raw = if a.raw_log {
        Some(BufWriter::new(File::create(format!(
            "{}/serial.log",
            a.out
        ))?))
    } else {
        None
    };
    let mut rec = BufWriter::new(File::create(format!("{}/rec.h264", a.out))?);
    let mut objlog = BufWriter::new(File::create(format!("{}/objects.jsonl", a.out))?);
    // Single-threaded decode: frame threading would hold back (threads-1) frames.
    // No `-fflags nobuffer`: it discards the packets read while the stream is analysed,
    // which cost every run its first GOP. Without it the decoder's steady-state latency
    // measured the same; ffplay shares these flags and was not measured.
    let low = [
        "-flags",
        "low_delay",
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        "-threads",
        "1",
    ];
    let mut play = if a.play {
        let mut c = Command::new("ffplay")
            .args(["-hide_banner", "-loglevel", "error"])
            .args(low)
            .args([
                "-framedrop",
                "-sync",
                "ext",
                "-window_title",
                "SX1276 FSK live video (moq-subghz)",
                "-x",
                "640",
                "-y",
                "480",
                "-f",
                "h264",
                "-i",
                "pipe:0",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn ffplay")?;
        let si = c.stdin.take().unwrap();
        Some(Sink::new(si, Some(c)))
    } else {
        None
    };
    // Same objects the decoder gets, one record each. stderr stays on the terminal so
    // the publisher's connection errors are visible.
    let mut publish = match &publish_url {
        Some(url) => {
            // The URL goes in the child's environment, never its argv.
            let mut c = Command::new(&a.publish_bin)
                .args(["--name", &a.publish_name, "--size", &a.size])
                .env("MOQ_SUBGHZ_RELAY", url)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .spawn()
                .with_context(|| format!("spawn {}", a.publish_bin))?;
            let si = c.stdin.take().unwrap();
            Some(Sink::new(si, Some(c)))
        }
        None => None,
    };
    let mut dec_th = None;
    let mut dec = if a.decode {
        let mut c = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error"])
            .args(low)
            .args([
                "-f",
                "h264",
                "-i",
                "pipe:0",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
                "-s",
                &a.size,
                "-fps_mode",
                "passthrough",
                "-flush_packets",
                "1",
                "pipe:1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("spawn ffmpeg decoder")?;
        let si = c.stdin.take().unwrap();
        let mut so = c.stdout.take().unwrap();
        let path = format!("{}/decoded.jsonl", a.out);
        let mut lg = BufWriter::new(File::create(&path).with_context(|| format!("create {path}"))?);
        let off = a.clock_offset_ms;
        dec_th = Some(std::thread::spawn(
            move || -> (Vec<u64>, Vec<(u64, f64)>, u64) {
                let mut buf = vec![0u8; w * h];
                let (mut times, mut lat, mut unread) = (Vec::new(), Vec::new(), 0u64);
                while so.read_exact(&mut buf).is_ok() {
                    let t = unix_us();
                    times.push(t);
                    match stamp::read_barcode(&buf, w, h) {
                        Some(v) => {
                            let now_ms = (t / 1000) as u32;
                            let l = now_ms.wrapping_sub(v) as i32 as f64 + off;
                            lat.push((t, l));
                            let _ = writeln!(
                                lg,
                                "{{\"t_us\":{t},\"cap_ms_lo\":{v},\"lat_ms\":{l:.1}}}"
                            );
                        }
                        None => {
                            unread += 1;
                            let _ = writeln!(lg, "{{\"t_us\":{t},\"cap_ms_lo\":null}}");
                        }
                    }
                }
                (times, lat, unread)
            },
        ));
        Some(Sink::new(si, Some(c)))
    } else {
        None
    };

    for s in play.iter().chain(dec.iter()) {
        s.send(&AUD);
    }
    let mut sealer = match &a.key_file {
        Some(kf) => {
            let c = seal::Cipher::parse(&a.cipher).context("--cipher")?;
            Some(seal::open_sealer(std::path::Path::new(kf), c).map_err(|e| anyhow::anyhow!(e))?)
        }
        None => None,
    };
    let (mut auth_fail, mut open_s) = (0u64, 0f64);
    // ---- main loop
    let mut re = Reassembler::new();
    let mut gate = GroupGate::default();
    let t0 = Instant::now();
    let (mut ok_objs, mut lost_objs, mut delivered_bytes, mut au_bytes_all) =
        (0u64, 0u64, 0u64, 0u64);
    let (mut rx_err, mut first_frame, mut last_frame) = (0u64, None::<u64>, 0u64);
    let mut obj_lat = Vec::new();
    let mut rx_bytes = 0u64;
    let mut obj_times = Vec::new();
    let mut rssi = Vec::new();
    let mut last_rx = Instant::now();
    // Source frame numbers: the confirmed range sets the expected count, and loss counts
    // distinct frame numbers delivered inside it, so it is never negative or inflated.
    let mut frame_range = FrameRange::default();
    let mut delivered_frames = std::collections::HashSet::new();
    let mut handle = |e: Event, t: u64| -> Result<()> {
        match e {
            Event::Object {
                group,
                obj,
                data,
                recovered,
            } => {
                let data = match sealer.as_mut() {
                    Some(sl) => {
                        let t0 = Instant::now();
                        let r = sl.open(group, obj, &data);
                        open_s += t0.elapsed().as_secs_f64();
                        match r {
                            Some(d) => d,
                            None => {
                                auth_fail += 1;
                                lost_objs += 1;
                                gate.lost(group, obj);
                                writeln!(objlog, "{{\"t_us\":{t},\"group\":{group},\"obj\":{obj},\"lost\":true,\"auth_fail\":true}}")?;
                                return Ok(());
                            }
                        }
                    }
                    None => data,
                };
                let Some((oh, au)) = ObjHdr::decode(&data) else {
                    return Ok(());
                };
                ok_objs += 1;
                au_bytes_all += au.len() as u64;
                frame_range.observe(oh.frame_no);
                let pass = a.no_group_gate || gate.accept(group, obj);
                let lat = (t as f64 - oh.cap_us as f64) / 1000.0 + a.clock_offset_ms;
                writeln!(objlog, "{{\"t_us\":{t},\"group\":{group},\"obj\":{obj},\"frame_no\":{},\"key\":{},\"au_bytes\":{},\"recovered\":{recovered},\"passed\":{pass},\"enc_ms\":{:.1},\"lat_ms\":{lat:.1}}}", oh.frame_no, oh.key, au.len(), oh.enc_us as f64 / 1000.0)?;
                if pass {
                    delivered_frames.insert(oh.frame_no);
                    delivered_bytes += au.len() as u64;
                    obj_lat.push(lat);
                    obj_times.push(t);
                    rec.write_all(au)?;
                    let live = live_bytes(au);
                    if let Some(s) = &play {
                        s.send(&live);
                    }
                    if let Some(s) = &dec {
                        s.send(&live);
                    }
                    if let Some(s) = &publish {
                        let r = moq_subghz_wire::objstream::Record {
                            group,
                            obj,
                            key: oh.key,
                            frame_no: oh.frame_no,
                            cap_us: oh.cap_us,
                            payload: au.to_vec(),
                        };
                        s.send(&r.encode());
                    }
                }
            }
            Event::Lost {
                group,
                obj,
                have,
                need,
            } => {
                lost_objs += 1;
                gate.lost(group, obj);
                writeln!(objlog, "{{\"t_us\":{t},\"group\":{group},\"obj\":{obj},\"lost\":true,\"have\":{have},\"need\":{need}}}")?;
            }
        }
        Ok(())
    };
    let mut port_closed = false;
    while t0.elapsed().as_secs_f64() < a.secs {
        match lrx.recv_timeout(Duration::from_millis(20)) {
            Ok((t, l)) => {
                if let Some(r) = raw.as_mut() {
                    match l.rsplit_once(' ') {
                        Some((head, hx)) if l.starts_with("R ") => writeln!(
                            r,
                            "{t} {head} len={} seq={}",
                            hx.len() / 2,
                            hx.get(0..4).unwrap_or("")
                        )?,
                        _ => writeln!(r, "{t} {l}")?,
                    }
                }
                if l.starts_with("R ") {
                    match parse_r_line(&l) {
                        Some(r) => {
                            if let Some(v) = r.rssi {
                                rssi.push(v as f64);
                            }
                            first_frame.get_or_insert(t);
                            rx_bytes += r.frame.len() as u64;
                            last_frame = t;
                            last_rx = Instant::now();
                            for e in re.push(&r.frame) {
                                handle(e, t)?;
                            }
                        }
                        // garbled line from the serial link: not a frame
                        None => rx_err += 1,
                    }
                } else if l.starts_with("X ") {
                    rx_err += 1;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if a.replay.is_none() {
                    // Keep what we have: write the summary, then exit non-zero.
                    eprintln!("serial port closed, stopping");
                    port_closed = true;
                }
                break;
            }
        }
        if last_rx.elapsed() > Duration::from_millis(150) {
            if let Some(e) = re.flush() {
                handle(e, unix_us())?;
            }
        }
    }
    if let Some(e) = re.finish() {
        handle(e, unix_us())?;
    }
    // A replay reader that stopped on an error must not pass for a clean end of file. One
    // still running was cut off by --secs, which is not an error.
    let input_error = match replay_th.take().filter(|t| t.is_finished()).map(|t| t.join()) {
        Some(Ok(Err(e))) => Some(e.to_string()),
        Some(Err(_)) => Some("the replay reader panicked".to_string()),
        _ => None,
    };
    let mut stats_line = String::new();
    if a.radio {
        // stop the RX thread and put the SX1276 back to sleep (it clears the flag when done)
        RADIO_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
        let t = Instant::now();
        while RADIO_STOP.load(std::sync::atomic::Ordering::Relaxed)
            && t.elapsed() < Duration::from_secs(1)
        {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    if let Some(mut wr) = port_w.filter(|_| !port_closed) {
        if let Err(e) = wr.write_all(b"S\n") {
            eprintln!("stopping the CatWAN failed: {e}");
        }
        let t = Instant::now();
        while t.elapsed() < Duration::from_secs(2) {
            if let Ok((_, l)) = lrx.recv_timeout(Duration::from_millis(100)) {
                if l.starts_with("OK S") {
                    stats_line = l;
                    break;
                }
            }
        }
    }
    rec.flush()?;
    objlog.flush()?;
    if let Some(r) = raw.as_mut() {
        r.flush()?;
    }
    let play_dropped = play
        .as_mut()
        .map(|s| s.close(Duration::from_secs(3)).dropped);
    let publish_closed = publish.as_mut().map(|s| s.close(Duration::from_secs(5)));
    let publish_dropped = publish_closed.map(|c| c.dropped);
    let publish_failed = publish_closed.is_some_and(|c| c.dropped > 0 || !c.exited_ok);
    let mut decode_dropped = None;
    let (mut dtimes, dlat_t, unread) = if let Some(mut s) = dec.take() {
        decode_dropped = Some(s.close(Duration::from_secs(5)).dropped);
        dec_th.take().unwrap().join().unwrap()
    } else {
        (Vec::new(), Vec::new(), 0)
    };
    let opt = |v: Option<u64>| v.map_or("null".to_string(), |n| n.to_string());
    // The CLI decoder holds one frame until more input (or EOF) arrives; the frame it
    // flushes when we close its stdin at the end of the run is not a stall or latency
    // sample, so ignore decoded frames that appear >1 s after the last object.
    let cutoff = obj_times
        .last()
        .copied()
        .unwrap_or(u64::MAX)
        .saturating_add(1_000_000);
    let flushed = dtimes.iter().filter(|&&t| t > cutoff).count();
    dtimes.retain(|&t| t <= cutoff);
    let dlat: Vec<f64> = dlat_t
        .iter()
        .filter(|(t, _)| *t <= cutoff)
        .map(|x| x.1)
        .collect();
    let span = first_frame
        .map(|f| (last_frame - f) as f64 / 1e6)
        .unwrap_or(0.0);
    // CPU over the whole run (children = ffmpeg decoder / ffplay, after they exited)
    let wall = t0.elapsed().as_secs_f64().max(1e-9);
    let (ost, oworst) = stalls(&obj_times, a.stall_ms);
    let (dst, dworst) = stalls(&dtimes, a.stall_ms);
    let frames_expected = frame_range.expected();
    let frames_delivered = delivered_frames
        .iter()
        .filter(|&&f| frame_range.contains(f))
        .count();
    let video_frame_loss = if frames_expected == 0 {
        f64::NAN
    } else {
        1.0 - frames_delivered as f64 / frames_expected as f64
    };
    let s = format!(
        "{{\"role\":\"recv\",\"profile\":\"{}\",\"span_s\":{span:.2},\"radio_frames_ok\":{},\"radio_frames_missing\":{},\"radio_frames_bad\":{},\"radio_frames_late\":{},\"radio_frames_stray\":{},\"radio_frames_stale\":{},\"radio_frame_loss\":{:.5},\"rx_err_lines\":{rx_err},\"fec_recovered\":{},\"objects_ok\":{ok_objs},\"objects_lost\":{lost_objs},\"objects_delivered\":{},\"frames_expected\":{frames_expected},\"frames_delivered\":{frames_delivered},\"frame_no_rejected\":{},\"video_frame_loss\":{},\"groups_cut\":{},\"objects_skipped_by_gate\":{},\"delivered_video_bps\":{:.0},\"received_video_bps\":{:.0},\"obj_lat_ms\":{{\"p50\":{},\"p95\":{},\"max\":{}}},\"decoded_frames\":{},\"barcode_unread\":{unread},\"g2g_lat_ms\":{{\"p50\":{},\"p95\":{},\"max\":{}}},\"stalls_obj\":{ost},\"worst_gap_obj_ms\":{oworst:.0},\"stalls_decoded\":{dst},\"worst_gap_decoded_ms\":{dworst:.0},\"stall_threshold_ms\":{},\"rssi_mean\":{},\"clock_offset_ms\":{},\"cipher\":\"{}\",\"auth_fail\":{auth_fail},\"open_us_per_object\":{:.1},\"radio_goodput_bps\":{:.0},\"catwan\":\"{}\",\"decoded_flushed_at_end\":{flushed},\"cpu_pct_recv\":{:.1},\"cpu_pct_children\":{:.1},\"cpu_cores\":{},\"port_closed\":{port_closed},\"play_dropped_aus\":{},\"decode_dropped_aus\":{},\"publish_dropped_objects\":{},\"publish_ok\":{},\"input_error\":{}}}",
        a.profile, re.frames_ok, re.frames_missing, re.frames_bad, re.frames_late, re.frames_stray, re.frames_stale,
        re.frames_missing as f64 / (re.frames_ok + re.frames_missing).max(1) as f64,
        re.fec_recovered, obj_times.len(),
        frame_range.rejected,
        jnum5(video_frame_loss),
        gate.groups_cut, gate.objects_skipped,
        delivered_bytes as f64 * 8.0 / span.max(1e-9), au_bytes_all as f64 * 8.0 / span.max(1e-9),
        jnum(pct(&mut obj_lat.clone(), 0.5)), jnum(pct(&mut obj_lat.clone(), 0.95)), jnum(obj_lat.iter().cloned().fold(f64::NAN, f64::max)),
        dtimes.len(), jnum(pct(&mut dlat.clone(), 0.5)), jnum(pct(&mut dlat.clone(), 0.95)), jnum(dlat.iter().copied().fold(f64::NAN, f64::max)),
        a.stall_ms, jnum(if rssi.is_empty() { f64::NAN } else { rssi.iter().sum::<f64>() / rssi.len() as f64 }), a.clock_offset_ms,
        sealer.as_ref().map(|x| x.cipher.name()).unwrap_or("none"),
        1e6 * open_s / ok_objs.max(1) as f64,
        rx_bytes as f64 * 8.0 / span.max(1e-9),
        json_escape(&stats_line),
        100.0 * cpu_s(libc::RUSAGE_SELF) / wall, 100.0 * cpu_s(libc::RUSAGE_CHILDREN) / wall,
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        opt(play_dropped), opt(decode_dropped), opt(publish_dropped),
        publish_closed.map_or("null".to_string(), |_| (!publish_failed).to_string()),
        input_error.as_ref().map_or("null".to_string(), |e| format!("\"{}\"", json_escape(e))),
    );
    std::fs::write(format!("{}/summary.json", a.out), &s)?;
    println!("{s}");
    // The recording is written either way; a failed input or publish still fails the run.
    if let Some(e) = &input_error {
        eprintln!("reading the input failed: {e}");
    }
    if publish_failed {
        eprintln!("publishing failed: the publisher exited with an error or missed objects");
    }
    if port_closed || input_error.is_some() || publish_failed {
        std::process::exit(1);
    }
    Ok(())
}

#[allow(deprecated)]
fn cpu_s(who: i32) -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: getrusage writes into a zeroed rusage we own.
    unsafe { libc::getrusage(who, &mut ru) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

#[cfg(test)]
mod tests {
    use super::{jnum, json_escape, Sink};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    fn child(cmd: &str, args: &[&str]) -> Sink {
        let mut c = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let si = c.stdin.take().unwrap();
        Sink::new(si, Some(c))
    }

    #[test]
    fn sink_counts_aus_a_stalled_child_never_got() {
        // `sleep` never reads: the pipe fills, then the 256-deep channel, then sends are refused
        let mut s = child("sleep", &["10"]);
        let au = vec![0u8; 100_000];
        for _ in 0..300 {
            s.send(&au);
        }
        let c = s.close(Duration::from_millis(100));
        assert!(c.dropped >= 300 - 256 - 2, "{c:?}");
        assert!(!c.exited_ok, "a child killed at the deadline did not exit cleanly");
    }

    #[test]
    fn sink_counts_writes_to_a_child_that_exited() {
        let mut s = child("true", &[]);
        std::thread::sleep(Duration::from_millis(200)); // let it exit
        for _ in 0..10 {
            s.send(b"au");
        }
        assert_eq!(s.close(Duration::from_millis(100)).dropped, 10);
    }

    #[test]
    fn sink_reports_a_child_that_exited_with_an_error() {
        let mut s = child("false", &[]);
        std::thread::sleep(Duration::from_millis(200));
        assert!(!s.close(Duration::from_millis(100)).exited_ok);
        let mut ok = child("cat", &[]);
        ok.send(b"x");
        let c = ok.close(Duration::from_secs(2));
        assert_eq!((c.dropped, c.exited_ok), (0, true));
    }

    #[test]
    fn jnum_writes_null_for_missing_values() {
        assert_eq!(jnum(12.34), "12.3");
        assert_eq!(jnum(f64::NAN), "null");
        assert_eq!(jnum(f64::INFINITY), "null");
    }

    #[test]
    fn json_escape_leaves_stats_and_quotes() {
        let stats = "OK S rx_ok=1 rx_err=0 tx=0 out_drop=0";
        assert_eq!(json_escape(stats), stats);
        assert_eq!(json_escape("a\"b\\c"), "a\\\"b\\\\c");
    }
}
