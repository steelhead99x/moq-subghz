//! moq-subghz-chan: push a recording of wire frames through a modeled channel offline.
//!
//! Reads a file of `R <t> <rssi> <snr> <hex>` lines (what `moq-subghz-send --loopback` writes),
//! packs the frames into packets the way the sender does, runs each packet through the seeded
//! channel model in virtual time and writes the survivors as `R` lines that
//! `moq-subghz-recv --replay` reads. Prints one JSON stats line on stdout; it always carries
//! `"provenance":"modeled"`. With `--lens 255x34,96x2` it prints the deterministic airtime of
//! those frames instead. No radio, no serial port, no network.
use clap::Parser;
use moq_subghz_link::Link;
use moq_subghz_radio_emu::args::{EmuArgs, EmuSel};
use moq_subghz_radio_emu::{outfile, pack, EmuLink};
use moq_subghz_wire::{parse_r_line, FrameHdr};
use std::path::Path;

#[derive(Parser)]
#[command(version, about)]
struct A {
    #[arg(long, value_enum, default_value = "s1g-emu")]
    link: EmuSel,
    #[command(flatten)]
    emu: EmuArgs,
    /// Input R-line file
    #[arg(long = "in")]
    input: Option<String>,
    /// Output R-line file (survivors only)
    #[arg(long)]
    out: Option<String>,
    /// Duration of the original run, for the busy share (seconds)
    #[arg(long, default_value_t = 0.0)]
    wall_s: f64,
    /// Instead of a file: frame lengths LENxCOUNT,... for a deterministic airtime
    #[arg(long)]
    lens: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    moq_subghz_radio_emu::args::warn_deprecated_spellings();
    let a = A::parse();
    let mut cfg = a.emu.to_cfg(a.link)?;
    cfg.expected = a.lens.is_some();
    let name = cfg.name();
    // frames: ((group, obj), bytes)
    let mut frames: Vec<((u16, u8), Vec<u8>)> = Vec::new();
    if let Some(l) = &a.lens {
        for part in l.split(',') {
            let (len, n) = part.split_once('x').ok_or("--lens LENxCOUNT,...")?;
            let (len, n): (usize, usize) = (len.parse()?, n.parse()?);
            if len == 0 || len > moq_subghz_wire::MAX_FRAME {
                return Err("--lens: frame length must be 1..=255".into());
            }
            for _ in 0..n {
                frames.push(((0, 0), vec![0u8; len]));
            }
        }
    } else {
        let text = std::fs::read_to_string(a.input.as_ref().ok_or("need --in or --lens")?)?;
        for line in text.lines() {
            if let Some(r) = parse_r_line(line) {
                let key = FrameHdr::decode(&r.frame)
                    .map(|(h, _)| (h.group, h.obj))
                    .unwrap_or((u16::MAX, 0));
                frames.push((key, r.frame));
            }
        }
        if frames.is_empty() {
            return Err("--in has no R lines to replay: refusing to write an empty result".into());
        }
    }
    // Read the whole input before any output file exists, so the output can never be the
    // input in disguise, and write the survivors through a temporary file that replaces --out
    // only after the run succeeded.
    let pending = match a.out.as_deref() {
        Some(o) => Some(outfile::plan(
            Path::new(o),
            a.input.as_deref().map(Path::new),
        )?),
        // Without --out the survivors go nowhere: write to the null device.
        None => None,
    };
    let sink = pending.as_ref().map_or("/dev/null".to_string(), |p| {
        p.write_path.to_string_lossy().into_owned()
    });
    let mut link = EmuLink::file_sender(cfg, &sink)?;
    let (mtu, maxf) = (link.caps().max_payload, link.caps().max_frames_per_packet);
    let (mut t, mut i) = (0u64, 0usize);
    let (mut pkts, mut attempts, mut fl_sent, mut fl_ok, mut pay_bytes) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut lat = Vec::new();
    while i < frames.len() {
        let key = frames[i].0;
        let n = if maxf > 1 {
            let lens: Vec<usize> = frames[i..]
                .iter()
                .take_while(|f| f.0 == key)
                .map(|f| f.1.len())
                .collect();
            pack::take_batch(&lens, mtu, maxf).max(1)
        } else {
            1
        };
        let group: Vec<&[u8]> = frames[i..i + n].iter().map(|f| f.1.as_slice()).collect();
        let packet = if maxf > 1 {
            pack::pack(&group)?
        } else {
            group[0].to_vec()
        };
        let r = link.send(&packet)?;
        pkts += 1;
        attempts += u64::from(r.attempts);
        fl_sent += n as u64;
        pay_bytes += packet.len() as u64;
        if r.delivered == Some(true) {
            fl_ok += n as u64;
            lat.push(r.deliver_after_us.unwrap_or(0));
        }
        t += r.busy_us;
        i += n;
    }
    link.idle()?;
    drop(link);
    if let Some(p) = pending {
        p.commit()?;
    }
    lat.sort_unstable();
    let pct = |p: f64| {
        lat.get(((lat.len() as f64 - 1.0) * p).round() as usize)
            .copied()
            .unwrap_or(0)
    };
    let busy_pct = if a.wall_s > 0.0 {
        100.0 * t as f64 / 1e6 / a.wall_s
    } else {
        0.0
    };
    println!(
        "{{\"provenance\":\"modeled\",\"phy\":\"{name}\",\"frames\":{fl_sent},\"packets\":{pkts},\"attempts\":{attempts},\"frames_delivered\":{fl_ok},\"frames_lost\":{},\"residual_frame_loss\":{:.6},\"payload_bytes\":{pay_bytes},\"busy_us\":{t},\"busy_pct_of_wall\":{busy_pct:.2},\"mean_attempts\":{:.4},\"frames_per_packet\":{:.3},\"pkt_latency_us_p50\":{},\"pkt_latency_us_p95\":{}}}",
        fl_sent - fl_ok,
        if fl_sent > 0 { (fl_sent - fl_ok) as f64 / fl_sent as f64 } else { 0.0 },
        attempts as f64 / pkts.max(1) as f64,
        fl_sent as f64 / pkts.max(1) as f64,
        pct(0.5),
        pct(0.95)
    );
    Ok(())
}
