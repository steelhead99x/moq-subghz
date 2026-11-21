//! The committed test frames through every mode: what the sender would put on the air,
//! turned into the stick's `R` lines, with loss and repeats, then read back by the logger.
//! The SSDV files were made by the reference `ssdv` tool, so these also check the packet
//! format against it.

use moq_subghz_lora2015::log::{images_csv, Logger};
use moq_subghz_lora2015::plan::{Dr, Mode, Plan};
use moq_subghz_lora2015::profile::named;
use moq_subghz_lora2015::ssdv;
use std::path::PathBuf;

fn frame(name: &str) -> Vec<u8> {
    let p: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "results",
        "lora-2015",
        "frames",
        name,
    ]
    .iter()
    .collect();
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn r_line(t_us: u64, f: &[u8]) -> String {
    let hex: String = f.iter().map(|b| format!("{b:02x}")).collect();
    format!("R {t_us} -88 22 {hex}")
}

/// Send `passes` passes of the file's frames, dropping the ones `lose(pass, index)` says to.
fn run(
    mode: Mode,
    file: &[u8],
    reference: Option<Vec<u8>>,
    passes: usize,
    lose: impl Fn(usize, usize) -> bool,
) -> (Vec<moq_subghz_lora2015::log::ImageResult>, u32) {
    let p = named("sf7-125").unwrap();
    let mut l = Logger::new(mode, p, None, reference);
    let (mut t, mut sent, mut fcnt) = (1_000_000u64, 0u32, 0u16);
    for pass in 0..passes {
        let frames = Plan {
            mode,
            file,
            image: 1,
            dr: Dr::Dr3,
            payload: None,
            fcnt,
        }
        .frames()
        .unwrap();
        for (i, f) in frames.iter().enumerate() {
            assert!(f.len() <= 255);
            t += p.toa_us(f.len()).unwrap() as u64 + 20_000;
            sent += 1;
            if !lose(pass, i) {
                assert!(l.feed_line(t as f64 / 1e6, &r_line(t, f)).is_some());
            }
        }
        fcnt += frames.len() as u16;
    }
    (l.finish().0, sent)
}

#[test]
fn chunked_jpeg_comes_back_byte_for_byte_after_loss() {
    for name in ["f320x240-q10.jpg", "f160x120-q20.jpg"] {
        let j = frame(name);
        // pass 1 loses every 4th chunk, pass 2 every 3rd starting at 1: together they cover it
        let (rs, sent) = run(Mode::Chunk, &j, Some(j.clone()), 2, |p, i| {
            if p == 0 {
                i % 4 == 3
            } else {
                i % 3 == 1 && i % 4 != 3
            }
        });
        let r = &rs[0];
        assert!(r.complete, "{name}");
        assert_eq!(r.matches_reference, Some(true), "{name}");
        assert_eq!(r.jpeg.as_deref(), Some(&j[..]));
        assert!(r.loss_est_pct.unwrap() > 0.0 && sent > r.good);
        assert!(images_csv(&rs).contains(",1,"));
    }
}

#[test]
fn lorawan_framed_image_comes_back_with_a_checked_mic() {
    let j = frame("f320x240-q10.jpg");
    let (rs, sent) = run(Mode::Lorawan, &j, Some(j.clone()), 1, |_, _| false);
    let r = &rs[0];
    assert!(r.complete);
    assert_eq!(r.matches_reference, Some(true));
    assert_eq!(sent as usize, j.len().div_ceil(240));
    assert_eq!(r.loss_est_pct, Some(0.0));
}

#[test]
fn ssdv_files_from_the_reference_tool_parse_and_survive_loss() {
    for (name, id) in [("f320x240-q10.ssdv", 1), ("f160x112-q20.ssdv", 2)] {
        let file = frame(name);
        let pkts = ssdv::read_packets(&file).unwrap();
        let first = ssdv::parse(&pkts[0]).unwrap();
        assert_eq!(first.image_id, id);
        assert_eq!(ssdv::callsign_string(first.callsign), "TEST01");
        assert!(pkts[..pkts.len() - 1]
            .iter()
            .all(|p| !ssdv::parse(p).unwrap().eoi));
        assert!(ssdv::parse(pkts.last().unwrap()).unwrap().eoi);
        // on the air they are 255 bytes
        for p in &pkts {
            assert_eq!(ssdv::to_air(p).unwrap().len(), 255);
        }
        // lose two packets in pass 1, a different one in pass 2: all packets arrive
        let (rs, _) = run(Mode::Ssdv, &file, None, 2, |p, i| {
            (p == 0 && (i == 2 || i == 5)) || (p == 1 && i == 9)
        });
        let r = &rs[0];
        assert!(r.complete, "{name}");
        assert_eq!(r.ssdv_file.as_deref(), Some(&file[..]));
        // one pass only: the missing packets stay missing, and the file keeps its gaps
        let (rs, _) = run(Mode::Ssdv, &file, None, 1, |_, i| i == 2 || i == 5);
        let r = &rs[0];
        assert!(!r.complete);
        assert_eq!(r.ssdv_file.as_ref().unwrap().len(), file.len() - 2 * 256);
        let loss = r.loss_est_pct.unwrap();
        assert!((loss - 200.0 / pkts.len() as f64).abs() < 1e-9);
    }
}

#[test]
fn every_frame_fits_the_radio_and_the_dwell_limit() {
    let p = named("sf7-125").unwrap();
    for (mode, name) in [
        (Mode::Chunk, "f320x240-q10.jpg"),
        (Mode::Lorawan, "f320x240-q10.jpg"),
        (Mode::Ssdv, "f320x240-q10.ssdv"),
    ] {
        let f = frame(name);
        let frames = Plan {
            mode,
            file: &f,
            image: 0,
            dr: Dr::Dr4,
            payload: None,
            fcnt: 0,
        }
        .frames()
        .unwrap();
        // 400 ms is the US915 dwell time on a 125 kHz channel
        assert!(frames
            .iter()
            .all(|x| x.len() <= 255 && p.toa_us(x.len()).unwrap() < 400_000));
    }
}
