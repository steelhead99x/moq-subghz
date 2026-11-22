//! Pi side of the 2015-style LoRa test: send a JPEG as chunks, as SSDV packets or as
//! LoRaWAN-framed uplinks over the Dragino HAT's SX1276 in LoRa mode.
//!
//! `--dry-run` prints the frames and the calculated airtime and touches no hardware;
//! `--loopback FILE` writes the frames as stick `R` lines for an offline run of the logger.
//! LoRaWAN-framed means the bytes of a LoRaWAN 1.0 unconfirmed uplink with test keys.
//! There is no join, no gateway and no network behind it.
use anyhow::{bail, Context, Result};
use clap::Parser;
use moq_subghz_lora2015::plan::{Dr, Mode, Plan};
use moq_subghz_lora2015::profile::{self, Profile};
use std::io::Write;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// chunk: JPEG in 255 byte frames; ssdv: `ssdv -e -n` output; lorawan: chunks in uplinks
    #[arg(long, value_enum)]
    mode: Mode,
    /// The JPEG (chunk, lorawan) or the SSDV packet file (ssdv)
    #[arg(long)]
    file: String,
    /// sf7-125 (902.3 MHz), sf7-500, sf8-500 or sf7-250 (all 903.0 MHz)
    #[arg(long, default_value = "sf7-125")]
    profile: String,
    /// Override the carrier, MHz (902-928, never 925.0)
    #[arg(long)]
    freq: Option<f64>,
    /// Override the coding rate denominator, 5..=8 (default 5, 4/5)
    #[arg(long)]
    cr: Option<u8>,
    /// dBm on PA_BOOST, 2..=5
    #[arg(long, default_value_t = 5)]
    power: i8,
    /// Chunk header image id, 0..=15 (chunk and lorawan modes)
    #[arg(long, default_value_t = 0)]
    image_id: u8,
    /// LoRaWAN payload size: dr3 and dr4 are both 242 bytes
    #[arg(long, value_enum, default_value = "dr3")]
    dr: Dr,
    /// LoRaWAN payload bytes per uplink instead of the --dr size (1..=242; 222 is the
    /// repeater-compatible US915 limit)
    #[arg(long)]
    payload: Option<usize>,
    /// Times to send the whole image
    #[arg(long, default_value_t = 1)]
    passes: u32,
    /// Wait this long after every frame, milliseconds
    #[arg(long, default_value_t = 0)]
    gap_ms: u32,
    /// Keep airtime under this percent by waiting after each frame (0 = off). 1 is the EU limit.
    #[arg(long, default_value_t = 0.0)]
    duty_pct: f64,
    /// Wait this long between passes, milliseconds
    #[arg(long, default_value_t = 1000)]
    pass_gap_ms: u32,
    /// Stop starting frames after this many seconds (0 = no limit). 30 is the smoke test.
    #[arg(long, default_value_t = 0)]
    max_secs: u32,
    /// Print the plan and the calculated airtime, send nothing
    #[arg(long)]
    dry_run: bool,
    /// Write one CSV line per frame sent to this file
    #[arg(long)]
    tx_log: Option<String>,
    /// No radio: write the frames of every pass as stick `R` lines to FILE, for
    /// `lora2015-log --replay`
    #[arg(long)]
    loopback: Option<String>,
    #[arg(long, default_value_t = 8_000_000)]
    spi_hz: u32,
    /// Required to transmit: the antenna is on the HAT
    #[arg(long)]
    i_have_an_antenna: bool,
}

fn settings(a: &Args) -> Result<Profile> {
    let mut p = profile::named(&a.profile).map_err(anyhow::Error::msg)?;
    if let Some(f) = a.freq {
        p.freq_hz = (f * 1e6).round() as u32;
    }
    if let Some(c) = a.cr {
        p.cr = c;
    }
    p.validate().map_err(anyhow::Error::msg)?;
    profile::check_power(a.power).map_err(anyhow::Error::msg)?;
    if !(a.duty_pct.is_finite() && (0.0..100.0).contains(&a.duty_pct)) {
        bail!("--duty-pct must be 0 (off) or above 0 and below 100");
    }
    if a.passes == 0 {
        bail!("--passes must be at least 1");
    }
    Ok(p)
}

fn main() -> Result<()> {
    let a = Args::parse();
    let p = settings(&a)?;
    let file = std::fs::read(&a.file).with_context(|| format!("read {}", a.file))?;
    let plan = |fcnt| Plan {
        mode: a.mode,
        file: &file,
        image: a.image_id,
        dr: a.dr,
        payload: a.payload,
        fcnt,
    };
    let first = plan(0).frames().map_err(anyhow::Error::msg)?;
    let toas: Vec<u32> = first
        .iter()
        .map(|f| p.toa_us(f.len()))
        .collect::<Result<_, _>>()
        .map_err(anyhow::Error::msg)?;
    let air_us: u64 = toas.iter().map(|&t| t as u64).sum();
    let wall_us: u64 = toas
        .iter()
        .map(|&t| t as u64 + profile::gap_us(t, a.gap_ms, a.duty_pct))
        .sum();
    let bytes: usize = first.iter().map(Vec::len).sum();
    println!(
        "{:?}: {} frames, {bytes} bytes, {:.3} s airtime per pass (calculated), {:.3} s per pass with pacing, {} MHz SF{} BW {} CR 4/{}",
        a.mode,
        first.len(),
        air_us as f64 / 1e6,
        wall_us as f64 / 1e6,
        p.freq_hz as f64 / 1e6,
        p.sf,
        p.bw_hz,
        p.cr
    );
    if a.mode == Mode::Lorawan {
        println!("LoRaWAN-framed, no network: test keys, no join, no gateway");
    }
    if a.dry_run {
        println!(
            "summary mode={:?} frames={} air_bytes={bytes} toa_s={:.3} pass_s={:.3}",
            a.mode,
            first.len(),
            air_us as f64 / 1e6,
            wall_us as f64 / 1e6
        );
        return Ok(());
    }
    if let Some(f) = &a.loopback {
        return loopback(&a, &p, &plan, f);
    }
    transmit(&a, &p, &plan, first.len())
}

