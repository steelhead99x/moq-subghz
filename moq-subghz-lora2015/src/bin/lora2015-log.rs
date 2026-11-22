//! Host side of the 2015-style LoRa test: listen on an RP2040-based device running firmware v5, rebuild
//! the images and write the numbers.
//!
//! Output directory: `rx.csv` (every frame), `images.csv` (every image), `run.csv` (the run
//! against the sender's frame count), `raw.log` (the stick's lines as received, live runs
//! only), the rebuilt JPEGs, and for SSDV the packet files. Files are named `image<id>`, with
//! `-2`, `-3`... for a later image that reused an id.
use anyhow::{bail, Context, Result};
use clap::Parser;
use moq_subghz_lora2015::log::{images_csv, Logger, Status, RX_HEADER};
use moq_subghz_lora2015::plan::Mode;
use moq_subghz_lora2015::profile::{self, Profile};
use moq_subghz_lora2015::stick::{self, LineIo, Serial};
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    STOP.store(true, Ordering::Relaxed);
}

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Stick serial port (macOS: /dev/cu.usbmodemXXXX, Linux: /dev/ttyACM0)
    #[arg(long)]
    port: Option<String>,
    /// Read a saved raw.log instead of a port
    #[arg(long)]
    replay: Option<String>,
    /// chunk, ssdv or lorawan: must match the sender
    #[arg(long, value_enum)]
    mode: Mode,
    /// Must match the sender: sf7-125, sf7-500, sf8-500 or sf7-250
    #[arg(long, default_value = "sf7-125")]
    profile: String,
    /// Override the carrier, MHz
    #[arg(long)]
    freq: Option<f64>,
    /// Override the coding rate denominator
    #[arg(long)]
    cr: Option<u8>,
    /// Listen this long, seconds (Ctrl-C ends it early and still writes the results)
    #[arg(long, default_value_t = 30.0)]
    secs: f64,
    /// Output directory, created if missing
    #[arg(long, default_value = "lora2015-run")]
    out: String,
    /// Frames the sender reports sending, for the exact loss figure
    #[arg(long)]
    sent_frames: Option<u32>,
    /// The JPEG that was sent, to check the rebuilt one against
    #[arg(long)]
    reference: Option<String>,
    /// The ssdv tool, to turn rebuilt packets into a JPEG (default: `ssdv` on PATH, if any)
    #[arg(long)]
    ssdv_bin: Option<String>,
}

