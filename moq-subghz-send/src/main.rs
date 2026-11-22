//! moq-subghz-send: live video -> MoQ-style objects/groups -> SX1276 FSK frames.
//!
//! Pipeline (all glue in Rust, ffmpeg only as source and encoder):
//!   source ffmpeg (-re, rawvideo yuv420p) -> [Rust: stamp capture time, burn clock + barcode]
//!   -> encoder ffmpeg (H.264, AUD + in-band SPS/PPS) -> [Rust: split access units,
//!   one object per AU, one group per GOP, fragment + XOR parity] -> TX queue with MoQ
//!   group-drop on staleness -> radio (or `--loopback FILE` to test without RF).
use anyhow::{bail, Context, Result};
use clap::Parser;
use moq_subghz_link::Link;
use moq_subghz_radio_emu::args::{EmuArgs, EmuSel};
use moq_subghz_radio_emu::{pack, EmuLink};
use moq_subghz_radio_sx1276::{CatwanTx, FileSink};
use moq_subghz_wire::{annexb, parse_fsk_profile, seal, stamp, Fragmenter, ObjHdr};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What `--link` selects. `fsk` is the real SX1276 path; the `-emu` kinds are MODELED channels
/// that drive no radio (they need `--emu-udp` or `--loopback`).
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum LinkKind {
    Fsk,
    FskEmu,
    LoraEmu,
    HalowEmu,
}

impl LinkKind {
    fn emu(self) -> Option<EmuSel> {
        match self {
            LinkKind::Fsk => None,
            LinkKind::FskEmu => Some(EmuSel::Fsk),
            LinkKind::LoraEmu => Some(EmuSel::Lora),
            LinkKind::HalowEmu => Some(EmuSel::Halow),
        }
    }
}

#[derive(Parser, Clone)]
#[command(version, about)]
struct Args {
    /// Video source: `testsrc2`, `smptehdbars`, ... (lavfi) or a file path (looped)
    #[arg(long, default_value = "testsrc2")]
    src: String,
    #[arg(long, default_value = "320x240")]
    size: String,
    #[arg(long, default_value_t = 15)]
    fps: u32,
    /// Keyframe interval in frames (= MoQ group length)
    #[arg(long, default_value_t = 15)]
    gop: u32,
    /// Video bitrate in bit/s
    #[arg(long, default_value_t = 180_000)]
    bitrate: u32,
    /// hw = h264_v4l2m2m (Pi VideoCore), x264 = libx264 ultrafast/zerolatency
    #[arg(long, default_value = "x264")]
    encoder: String,
    /// FSK profile BITRATE:FDEV:PREAMBLE_BYTES[:x]  (x = beyond datasheet, stress)
    #[arg(long, default_value = "300000:100000:6")]
    profile: String,
    /// MHz; US 902-928, never 925.0
    #[arg(long, default_value_t = 915.0)]
    freq: f64,
    /// dBm on PA_BOOST (this demo refuses > 5)
    #[arg(long, default_value_t = 5)]
    power: i8,
    /// FEC: one XOR parity frame per K data frames (0 = off, max 15)
    #[arg(long, default_value_t = 0)]
    fec_k: u8,
    /// Zero-pad radio frames shorter than this (a tiny frame right after a 255 B one is
    /// the one the CatWAN misses; 0 = off)
    #[arg(long, default_value_t = 96)]
    min_frame: usize,
    /// Drop (MoQ-style, whole rest of group) objects older than this before they start TX
    #[arg(long, default_value_t = 1000)]
    max_age_ms: u64,
    /// Stop after this many seconds
    #[arg(long, default_value_t = 330.0)]
    secs: f64,
    /// Start at this unix time (sync with the receiver)
    #[arg(long, default_value_t = 0.0)]
    start_at: f64,
    /// Encrypt every object with rind AEAD using the hex key in this file
    #[arg(long)]
    key_file: Option<String>,
    /// chacha20poly1305 | aes128gcm | aes256gcm
    #[arg(long, default_value = "chacha20poly1305")]
    cipher: String,
    /// Per-object JSON lines log
    #[arg(long)]
    log: Option<String>,
    /// Radio link. `fsk` is the SX1276 in FSK mode: the Pi HAT, or the CatWAN stick or a file when
    /// `--catwan` or `--loopback` is given. `fsk-emu`, `lora-emu` and `halow-emu` are modeled
    /// channels (no radio is driven, results are MODELED).
    #[arg(long, value_enum, default_value_t = LinkKind::Fsk)]
    link: LinkKind,
    /// With a `-emu` link: send each delivered packet as a UDP datagram to this loopback address
    /// (the receiver runs `--link ... --emu-udp ADDR`). Without it `--loopback FILE` is used.
    #[arg(long)]
    emu_udp: Option<String>,
    #[command(flatten)]
    emu: EmuArgs,
    /// Transmit through a CatWAN stick (sx1276-rs catwan-fw) on this serial port instead of
    /// the local SX1276 (reverse direction: Mac -> CatWAN -> Pi)
    #[arg(long)]
    catwan: Option<String>,
    /// No radio: write frames as CatWAN `R` lines to FILE, paced at the FSK airtime
    #[arg(long)]
    loopback: Option<String>,
    #[arg(long, default_value_t = 8_000_000)]
    spi_hz: u32,
    /// Required to transmit
    #[arg(long)]
    i_have_an_antenna: bool,
}