/// The frames as the stick would report them, on the calculated airtime clock.
fn loopback<'a>(a: &Args, p: &Profile, plan: &dyn Fn(u16) -> Plan<'a>, file: &str) -> Result<()> {
    let mut w = std::io::BufWriter::new(
        std::fs::File::create(file).with_context(|| format!("create {file}"))?,
    );
    let (mut t_us, mut fcnt, mut n) = (1_000_000u64, 0u16, 0u64);
    for pass in 0..a.passes {
        if pass > 0 {
            t_us += a.pass_gap_ms as u64 * 1000;
        }
        let frames = plan(fcnt).frames().map_err(anyhow::Error::msg)?;
        for f in &frames {
            let toa = p.toa_us(f.len()).map_err(anyhow::Error::msg)?;
            t_us += toa as u64;
            writeln!(
                w,
                "{}",
                moq_subghz_lora2015::stick::r_line(t_us, -90, 24, f)
            )?;
            t_us += profile::gap_us(toa, a.gap_ms, a.duty_pct);
            n += 1;
        }
        fcnt = fcnt.wrapping_add(frames.len() as u16);
    }
    w.flush()?;
    println!("wrote {n} frames to {file}");
    Ok(())
}

#[cfg(target_os = "linux")]
fn transmit<'a>(
    a: &Args,
    p: &Profile,
    plan: &dyn Fn(u16) -> Plan<'a>,
    frames_per_pass: usize,
) -> Result<()> {
    use moq_subghz_link::Link;
    use moq_subghz_radio_sx1276::Sx1276LoraTx;
    if !a.i_have_an_antenna {
        bail!("pass --i-have-an-antenna to transmit");
    }
    let cfg = p.lora().map_err(anyhow::Error::msg)?;
    let mut r = Sx1276LoraTx::open(a.spi_hz, p.freq_hz, &cfg, a.power)?;
    let [mc1, mc2, mc3] = r.modem_config()?;
    if !profile::modem_readback_ok(mc1, mc2, mc3, &cfg) {
        bail!("LoRa modem readback differs from what was written");
    }
    let mut log = match &a.tx_log {
        Some(f) => {
            let mut w = std::io::BufWriter::new(
                std::fs::File::create(f).with_context(|| format!("create {f}"))?,
            );
            writeln!(w, "pass,index,len,start_s,tx_s,toa_s_calc")?;
            Some(w)
        }
        None => None,
    };
    let start = Instant::now();
    let limit = (a.max_secs > 0).then(|| Duration::from_secs(a.max_secs as u64));
    let (mut sent, mut bytes, mut air_us) = (0u64, 0u64, 0u64);
    let mut fcnt = 0u16;
    'passes: for pass in 0..a.passes {
        if pass > 0 {
            std::thread::sleep(Duration::from_millis(a.pass_gap_ms as u64));
        }
        let frames = plan(fcnt).frames().map_err(anyhow::Error::msg)?;
        for (i, f) in frames.iter().enumerate() {
            if limit.is_some_and(|l| start.elapsed() >= l) {
                break 'passes;
            }
            let toa = p.toa_us(f.len()).map_err(anyhow::Error::msg)?;
            let t0 = start.elapsed();
            r.send(f)?;
            let tx = start.elapsed() - t0;
            sent += 1;
            bytes += f.len() as u64;
            air_us += toa as u64;
            if let Some(w) = log.as_mut() {
                writeln!(
                    w,
                    "{pass},{i},{},{:.6},{:.6},{:.6}",
                    f.len(),
                    t0.as_secs_f64(),
                    tx.as_secs_f64(),
                    toa as f64 / 1e6
                )?;
            }
            std::thread::sleep(Duration::from_micros(profile::gap_us(
                toa, a.gap_ms, a.duty_pct,
            )));
        }
        fcnt = fcnt.wrapping_add(frames_per_pass as u16);
    }
    r.idle()?;
    if let Some(mut w) = log {
        w.flush()?;
    }
    let wall = start.elapsed().as_secs_f64();
    println!(
        "sent frames={sent} bytes={bytes} wall_s={wall:.3} air_s_calc={:.3} duty_pct={:.1}",
        air_us as f64 / 1e6,
        100.0 * air_us as f64 / 1e6 / wall.max(1e-9)
    );
    println!("pass --sent-frames {sent} to lora2015-log for the exact loss figure");
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn transmit<'a>(_: &Args, _: &Profile, _: &dyn Fn(u16) -> Plan<'a>, _: usize) -> Result<()> {
    bail!("transmitting needs the Pi (Linux, SPI); use --dry-run here")
}