fn main() -> Result<()> {
    let a = Args::parse();
    let mut p: Profile = profile::named(&a.profile).map_err(anyhow::Error::msg)?;
    if let Some(f) = a.freq {
        p.freq_hz = (f * 1e6).round() as u32;
    }
    if let Some(c) = a.cr {
        p.cr = c;
    }
    p.validate().map_err(anyhow::Error::msg)?;
    if !(a.secs.is_finite() && a.secs > 0.0) {
        bail!("--secs must be positive");
    }
    let reference = match &a.reference {
        Some(f) => Some(std::fs::read(f).with_context(|| format!("read {f}"))?),
        None => None,
    };
    let out = Path::new(&a.out);
    std::fs::create_dir_all(out).with_context(|| format!("create {}", a.out))?;
    let mut rx_csv = std::io::BufWriter::new(std::fs::File::create(out.join("rx.csv"))?);
    writeln!(rx_csv, "{RX_HEADER}")?;
    // A replay may read the very raw.log of this directory, so only a live run writes one.
    let mut raw = match &a.port {
        Some(_) => Some(std::io::BufWriter::new(std::fs::File::create(
            out.join("raw.log"),
        )?)),
        None => None,
    };
    let mut logger = Logger::new(a.mode, p, a.sent_frames, reference);

    let mut handle = |line: &str, host_s: f64, logger: &mut Logger| -> Result<()> {
        if let Some(w) = raw.as_mut() {
            writeln!(w, "{line}")?;
        }
        if let Some(row) = logger.feed_line(host_s, line) {
            writeln!(rx_csv, "{}", row.csv())?;
            rx_csv.flush()?;
            if let Some(w) = raw.as_mut() {
                w.flush()?;
            }
            eprintln!(
                "{:7.2}s  rssi {:4} snr {:5.1}  {} bytes  image {} index {}  {}",
                row.host_s,
                row.rssi,
                row.snr_db,
                row.len,
                row.image.map_or("-".into(), |v| v.to_string()),
                row.index.map_or("-".into(), |v| v.to_string()),
                match &row.status {
                    Status::Bad(why) => format!("BAD: {why}"),
                    s => format!("{s:?}"),
                }
            );
        }
        Ok(())
    };

    match (&a.port, &a.replay) {
        (Some(port), None) => {
            let mut s = Serial::open(port)?;
            let b = stick::start(&mut s, &p).map_err(anyhow::Error::msg)?;
            eprintln!(
                "RP2040-based device: firmware {} (SX1276 0x{:02x}); listening on {}",
                b.version,
                b.sx_version,
                p.stick_command()
            );
            // SAFETY: the handler only stores to an atomic.
            unsafe {
                libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
            }
            let t0 = Instant::now();
            let end = Duration::from_secs_f64(a.secs);
            while t0.elapsed() < end && !STOP.load(Ordering::Relaxed) {
                if let Some(l) = s.recv(Duration::from_millis(200))? {
                    handle(&l, t0.elapsed().as_secs_f64(), &mut logger)?;
                }
            }
            // back to standby so the stick stops listening
            let _ = s.send("S");
            if s.dropped() > 0 {
                eprintln!("{} over-long serial lines were dropped", s.dropped());
            }
        }
        (None, Some(file)) => {
            let f = std::fs::File::open(file).with_context(|| format!("open {file}"))?;
            for l in std::io::BufReader::new(f).lines() {
                let l = l?;
                // no host clock in a replay: the stick's own time stands in
                let t = stick::parse_rx(&l).map_or(0.0, |r| r.t_us as f64 / 1e6);
                handle(&l, t, &mut logger)?;
            }
        }
        _ => bail!("give exactly one of --port and --replay"),
    }
    rx_csv.flush()?;

    let (frames, errs) = (logger.frames, logger.rx_errors);
    let (results, run) = logger.finish();
    let ssdv_bin = a.ssdv_bin.clone().or_else(|| which("ssdv"));
    let names = file_names(&results);
    for (r, name) in results.iter().zip(&names) {
        if let Some(j) = &r.jpeg {
            std::fs::write(out.join(format!("{name}.jpg")), j)?;
        }
        if let Some(f) = &r.ssdv_file {
            let path = out.join(format!("{name}.ssdv"));
            std::fs::write(&path, f)?;
            match &ssdv_bin {
                Some(bin) => {
                    let jpg = out.join(format!("{name}.jpg"));
                    let ok = std::process::Command::new(bin)
                        .arg("-d")
                        .arg(&path)
                        .arg(&jpg)
                        .stderr(std::process::Stdio::null())
                        .status()
                        .is_ok_and(|s| s.success());
                    if !ok {
                        eprintln!("ssdv -d failed on {}", path.display());
                    }
                }
                None => eprintln!(
                    "no ssdv tool found: decode with `ssdv -d {} out.jpg`",
                    path.display()
                ),
            }
        }
    }
    std::fs::write(out.join("images.csv"), images_csv(&results))?;
    let pct = |v: Option<f64>| v.map(|x| format!("{x:.1}")).unwrap_or_default();
    std::fs::write(
        out.join("run.csv"),
        format!(
            "frames_heard,frames_good,frames_sent,loss_exact_pct\n{},{},{},{}\n",
            run.frames,
            run.good,
            run.sent.map(|s| s.to_string()).unwrap_or_default(),
            pct(run.loss_exact_pct)
        ),
    )?;
    println!(
        "{frames} frames, {errs} receive errors, {} image(s){}",
        results.len(),
        run.loss_exact_pct
            .map_or(String::new(), |l| format!(", {l:.1}% of sent frames lost"))
    );
    for (r, name) in results.iter().zip(&names) {
        println!(
            "{name}: {} of {} frames, {}{}",
            r.unique,
            r.expected.map_or("?".into(), |e| e.to_string()),
            if r.complete { "complete" } else { "incomplete" },
            r.seconds.map_or(String::new(), |s| format!(", {s:.2} s")),
        );
    }
    println!("wrote {}", out.display());
    Ok(())
}

/// `image<id>` per image, `-2`, `-3`... for a later image that reused an id, so no output
/// file overwrites another.
fn file_names(rs: &[moq_subghz_lora2015::log::ImageResult]) -> Vec<String> {
    let mut seen = std::collections::HashMap::<String, u32>::new();
    rs.iter()
        .map(|r| {
            let base = match r.callsign.as_str() {
                "" => format!("image{}", r.id),
                c => format!("image{}-{c}", r.id),
            };
            let n = seen.entry(base.clone()).or_insert(0);
            *n += 1;
            if *n == 1 {
                base
            } else {
                format!("{base}-{n}")
            }
        })
        .collect()
}

fn which(name: &str) -> Option<String> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}