fn unix_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_micros() as u64
}

fn parse_profile(s: &str) -> Result<moq_subghz_wire::FskProfile> {
    parse_fsk_profile(s).map_err(|e| anyhow::anyhow!(e))
}

struct TxObj {
    group: u16,
    cap_us: u64,
    started: bool,
    frames: VecDeque<Vec<u8>>,
}

/// Objects and groups the assembler dropped because the TX queue was full.
#[derive(Default)]
struct QueueDrops {
    objects: u64,
    groups: u64,
    /// Group whose remaining objects are dropped as they arrive.
    group: Option<u16>,
}

/// Frames the TX queue may hold: twice what the radio sends in `--max-age-ms`, which
/// is more than a live link ever queues (the measured runs peaked near 50), and at
/// least 64.
fn queue_cap_frames(max_age_us: u64, frame_toa_us: u64) -> usize {
    (2 * max_age_us / frame_toa_us.max(1)).max(64) as usize
}

/// Queue `o`, keeping at most `cap` frames. When full, whole groups are dropped,
/// oldest first and never the object on air, as a MoQ relay drops a stale group; if
/// that is not enough, the new object's group goes too, including its later objects.
/// Staleness is still checked when the radio thread reaches an object; this bounds
/// memory when the radio thread is stuck and never reaches one.
fn push_bounded(q: &mut VecDeque<TxObj>, o: TxObj, cap: usize, d: &mut QueueDrops) {
    if d.group.is_some_and(|g| g != o.group) {
        d.group = None;
    }
    if d.group == Some(o.group) {
        d.objects += 1;
        return;
    }
    let queued = |q: &VecDeque<TxObj>| q.iter().map(|x| x.frames.len()).sum::<usize>();
    while queued(q) + o.frames.len() > cap {
        let Some(g) = q.iter().find(|x| !x.started).map(|x| x.group) else {
            break;
        };
        let before = q.len();
        q.retain(|x| x.started || x.group != g);
        d.objects += (before - q.len()) as u64;
        d.groups += 1;
        if g == o.group {
            d.group = Some(g);
            d.objects += 1;
            return;
        }
    }
    if queued(q) + o.frames.len() > cap {
        d.group = Some(o.group);
        d.groups += 1;
        d.objects += 1;
        return;
    }
    q.push_back(o);
}

#[derive(Default)]
struct TxStats {
    frames: u64,
    bytes: u64,
    objects_dropped: u64,
    groups_dropped: u64,
    tx_errors: u64,
    max_queue_frames: usize,
    /// Modeled links only: packets sent, PHY attempts made, packets the model lost.
    emu_packets: u64,
    emu_attempts: u64,
    emu_lost_packets: u64,
}

fn spawn(cmd: &[String], stdin: Stdio) -> Result<Child> {
    Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("spawn {}", cmd[0]))
}

#[allow(deprecated)]
fn cpu_s(who: i32) -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(who, &mut ru) };
    let tv = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    tv(ru.ru_utime) + tv(ru.ru_stime)
}

/// Capture record for the next access unit: its frame number and capture time, and
/// whether a captured frame backs it. An AU the encoder emits beyond the frames it was
/// given has none; it reuses the last real frame number, because the receiver counts
/// expected frames from first to last frame number and any sentinel would inflate it.
fn next_capture(caps: &mut VecDeque<(u32, u64)>, last: &mut u32, now: u64) -> (u32, u64, bool) {
    match caps.pop_front() {
        Some((n, t)) => {
            *last = n;
            (n, t, true)
        }
        None => (*last, now, false),
    }
}

/// The (group, object) position of the next access unit, or `None` when a GOP has
/// run past 256 frames. Only a keyframe opens a group: a group opened on a P-frame
/// would hand the receiver a non-IDR as its random access point.
fn advance(group: u16, obj: u8, key: bool) -> Option<(u16, u8)> {
    if key {
        Some((group.wrapping_add(1), 0))
    } else {
        Some((group, obj.checked_add(1)?))
    }
}

/// A fresh random salt for the run's encryption subkey.
fn random_salt() -> [u8; seal::SALT_LEN] {
    let mut b = [0u8; seal::SALT_LEN];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .expect("/dev/urandom");
    b
}

#[allow(deprecated)]
fn local_clock(us: u64) -> String {
    // wall clock in the Pi's local time zone
    let t = (us / 1_000_000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        (us / 1000) % 1000
    )
}

fn main() -> Result<()> {
    let a = Args::parse();
    if !(902.0..=928.0).contains(&a.freq) || (a.freq - 925.0).abs() < 0.3 {
        bail!("--freq must be inside 902-928 MHz and not 925.0");
    }
    if a.power > 5 || a.power < 2 {
        bail!("--power must be 2..=5 dBm");
    }
    // An object id is one byte, so a group holds at most 256 frames.
    if !(1..=256).contains(&a.gop) {
        bail!("--gop must be 1..=256 (a group's object id is one byte)");
    }
    let (w, h): (usize, usize) = {
        let (x, y) = a.size.split_once('x').context("--size WxH")?;
        (x.parse()?, y.parse()?)
    };
    if w == 0 || w % 40 != 0 || h % 2 != 0 || h < 64 {
        bail!("--size: width must be a non-zero multiple of 40, height even and at least 64");
    }
    // The clock and counter overlay (two text rows, scale w/80) must fit above the barcode.
    let need_h = 2 + 15 * (w / 80).max(1) + stamp::BAR_ROWS;
    if h < need_h {
        bail!(
            "--size {}: too short for the clock overlay at this width (height must be >= {need_h})",
            a.size
        );
    }
    let prof = parse_profile(&a.profile)?;
    let freq_hz = (a.freq * 1e6).round() as u32;
    let frame_toa_us =
        (prof.preamble as u64 + 2 + 1 + 255 + 2) * 8_000_000 / u64::from(prof.bitrate);

    let emu_sel = a.link.emu();
    let mut frame_toa_us = frame_toa_us;
    let mut link: Box<dyn Link> = match &a.loopback {
        _ if emu_sel.is_some() => {
            if a.catwan.is_some() {
                bail!("--catwan is a real link; a -emu link drives no radio");
            }
            let cfg = a
                .emu
                .to_cfg(emu_sel.unwrap())
                .map_err(|e| anyhow::anyhow!(e))?;
            let per_packet = cfg.max_frames_per_packet();
            // time one wire frame takes on the modeled air, for queue sizing
            frame_toa_us = (cfg.airtime_us(if per_packet > 1 {
                per_packet * 256
            } else {
                255
            }) / per_packet as u64)
                .max(1);
            let name = cfg.name();
            let l = match (&a.emu_udp, &a.loopback) {
                (Some(addr), _) => EmuLink::udp_sender(cfg, addr),
                (None, Some(p)) => EmuLink::file_sender(cfg, p),
                (None, None) => bail!("a -emu link needs --emu-udp ADDR or --loopback FILE"),
            }
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            eprintln!("modeled link: {name} (no radio is driven; results are MODELED)");
            Box::new(l.realtime(true))
        }
        None if a.catwan.is_some() => {
            if !a.i_have_an_antenna {
                bail!("refusing to transmit without --i-have-an-antenna");
            }
            Box::new(CatwanTx::open(
                a.catwan.as_ref().unwrap(),
                freq_hz,
                &prof,
                a.power,
                frame_toa_us,
            )?)
        }
        Some(p) => Box::new(FileSink::create(p, Duration::from_micros(frame_toa_us))?),
        None => {
            #[cfg(target_os = "linux")]
            {
                if !a.i_have_an_antenna {
                    bail!("refusing to transmit without --i-have-an-antenna");
                }
                let cfg = sx1276::fsk::FskConfig {
                    bitrate_bps: prof.bitrate,
                    fdev_hz: prof.fdev,
                    preamble_bytes: prof.preamble,
                    unchecked: prof.unchecked,
                    ..Default::default()
                };
                cfg.validate()
                    .map_err(|x| anyhow::anyhow!("fsk config: {x:?}"))?;
                Box::new(moq_subghz_radio_sx1276::Sx1276FskTx::open(
                    a.spi_hz, freq_hz, &cfg, a.power,
                )?)
            }
            #[cfg(not(target_os = "linux"))]
            bail!("radio needs Linux; use --loopback")
        }
    };

    if a.start_at > 0.0 {
        let wait = a.start_at - unix_us() as f64 / 1e6;
        if wait > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(wait));
        }
    }

    // ---- source + encoder processes
    let ff = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut src_cmd = ff(&[
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-re",
    ]);
    if std::path::Path::new(&a.src).exists() {
        src_cmd.extend(ff(&["-stream_loop", "-1", "-i", &a.src]));
    } else {
        src_cmd.extend(ff(&[
            "-f",
            "lavfi",
            "-i",
            &format!("{}=size={}:rate={}", a.src, a.size, a.fps),
        ]));
    }
    src_cmd.extend(ff(&[
        "-vf",
        &format!("scale={w}:{h},fps={}", a.fps),
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-",
    ]));
    let br = a.bitrate.to_string();
    let gop = a.gop.to_string();
    let mut enc_cmd = ff(&[
        "ffmpeg",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "yuv420p",
        "-s",
        &a.size,
        "-r",
        &a.fps.to_string(),
        "-i",
        "-",
    ]);
    match a.encoder.as_str() {
        "hw" => enc_cmd.extend(ff(&["-c:v", "h264_v4l2m2m", "-b:v", &br, "-g", &gop])),
        "x264" => enc_cmd.extend(ff(&[
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-b:v",
            &br,
            "-maxrate",
            &br,
            "-bufsize",
            &(a.bitrate / 2).to_string(),
            "-g",
            &gop,
            "-keyint_min",
            &gop,
            "-sc_threshold",
            "0",
            "-threads",
            "2",
        ])),
        x => bail!("unknown encoder {x}"),
    }
    enc_cmd.extend(ff(&[
        "-bsf:v",
        "dump_extra=freq=keyframe,h264_metadata=aud=insert",
        "-flush_packets",
        "1",
        "-f",
        "h264",
        "-",
    ]));
    eprintln!(
        "source: {}\nencoder: {}",
        src_cmd.join(" "),
        enc_cmd.join(" ")
    );
    let mut src = spawn(&src_cmd, Stdio::null())?;
    let mut enc = spawn(&enc_cmd, Stdio::piped())?;

    let stop = Arc::new(AtomicBool::new(false));
    let caps: Arc<Mutex<VecDeque<(u32, u64)>>> = Arc::default();
    let t_run = Instant::now();
    let cap_done = Arc::new(AtomicBool::new(false));
    let source_stalled = Arc::new(AtomicBool::new(false));

    // ---- capture thread: raw frames -> stamp -> encoder
    let cap_thread = {
        let (mut sout, mut ein) = (src.stdout.take().unwrap(), enc.stdin.take().unwrap());
        let (stop, caps, secs, done) = (stop.clone(), caps.clone(), a.secs, cap_done.clone());
        let label = format!("FSK {}k #", prof.bitrate / 1000);
        std::thread::spawn(move || -> Result<u32> {
            let mut buf = vec![0u8; w * h * 3 / 2];
            let mut n = 0u32;
            while !stop.load(Ordering::Relaxed) && t_run.elapsed().as_secs_f64() < secs {
                if sout.read_exact(&mut buf).is_err() {
                    break;
                }
                let t = unix_us();
                let s = (w / 80).max(1);
                stamp::draw_text(&mut buf, w, h, 2, 2, &local_clock(t), s);
                stamp::draw_text(&mut buf, w, h, 2, 2 + 8 * s, &format!("{label}{n}"), s);
                stamp::write_barcode(&mut buf, w, h, (t / 1000) as u32);
                caps.lock().unwrap().push_back((n, t));
                if ein.write_all(&buf).is_err() {
                    break;
                }
                n += 1;
            }
            done.store(true, Ordering::Relaxed);
            Ok(n) // dropping `ein` closes the encoder input -> encoder flushes and exits
        })
    };

    // ---- watchdog: the capture thread checks --secs and `stop` only between frames, so
    // a source that stays up but sends nothing would block it, and the run, forever.
    // Two seconds past either, kill the source; its pipe closes and the read returns.
    let watchdog = {
        let (done, stop, stalled, secs) = (
            cap_done.clone(),
            stop.clone(),
            source_stalled.clone(),
            a.secs,
        );
        let pid = src.id() as libc::pid_t;
        std::thread::spawn(move || {
            let grace = Duration::from_secs(2);
            let mut stopped_at: Option<Instant> = None;
            while !done.load(Ordering::Relaxed) {
                if stop.load(Ordering::Relaxed) {
                    stopped_at.get_or_insert_with(Instant::now);
                }
                let overdue = t_run.elapsed().as_secs_f64() > secs + grace.as_secs_f64()
                    || stopped_at.is_some_and(|t| t.elapsed() > grace);
                if overdue {
                    eprintln!("video source sent no frame for {grace:?}, killing it");
                    stalled.store(true, Ordering::Relaxed);
                    // SAFETY: main reaps the source only after joining this thread, so the
                    // pid still names it.
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };

    // ---- encoder output reader
    let (ctx, crx) = mpsc::channel::<Vec<u8>>();
    let rd_thread = {
        let mut eout = enc.stdout.take().unwrap();
        std::thread::spawn(move || {
            let mut b = vec![0u8; 65536];
            while let Ok(n) = eout.read(&mut b) {
                if n == 0 || ctx.send(b[..n].to_vec()).is_err() {
                    break;
                }
            }
        })
    };

    // ---- radio thread
    let q: Arc<(Mutex<VecDeque<TxObj>>, Condvar)> = Arc::default();
    let done = Arc::new(AtomicBool::new(false));
    let max_age = a.max_age_ms * 1000;
    let queue_cap = queue_cap_frames(max_age, frame_toa_us);
    let (pkt_mtu, pkt_frames) = (link.caps().max_payload, link.caps().max_frames_per_packet);
    let modeled = link.caps().provenance == moq_subghz_link::Provenance::Modeled;
    let link_name = link.caps().name;
    let mut qdrops = QueueDrops::default();
    let tx_thread = {
        let (q, done, stop) = (q.clone(), done.clone(), stop.clone());
        std::thread::spawn(move || -> (TxStats, Box<dyn Link>, f64, bool) {
            let mut st = TxStats::default();
            let mut dropped_group: Option<u16> = None;
            let mut busy = Duration::ZERO;
            let mut fatal = false;
            loop {
                let fr = {
                    let (m, cv) = &*q;
                    let mut g = m.lock().unwrap();
                    loop {
                        st.max_queue_frames = st
                            .max_queue_frames
                            .max(g.iter().map(|o| o.frames.len()).sum());
                        let Some(o) = g.front_mut() else {
                            if done.load(Ordering::Relaxed) {
                                break None;
                            }
                            g = cv.wait_timeout(g, Duration::from_millis(20)).unwrap().0;
                            continue;
                        };
                        if !o.started {
                            if dropped_group == Some(o.group)
                                || unix_us().saturating_sub(o.cap_us) > max_age
                            {
                                if dropped_group != Some(o.group) {
                                    st.groups_dropped += 1;
                                    dropped_group = Some(o.group);
                                }
                                st.objects_dropped += 1;
                                g.pop_front();
                                continue;
                            }
                            o.started = true;
                        }
                        let mut f = o.frames.pop_front().unwrap();
                        let mut nfr = 1u64;
                        if pkt_frames > 1 {
                            // a link whose packet holds several wire frames: whole frames of the
                            // same object share one packet as `[len][frame]` records
                            let lens: Vec<usize> = o.frames.iter().map(|x| x.len()).collect();
                            let used = f.len() + pack::RECORD_OVERHEAD;
                            let more = pack::take_batch(
                                &lens,
                                pkt_mtu.saturating_sub(used),
                                pkt_frames - 1,
                            );
                            let mut batch = vec![f];
                            for _ in 0..more {
                                batch.extend(o.frames.pop_front());
                            }
                            nfr = batch.len() as u64;
                            let refs: Vec<&[u8]> = batch.iter().map(|x| x.as_slice()).collect();
                            f = pack::pack(&refs).expect("frames are 1..=255 bytes");
                        }
                        if o.frames.is_empty() {
                            g.pop_front();
                        }
                        break Some((f, nfr));
                    }
                };
                let Some((f, nfr)) = fr else { break };
                let t = Instant::now();
                let mut errs = link.take_tx_errors();
                if errs > 0 {
                    eprintln!("tx error: CatWAN reported {errs}");
                }
                match link.send(&f) {
                    Ok(r) if modeled => {
                        st.emu_packets += 1;
                        st.emu_attempts += u64::from(r.attempts);
                        st.emu_lost_packets += u64::from(r.delivered == Some(false));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        errs += 1;
                        eprintln!("tx error: {e}");
                    }
                }
                st.tx_errors += errs;
                if errs > 0 && st.tx_errors > 20 {
                    // Give up, but let the run wind down and print its summary:
                    // stopping capture closes the encoder, which ends the main loop.
                    eprintln!("too many TX errors, stopping");
                    fatal = true;
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                busy += t.elapsed();
                st.frames += nfr;
                st.bytes += f.len() as u64;
            }
            (st, link, busy.as_secs_f64(), fatal)
        })
    };

    // ---- assembler: AUs -> objects -> frames
    let mut log = a
        .log
        .as_ref()
        .map(std::fs::File::create)
        .transpose()?
        .map(std::io::BufWriter::new);
    let mut sealer = match &a.key_file {
        Some(kf) => {
            let c = seal::Cipher::parse(&a.cipher).context("--cipher")?;
            Some(seal::open_sealer(std::path::Path::new(kf), c).map_err(|e| anyhow::anyhow!(e))?)
        }
        None => None,
    };
    let mut salt = random_salt();
    let mut aead_bytes = 0u64;
    let mut sealed = 0u64; // objects sealed in total
    let mut salt_used = false; // anything sealed under the current salt
    let (mut last_frame_no, mut uncaptured) = (0u32, 0u64);
    let mut gop_overrun = false;
    let mut seal_s = 0f64;
    let mut obj_bytes = 0u64;
    let mut frag = Fragmenter::new(a.fec_k).map_err(|x| anyhow::anyhow!("{x:?}"))?;
    frag.min_frame = a.min_frame;
    let mut sp = annexb::AuSplitter::default();
    let (mut group, mut obj, mut objects, mut au_bytes, mut keys) =
        (u16::MAX, 0u8, 0u64, 0u64, 0u64);
    // The closure borrows the assembler state. It has to end before that state
    // is read again below; a block drops it without `mem::drop` on a non-Drop type.
    {
        let mut handle = |au: Vec<u8>| -> Result<()> {
            let now = unix_us();
            let (frame_no, cap_us, captured) =
                next_capture(&mut caps.lock().unwrap(), &mut last_frame_no, now);
            if !captured {
                uncaptured += 1;
                eprintln!("access unit with no capture record, after frame {frame_no}");
            }
            let key = annexb::is_key(&au);
            if gop_overrun {
                return Ok(());
            }
            let Some((g, o)) = advance(group, obj, key) else {
                // The encoder ignored the keyframe interval. Stop instead of sending a
                // group the receiver can't start from; the run fails after its summary.
                eprintln!(
                    "encoder sent more than 256 frames without a keyframe (--gop {}), stopping",
                    a.gop
                );
                gop_overrun = true;
                stop.store(true, Ordering::Relaxed);
                return Ok(());
            };
            if key {
                if g == 0 && salt_used {
                    // nonces repeat once the group id wraps, so take a new subkey
                    salt = random_salt();
                    salt_used = false;
                }
                keys += 1;
            }
            (group, obj) = (g, o);
            let oh = ObjHdr {
                frame_no,
                cap_us,
                enc_us: now.saturating_sub(cap_us).min(u32::MAX as u64) as u32,
                key,
            };
            let mut data = oh.encode().to_vec();
            data.extend_from_slice(&au);
            if let Some(sl) = sealer.as_mut() {
                let t = Instant::now();
                let plain_len = data.len();
                data = sl.seal(&salt, group, obj, &data);
                seal_s += t.elapsed().as_secs_f64();
                aead_bytes += (data.len() - plain_len) as u64;
                sealed += 1;
                salt_used = true;
            }
            obj_bytes += data.len() as u64;
            let frames = match frag.fragment(group, obj, &data) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("object {group}/{obj} skipped: {e:?}");
                    return Ok(());
                }
            };
            if let Some(l) = log.as_mut() {
                writeln!(l, "{{\"t_us\":{now},\"group\":{group},\"obj\":{obj},\"frame_no\":{frame_no},\"has_capture\":{captured},\"key\":{key},\"au_bytes\":{},\"frames\":{},\"cap_us\":{cap_us},\"enc_us\":{}}}", au.len(), frames.len(), oh.enc_us)?;
            }
            objects += 1;
            au_bytes += au.len() as u64;
            let (m, cv) = &*q;
            push_bounded(
                &mut m.lock().unwrap(),
                TxObj {
                    group,
                    cap_us,
                    started: false,
                    frames: frames.into(),
                },
                queue_cap,
                &mut qdrops,
            );
            cv.notify_one();
            Ok(())
        };
        // Cut only at the next AUD: a pause on the pipe can fall inside a picture.
        while let Ok(chunk) = crx.recv() {
            for au in sp.push(&chunk) {
                handle(au)?;
            }
        }
        if let Some(au) = sp.finish() {
            handle(au)?;
        }
    }
    let wall = t_run.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    done.store(true, Ordering::Relaxed);
    q.1.notify_all();
    let captured = cap_thread.join().unwrap().unwrap_or(0);
    let _ = watchdog.join(); // before reaping the source, whose pid it may still kill
    let source_stalled = source_stalled.load(Ordering::Relaxed);
    let _ = src.kill();
    let _ = src.wait();
    // The source is killed on purpose, so its exit status says nothing; a source that
    // never produced a frame shows up as `captured == 0`. The encoder exits on its own
    // once its input closes, so a failure there is real.
    let enc_ok = match enc.wait() {
        Ok(s) => s.success(),
        Err(e) => {
            eprintln!("waiting for the encoder: {e}");
            false
        }
    };
    let _ = rd_thread.join();
    let (mut st, mut link, busy, mut tx_fatal) = tx_thread.join().unwrap();
    link.idle()?;
    // Errors the stick reports while draining its queue count toward the same limit.
    st.tx_errors += link.take_tx_errors();
    if st.tx_errors > 20 && !tx_fatal {
        eprintln!("too many TX errors ({})", st.tx_errors);
        tx_fatal = true;
    }
    drop(log);
    let emu_stats = if modeled {
        format!(
            ",\"provenance\":\"modeled\",\"emu_packets\":{},\"emu_attempts\":{},\"emu_lost_packets\":{}",
            st.emu_packets, st.emu_attempts, st.emu_lost_packets
        )
    } else {
        String::new()
    };
    let (cpu_self, cpu_child) = (cpu_s(libc::RUSAGE_SELF), cpu_s(libc::RUSAGE_CHILDREN));
    let objs = objects.max(1) as f64;
    println!(
        "{{\"role\":\"send\",\"profile\":\"{}\",\"freq_hz\":{freq_hz},\"power_dbm\":{},\"size\":\"{}\",\"fps\":{},\"gop\":{},\"encoder\":\"{}\",\"bitrate_target\":{},\"fec_k\":{},\"min_frame\":{},\"wall_s\":{wall:.2},\"captured\":{captured},\"objects\":{objects},\"keyframes\":{keys},\"video_bps\":{:.0},\"frames_tx\":{},\"bytes_tx\":{},\"radio_busy_pct\":{:.1},\"air_bps\":{:.0},\"objects_dropped\":{},\"groups_dropped\":{},\"tx_errors\":{},\"max_queue_frames\":{},\"queue_cap_frames\":{queue_cap},\"objects_dropped_queue_full\":{},\"groups_dropped_queue_full\":{},\"cipher\":\"{}\",\"au_bytes\":{au_bytes},\"object_bytes\":{obj_bytes},\"overhead_per_object\":{{\"objhdr\":{},\"aead\":{:.1},\"frame_headers_crc\":{:.1},\"padding_parity\":{:.1},\"total_air\":{:.1}}},\"seal_us_per_object\":{:.1},\"cpu_pct_sender\":{:.1},\"cpu_pct_ffmpeg\":{:.1},\"cpu_pct_total\":{:.1},\"cpu_cores\":{},\"link\":\"{}\",\"tx_fatal\":{tx_fatal},\"encoder_ok\":{enc_ok},\"source_stalled\":{source_stalled},\"aus_without_capture\":{uncaptured},\"gop_overrun\":{gop_overrun}{emu_stats}}}",
        a.profile, a.power, a.size, a.fps, a.gop, a.encoder, a.bitrate, a.fec_k, a.min_frame, au_bytes as f64 * 8.0 / wall, st.frames, st.bytes,
        100.0 * busy / wall, st.bytes as f64 * 8.0 / wall, st.objects_dropped, st.groups_dropped, st.tx_errors, st.max_queue_frames, qdrops.objects, qdrops.groups,
        sealer.as_ref().map(|x| x.cipher.name()).unwrap_or("none"),
        moq_subghz_wire::OBJ_HDR_LEN,
        aead_bytes as f64 / sealed.max(1) as f64,
        st.frames as f64 * moq_subghz_wire::HDR_LEN as f64 / objs,
        (st.bytes as f64 - st.frames as f64 * moq_subghz_wire::HDR_LEN as f64 - obj_bytes as f64) / objs,
        (st.bytes as f64 - au_bytes as f64) / objs,
        1e6 * seal_s / sealed.max(1) as f64,
        100.0 * cpu_self / wall, 100.0 * cpu_child / wall, 100.0 * (cpu_self + cpu_child) / wall,
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
        if modeled { link_name } else if a.loopback.is_some() { "loopback" } else if a.catwan.is_some() { "catwan" } else { "sx1276" }
    );
    // An AU with no capture record means the encoder and the capture count disagree,
    // so the run's latency and loss numbers can't be trusted.
    if tx_fatal || captured == 0 || !enc_ok || source_stalled || uncaptured > 0 || gop_overrun {
        // The summary above is the last line on stdout (the harness reads it with
        // `tail -1`); report the failure through the exit status only.
        eprintln!("run failed: tx_fatal={tx_fatal} captured={captured} encoder_ok={enc_ok} source_stalled={source_stalled} aus_without_capture={uncaptured} gop_overrun={gop_overrun}");
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        advance, next_capture, push_bounded, queue_cap_frames, Args, LinkKind, QueueDrops, TxObj,
    };
    use clap::Parser;

    use std::collections::VecDeque;
    use std::collections::VecDeque as Q;

    fn obj(group: u16, frames: usize, started: bool) -> TxObj {
        TxObj {
            group,
            cap_us: 0,
            started,
            frames: (0..frames).map(|_| vec![0u8; 8]).collect(),
        }
    }
    fn groups(q: &Q<TxObj>) -> Vec<u16> {
        q.iter().map(|o| o.group).collect()
    }

    #[test]
    fn full_queue_drops_whole_groups_oldest_first() {
        let (mut q, mut d) = (Q::new(), QueueDrops::default());
        for g in [1, 1, 2, 2, 3] {
            push_bounded(&mut q, obj(g, 4, false), 20, &mut d);
        }
        assert_eq!(groups(&q), [1, 1, 2, 2, 3]); // 20 frames: at the cap
        push_bounded(&mut q, obj(3, 4, false), 20, &mut d);
        assert_eq!(groups(&q), [2, 2, 3, 3]); // group 1 went, whole
        assert_eq!((d.objects, d.groups), (2, 1));
    }

    #[test]
    fn object_on_air_is_kept_and_a_stuck_queue_stays_bounded() {
        let (mut q, mut d) = (Q::new(), QueueDrops::default());
        push_bounded(&mut q, obj(1, 10, true), 12, &mut d); // on air
        push_bounded(&mut q, obj(1, 4, false), 12, &mut d);
        // the only droppable group is the new object's own: drop it and its rest
        assert_eq!(groups(&q), [1]);
        push_bounded(&mut q, obj(1, 1, false), 12, &mut d);
        assert_eq!(groups(&q), [1]); // rest of a dropped group
        push_bounded(&mut q, obj(2, 2, false), 12, &mut d); // next group fits
        assert_eq!(groups(&q), [1, 2]);
        // radio stuck forever: the queue never passes the cap
        for i in 0..10_000u32 {
            push_bounded(&mut q, obj(3 + (i / 15) as u16, 3, false), 12, &mut d);
            assert!(q.iter().map(|o| o.frames.len()).sum::<usize>() <= 12);
        }
        assert!(q[0].started);
    }

    #[test]
    fn cap_is_above_what_a_live_link_queues() {
        // 300 kbit/s, 6-byte preamble: about 7 ms per 255-byte frame
        assert_eq!(queue_cap_frames(1_000_000, 7_093), 281);
        assert_eq!(queue_cap_frames(10_000, 7_093), 64);
    }

    #[test]
    fn only_a_keyframe_opens_a_group() {
        assert_eq!(advance(u16::MAX, 0, true), Some((0, 0))); // first IDR
        assert_eq!(advance(3, 0, false), Some((3, 1)));
        assert_eq!(advance(3, 254, false), Some((3, 255)));
        assert_eq!(advance(3, 255, false), None); // 257th frame without an IDR
        assert_eq!(advance(3, 255, true), Some((4, 0)));
        assert_eq!(advance(u16::MAX, 7, true), Some((0, 0))); // group id wraps
    }

    #[test]
    fn uncaptured_au_reuses_the_last_frame_number() {
        let mut caps = VecDeque::from([(7, 100), (8, 200)]);
        let mut last = 0;
        assert_eq!(next_capture(&mut caps, &mut last, 999), (7, 100, true));
        assert_eq!(next_capture(&mut caps, &mut last, 999), (8, 200, true));
        assert_eq!(next_capture(&mut caps, &mut last, 999), (8, 999, false));
        // before any capture: frame 0, not u32::MAX
        assert_eq!(next_capture(&mut VecDeque::new(), &mut 0, 5), (0, 5, false));
    }

    #[test]
    fn link_defaults_to_fsk_and_rejects_unknown_kinds() {
        assert_eq!(Args::parse_from(["send"]).link, LinkKind::Fsk);
        assert_eq!(
            Args::parse_from(["send", "--link", "fsk"]).link,
            LinkKind::Fsk
        );
        assert!(Args::try_parse_from(["send", "--link", "lora"]).is_err());
        for (name, kind) in [
            ("fsk-emu", LinkKind::FskEmu),
            ("lora-emu", LinkKind::LoraEmu),
            ("halow-emu", LinkKind::HalowEmu),
        ] {
            let a = Args::parse_from(["send", "--link", name]);
            assert_eq!(a.link, kind);
            assert!(a.link.emu().is_some());
        }
        assert!(LinkKind::Fsk.emu().is_none());
    }
}
