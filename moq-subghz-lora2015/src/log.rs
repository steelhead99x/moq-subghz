//! What the logger does with the stick's lines: reassemble images, count loss, compute
//! the airtime the link needed, and write the numbers as CSV.
//!
//! Times in the output come from the stick's microsecond clock (the instant a frame ended
//! on the air), not from the USB read, so host scheduling does not blur them.

use crate::plan::Mode;
use crate::profile::Profile;
use crate::stick::{parse_rx, Rx};
use crate::{chunk, lorawan, ssdv};
use std::collections::BTreeMap;
use std::fmt::Write as _;

/// What happened to one received frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// A new piece of an image.
    New,
    /// A piece already held (a repeat pass).
    Duplicate,
    /// Not a frame of this mode, or a check failed. The reason is in the string.
    Bad(String),
}

impl Status {
    fn word(&self) -> &str {
        match self {
            Status::New => "new",
            Status::Duplicate => "dup",
            Status::Bad(_) => "bad",
        }
    }
}

/// One line of `rx.csv`.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Seconds since the logger started.
    pub host_s: f64,
    /// The stick's clock, microseconds.
    pub t_us: u64,
    /// Packet RSSI in dBm.
    pub rssi: i32,
    /// Packet SNR in dB.
    pub snr_db: f32,
    /// Frame length.
    pub len: usize,
    /// Image id, when the frame parsed.
    pub image: Option<u16>,
    /// Chunk or packet number.
    pub index: Option<u16>,
    /// What happened.
    pub status: Status,
    /// Calculated airtime of this frame in milliseconds.
    pub toa_ms: f64,
}

/// Header of `rx.csv`.
pub const RX_HEADER: &str =
    "host_s,stick_t_us,rssi_dbm,snr_db,len,image,index,status,toa_ms_calc,note";

impl Row {
    /// The CSV line, no newline.
    pub fn csv(&self) -> String {
        let opt = |v: Option<u16>| v.map(|x| x.to_string()).unwrap_or_default();
        let note = match &self.status {
            Status::Bad(why) => why.replace(',', ";"),
            _ => String::new(),
        };
        format!(
            "{:.3},{},{},{:.2},{},{},{},{},{:.2},{}",
            self.host_s,
            self.t_us,
            self.rssi,
            self.snr_db,
            self.len,
            opt(self.image),
            opt(self.index),
            self.status.word(),
            self.toa_ms,
            note
        )
    }
}

enum Asm {
    Pieces(chunk::Assembly),
    Packets(ssdv::Assembly),
}

/// What tells images apart: the SSDV callsign (0 in the chunk modes, which have none) and
/// the image id.
type Key = (u32, u16);

struct Image {
    id: u16,
    callsign: u32,
    asm: Asm,
    first_t: u64,
    first_len: usize,
    done_t: Option<u64>,
    order: Vec<u16>,
    good: u32,
    dups: u32,
    air_bytes: usize,
    payload_per_frame: usize,
    toa_us: u64,
    rssi: f64,
    snr: f64,
}

/// Everything known about one image at the end of the run.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageResult {
    /// Image id.
    pub id: u16,
    /// SSDV callsign, as `ssdv -d` prints it; empty in the chunk modes.
    pub callsign: String,
    /// Distinct frames in the image, when known.
    pub expected: Option<usize>,
    /// Distinct frames received.
    pub unique: usize,
    /// Good frames received, repeats included.
    pub good: u32,
    /// Repeats among them.
    pub dups: u32,
    /// All frames present and the result checks out.
    pub complete: bool,
    /// Seconds from the first frame to the image being complete, plus one frame of airtime.
    pub seconds: Option<f64>,
    /// Loss estimated from sequence gaps, percent.
    pub loss_est_pct: Option<f64>,
    /// Loss against the number of frames the sender says it sent, percent. Only when the
    /// run holds this one image: the sender's count is for the whole run.
    pub loss_exact_pct: Option<f64>,
    /// Bytes on the air for the distinct frames.
    pub air_bytes: usize,
    /// Image bytes: the JPEG, when it is assembled.
    pub image_bytes: Option<usize>,
    /// Image bytes carried per frame.
    pub payload_per_frame: usize,
    /// Calculated airtime of the distinct frames, seconds.
    pub toa_s_calc: f64,
    /// Mean packet RSSI, dBm.
    pub rssi_mean: f64,
    /// Mean packet SNR, dB.
    pub snr_mean: f64,
    /// The JPEG, when complete and (if a reference was given) when it matches it.
    pub jpeg: Option<Vec<u8>>,
    /// SSDV packets in order, for `ssdv -d`.
    pub ssdv_file: Option<Vec<u8>>,
    /// Whether the JPEG equals the reference file, when one was given.
    pub matches_reference: Option<bool>,
}

/// Frames lost per the sequence gaps: `(expected, lost)`.
///
/// A new pass starts when the index does not rise. A pass that ends early on a gap at its
/// tail, or a whole pass lost, cannot be seen: the sender's frame count (`loss_exact_pct`)
/// is the exact figure.
pub fn estimate_loss(order: &[u16], count: usize) -> (usize, usize) {
    let mut passes: Vec<Vec<u16>> = Vec::new();
    for &i in order {
        match passes.last_mut() {
            Some(p) if p.last().is_some_and(|&l| i > l) => p.push(i),
            _ => passes.push(vec![i]),
        }
    }
    let n = passes.len();
    let (mut expected, mut got) = (0, 0);
    for (k, p) in passes.iter().enumerate() {
        let top = *p.last().unwrap() as usize + 1;
        expected += if k + 1 == n { top.min(count) } else { count };
        got += p.len();
    }
    (expected, expected.saturating_sub(got))
}

/// Run state: feed it the stick's lines, then call [`Logger::finish`].
pub struct Logger {
    mode: Mode,
    profile: Profile,
    sent_frames: Option<u32>,
    reference: Option<Vec<u8>>,
    images: BTreeMap<Key, Image>,
    finished: Vec<Image>,
    /// Lines that were `R` frames.
    pub frames: u64,
    /// Lines the stick sent as receive errors (`X`).
    pub rx_errors: u64,
}

impl Logger {
    /// A logger for one mode and radio setting. `sent_frames` is the sender's frame count,
    /// for an exact loss figure. `reference` is the JPEG that was sent, to compare against.
    pub fn new(
        mode: Mode,
        profile: Profile,
        sent_frames: Option<u32>,
        reference: Option<Vec<u8>>,
    ) -> Self {
        Logger {
            mode,
            profile,
            sent_frames,
            reference,
            images: BTreeMap::new(),
            finished: Vec::new(),
            frames: 0,
            rx_errors: 0,
        }
    }

    /// One line from the stick. Returns the row for a frame; `None` for any other line.
    pub fn feed_line(&mut self, host_s: f64, line: &str) -> Option<Row> {
        if line.starts_with("X ") {
            self.rx_errors += 1;
            return None;
        }
        let rx = parse_rx(line)?;
        self.frames += 1;
        Some(self.feed(host_s, &rx))
    }

    fn parse(&self, frame: &[u8]) -> Result<(Key, u16, Piece), String> {
        match self.mode {
            Mode::Ssdv => {
                let pkt = ssdv::from_air(frame)?;
                let info = ssdv::parse(&pkt)?;
                Ok((
                    (info.callsign, info.image_id as u16),
                    info.packet_id,
                    Piece::Packet(Box::new(pkt), info),
                ))
            }
            Mode::Chunk => Self::pieces(frame),
            Mode::Lorawan => {
                let (_, payload) = lorawan::decode(&lorawan::TEST_KEY, frame)?;
                Self::pieces(&payload)
            }
        }
    }

    fn pieces(data: &[u8]) -> Result<(Key, u16, Piece), String> {
        let h = chunk::Hdr::decode(data).ok_or("bad chunk header")?;
        if data.len() <= chunk::HDR_LEN {
            return Err("chunk with no data".into());
        }
        Ok((
            (0, h.image as u16),
            h.seq as u16,
            Piece::Chunk(h, data[chunk::HDR_LEN..].to_vec()),
        ))
    }

    /// One received frame.
    pub fn feed(&mut self, host_s: f64, rx: &Rx) -> Row {
        let toa_us = self.profile.toa_us(rx.frame.len()).unwrap_or(0) as u64;
        let mut row = Row {
            host_s,
            t_us: rx.t_us,
            rssi: rx.rssi,
            snr_db: rx.snr_db,
            len: rx.frame.len(),
            image: None,
            index: None,
            status: Status::New,
            toa_ms: toa_us as f64 / 1000.0,
        };
        match self.parse(&rx.frame) {
            Err(why) => row.status = Status::Bad(why),
            Ok((key, idx, piece)) => {
                row.image = Some(key.1);
                row.index = Some(idx);
                row.status = self.store(key, idx, piece, rx, toa_us);
            }
        }
        row
    }

    fn store(&mut self, key: Key, idx: u16, piece: Piece, rx: &Rx, toa_us: u64) -> Status {
        // The same id with a different chunk count, or a piece that conflicts with one
        // already held (the sender never repeats a piece with other bytes), is a new image
        // that reused the id: close the old one and start again.
        if let (Some(img), Piece::Chunk(h, _)) = (self.images.get(&key), &piece) {
            if matches!(&img.asm, Asm::Pieces(a) if a.count() != h.count as usize) {
                self.retire(key);
            }
        }
        let fresh = match self.push(key, &piece, rx) {
            Ok(f) => f,
            Err(_) if self.images.get(&key).is_some_and(|i| i.good > 0) => {
                self.retire(key);
                match self.push(key, &piece, rx) {
                    Ok(f) => f,
                    Err(e) => return Status::Bad(e),
                }
            }
            Err(e) => return Status::Bad(e),
        };
        let img = self.images.get_mut(&key).expect("push created it");
        img.good += 1;
        img.order.push(idx);
        img.rssi += rx.rssi as f64;
        img.snr += rx.snr_db as f64;
        if !fresh {
            img.dups += 1;
            return Status::Duplicate;
        }
        img.air_bytes += rx.frame.len();
        img.toa_us += toa_us;
        let done = match &img.asm {
            Asm::Pieces(a) => a.assemble().is_some(),
            Asm::Packets(a) => a.complete(),
        };
        if done && img.done_t.is_none() {
            img.done_t = Some(rx.t_us);
        }
        Status::New
    }

    fn retire(&mut self, key: Key) {
        if let Some(old) = self.images.remove(&key) {
            self.finished.push(old);
        }
    }

    /// Put a piece into its image, creating the image if needed. `Ok(true)` for a new
    /// piece, `Ok(false)` for a repeat, `Err` for a conflict or a kind change.
    fn push(&mut self, key: Key, piece: &Piece, rx: &Rx) -> Result<bool, String> {
        let img = self.images.entry(key).or_insert_with(|| Image {
            id: key.1,
            callsign: key.0,
            asm: match piece {
                Piece::Chunk(h, _) => Asm::Pieces(chunk::Assembly::new(h.count)),
                Piece::Packet(..) => Asm::Packets(ssdv::Assembly::default()),
            },
            first_t: rx.t_us,
            first_len: rx.frame.len(),
            done_t: None,
            order: Vec::new(),
            good: 0,
            dups: 0,
            air_bytes: 0,
            payload_per_frame: 0,
            toa_us: 0,
            rssi: 0.0,
            snr: 0.0,
        });
        match (&mut img.asm, piece) {
            (Asm::Pieces(a), Piece::Chunk(h, data)) => {
                let fresh = a.push(h.seq, data)? == chunk::Push::New;
                if fresh {
                    img.payload_per_frame = img.payload_per_frame.max(data.len());
                }
                Ok(fresh)
            }
            (Asm::Packets(a), Piece::Packet(pkt, info)) => {
                let fresh = a.push(**pkt, *info)?;
                img.payload_per_frame = ssdv::PAYLOAD_LEN;
                Ok(fresh)
            }
            _ => Err("frame kind changed inside an image".into()),
        }
    }

    /// Close the run and compute each image's numbers, and the run's.
    pub fn finish(mut self) -> (Vec<ImageResult>, RunResult) {
        let mut all = std::mem::take(&mut self.finished);
        all.extend(std::mem::take(&mut self.images).into_values());
        let good: u64 = all.iter().map(|i| i.good as u64).sum();
        let one_image = all.len() == 1;
        let images = all.iter().map(|i| self.result(i, one_image)).collect();
        let run = RunResult {
            frames: self.frames,
            good,
            sent: self.sent_frames,
            loss_exact_pct: self
                .sent_frames
                .filter(|&s| s > 0)
                .map(|s| 100.0 * (1.0 - (good as f64 / s as f64).min(1.0))),
        };
        (images, run)
    }

    fn result(&self, img: &Image, one_image: bool) -> ImageResult {
        let (expected, unique, jpeg, ssdv_file) = match &img.asm {
            Asm::Pieces(a) => (
                Some(a.count()),
                a.have(),
                a.assemble().filter(|j| chunk::looks_like_jpeg(j)),
                None,
            ),
            Asm::Packets(a) => (a.count(), a.have(), None, Some(a.file())),
        };
        let complete = match &img.asm {
            Asm::Pieces(_) => jpeg.is_some(),
            Asm::Packets(a) => a.complete(),
        };
        let matches_reference = match (&self.reference, &jpeg) {
            (Some(r), Some(j)) => Some(r == j),
            (Some(_), None) if self.mode != Mode::Ssdv => Some(false),
            _ => None,
        };
        let count =
            expected.unwrap_or_else(|| img.order.iter().max().map_or(0, |&m| m as usize + 1));
        let (exp, lost) = estimate_loss(&img.order, count);
        let n = img.good.max(1) as f64;
        ImageResult {
            id: img.id,
            callsign: if img.callsign == 0 {
                String::new()
            } else {
                ssdv::callsign_string(img.callsign)
            },
            expected,
            unique,
            good: img.good,
            dups: img.dups,
            complete,
            seconds: img.done_t.filter(|_| complete).map(|t| {
                let toa = self.profile.toa_us(img.first_len).unwrap_or(0) as f64;
                (t.saturating_sub(img.first_t) as f64 + toa) / 1e6
            }),
            loss_est_pct: (exp > 0).then(|| 100.0 * lost as f64 / exp as f64),
            loss_exact_pct: self
                .sent_frames
                .filter(|&s| s > 0 && one_image)
                .map(|s| 100.0 * (1.0 - (img.good as f64 / s as f64).min(1.0))),
            air_bytes: img.air_bytes,
            image_bytes: jpeg.as_ref().map(|j| j.len()),
            payload_per_frame: img.payload_per_frame,
            toa_s_calc: img.toa_us as f64 / 1e6,
            rssi_mean: img.rssi / n,
            snr_mean: img.snr / n,
            jpeg: jpeg.filter(|_| matches_reference != Some(false)),
            ssdv_file,
            matches_reference,
        }
    }
}

/// The whole run against the sender's frame count.
#[derive(Clone, Debug, PartialEq)]
pub struct RunResult {
    /// Frames heard (`R` lines).
    pub frames: u64,
    /// Frames that parsed and were stored, repeats included.
    pub good: u64,
    /// Frames the sender reports sending, when given.
    pub sent: Option<u32>,
    /// Frames sent but not delivered intact, percent.
    pub loss_exact_pct: Option<f64>,
}

enum Piece {
    Chunk(chunk::Hdr, Vec<u8>),
    Packet(Box<[u8; ssdv::PKT_LEN]>, ssdv::Info),
}

/// Header of `images.csv`.
pub const IMAGES_HEADER: &str = "image,complete,frames_expected,frames_unique,frames_good,repeats,seconds_to_image,effective_fps,loss_est_pct,loss_exact_pct,image_bytes,air_bytes_unique,image_bytes_per_frame,air_toa_s_calc,calc_fps_if_no_loss,effective_image_bps,rssi_mean_dbm,snr_mean_db,matches_reference,callsign";

/// `images.csv`: one line per image, calculated columns marked `_calc`.
pub fn images_csv(rs: &[ImageResult]) -> String {
    let mut s = format!("{IMAGES_HEADER}\n");
    for r in rs {
        let f = |v: Option<f64>, p: usize| v.map(|x| format!("{x:.p$}")).unwrap_or_default();
        let u = |v: Option<usize>| v.map(|x| x.to_string()).unwrap_or_default();
        // The airtime of the whole image, from the frames received. Only meaningful when
        // the image is complete; otherwise the calc columns are left empty.
        let calc = (r.complete).then_some(r.toa_s_calc);
        let _ = writeln!(
            s,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.1},{:.2},{},{}",
            r.id,
            r.complete as u8,
            u(r.expected),
            r.unique,
            r.good,
            r.dups,
            f(r.seconds, 3),
            f(r.seconds.map(|x| 1.0 / x), 4),
            f(r.loss_est_pct, 1),
            f(r.loss_exact_pct, 1),
            u(r.image_bytes),
            r.air_bytes,
            r.payload_per_frame,
            f(calc, 3),
            f(calc.map(|x| 1.0 / x), 4),
            f(
                r.image_bytes
                    .zip(r.seconds)
                    .map(|(b, t)| b as f64 * 8.0 / t),
                0
            ),
            r.rssi_mean,
            r.snr_mean,
            r.matches_reference
                .map(|m| (m as u8).to_string())
                .unwrap_or_default(),
            r.callsign,
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Dr, Plan};
    use crate::profile::named;

    fn jpeg(n: usize) -> Vec<u8> {
        let mut v: Vec<u8> = (0..n).map(|i| (i * 13 + 5) as u8).collect();
        v[..2].copy_from_slice(&[0xFF, 0xD8]);
        v[n - 2..].copy_from_slice(&[0xFF, 0xD9]);
        v
    }

    fn rx(t_us: u64, frame: Vec<u8>) -> Rx {
        Rx {
            t_us,
            rssi: -80,
            snr_db: 6.0,
            frame,
        }
    }

    fn logger(mode: Mode, sent: Option<u32>, reference: Option<Vec<u8>>) -> Logger {
        Logger::new(mode, named("sf7-125").unwrap(), sent, reference)
    }

    fn frames(mode: Mode, file: &[u8]) -> Vec<Vec<u8>> {
        Plan {
            mode,
            file,
            image: 4,
            dr: Dr::Dr3,
            payload: None,
            fcnt: 0,
        }
        .frames()
        .unwrap()
    }

    #[test]
    fn loss_estimate_counts_gaps_per_pass() {
        // two full passes of 5, one chunk missing from each
        assert_eq!(estimate_loss(&[0, 1, 3, 4, 0, 2, 3, 4], 5), (10, 2));
        // a clean single pass
        assert_eq!(estimate_loss(&[0, 1, 2, 3, 4], 5), (5, 0));
        // the last pass is cut short: only what it should have reached counts
        assert_eq!(estimate_loss(&[0, 1, 2, 3, 4, 0, 2], 5), (8, 1));
        // the head of a pass lost shows as a gap from 0
        assert_eq!(estimate_loss(&[2, 3, 4], 5), (5, 2));
        assert_eq!(estimate_loss(&[], 5), (0, 0));
    }

    #[test]
    fn chunk_image_completes_across_passes_with_loss() {
        let j = jpeg(1000);
        let f = frames(Mode::Chunk, &j);
        assert_eq!(f.len(), 4);
        let mut l = logger(Mode::Chunk, Some(8), Some(j.clone()));
        // pass 1 loses chunk 2, pass 2 loses chunk 0
        let heard = [(0, 0), (1, 1), (3, 3), (4, 1), (5, 2), (6, 3)];
        for (n, (t, i)) in heard.iter().enumerate() {
            let row = l.feed(
                n as f64,
                &rx(1_000_000 + *t as u64 * 500_000, f[*i].clone()),
            );
            assert_eq!(
                row.status,
                if n == 3 || n == 5 {
                    Status::Duplicate
                } else {
                    Status::New
                },
                "row {n}"
            );
        }
        let r = &l.finish().0[0];
        assert!(r.complete);
        assert_eq!((r.unique, r.good, r.dups, r.expected), (4, 6, 2, Some(4)));
        assert_eq!(r.matches_reference, Some(true));
        assert_eq!(r.jpeg.as_deref(), Some(&j[..]));
        // first frame at 1.0 s, chunk 2 arrives at 3.5 s: 2.5 s plus one frame of airtime
        let toa = named("sf7-125").unwrap().toa_us(255).unwrap() as f64 / 1e6;
        assert!((r.seconds.unwrap() - (2.5 + toa)).abs() < 1e-9);
        assert_eq!(r.loss_exact_pct, Some(25.0));
        // passes [0,1,3] and [1,2,3]: expected 4+4, got 6
        assert_eq!(r.loss_est_pct, Some(25.0));
        assert_eq!(r.payload_per_frame, 253);
        assert_eq!(r.air_bytes, 255 * 3 + (2 + 1000 - 3 * 253));
    }

    #[test]
    fn incomplete_images_say_so_and_write_nothing() {
        let j = jpeg(1000);
        let f = frames(Mode::Chunk, &j);
        let mut l = logger(Mode::Chunk, None, None);
        for i in [0, 1, 3] {
            l.feed(0.0, &rx(i as u64, f[i].clone()));
        }
        let r = &l.finish().0[0];
        assert!(!r.complete);
        assert!(r.jpeg.is_none() && r.seconds.is_none() && r.image_bytes.is_none());
        let csv = images_csv(std::slice::from_ref(r));
        let line = csv.lines().nth(1).unwrap();
        assert!(line.starts_with("4,0,4,3,3,0,,,"), "{line}");
    }

    #[test]
    fn corrupted_reassembly_is_not_called_a_match() {
        let j = jpeg(1000);
        let mut other = j.clone();
        other[500] ^= 0xFF;
        let mut l = logger(Mode::Chunk, None, Some(other));
        for (i, fr) in frames(Mode::Chunk, &j).into_iter().enumerate() {
            l.feed(0.0, &rx(i as u64, fr));
        }
        let r = &l.finish().0[0];
        assert_eq!(r.matches_reference, Some(false));
        assert!(r.jpeg.is_none(), "a mismatching image is not saved as good");
    }

    #[test]
    fn lorawan_frames_need_a_good_mic() {
        let j = jpeg(700);
        let f = frames(Mode::Lorawan, &j);
        assert_eq!(f.len(), 3);
        let mut l = logger(Mode::Lorawan, None, Some(j.clone()));
        let mut bent = f[1].clone();
        bent[20] ^= 1;
        let row = l.feed(0.0, &rx(0, bent));
        assert_eq!(row.status, Status::Bad("MIC mismatch".into()));
        assert!(row.image.is_none());
        for (i, fr) in f.iter().enumerate() {
            l.feed(0.0, &rx(i as u64 * 10, fr.clone()));
        }
        let r = &l.finish().0[0];
        assert!(r.complete);
        assert_eq!(r.matches_reference, Some(true));
        assert_eq!(r.payload_per_frame, 240);
    }

    #[test]
    fn same_id_with_a_new_count_starts_a_new_image() {
        let (a, b) = (jpeg(1000), jpeg(300));
        let mut l = logger(Mode::Chunk, None, None);
        for (i, fr) in frames(Mode::Chunk, &a).into_iter().enumerate() {
            l.feed(0.0, &rx(i as u64, fr));
        }
        for (i, fr) in frames(Mode::Chunk, &b).into_iter().enumerate() {
            l.feed(0.0, &rx(100 + i as u64, fr));
        }
        let (rs, _) = l.finish();
        assert_eq!(rs.len(), 2);
        assert!(rs.iter().all(|r| r.complete));
    }

    #[test]
    fn ssdv_image_counts_packets_and_keeps_partial_files() {
        let pk: Vec<_> = (0..4)
            .map(|i| ssdv::tests_support::packet(7, i, i == 3))
            .collect();
        let file = pk.concat();
        let f = frames(Mode::Ssdv, &file);
        let mut l = logger(Mode::Ssdv, None, None);
        for i in [0usize, 1, 3, 0, 2, 3] {
            l.feed(0.0, &rx(i as u64 * 1000, f[i].clone()));
        }
        let r = &l.finish().0[0];
        assert!(r.complete);
        assert_eq!((r.id, r.expected, r.unique, r.dups), (7, Some(4), 4, 2));
        assert_eq!(r.ssdv_file.as_ref().unwrap(), &file);
        assert_eq!(r.payload_per_frame, 237);
        assert!(r.jpeg.is_none() && r.matches_reference.is_none());

        let mut l = logger(Mode::Ssdv, None, None);
        for i in [0usize, 2, 3] {
            l.feed(0.0, &rx(i as u64, f[i].clone()));
        }
        let r = &l.finish().0[0];
        assert!(!r.complete);
        assert_eq!(r.ssdv_file.as_ref().unwrap().len(), 3 * 256);
        assert_eq!(r.loss_est_pct, Some(25.0));
    }

    #[test]
    fn same_id_and_count_with_other_bytes_starts_a_new_image() {
        let a = jpeg(1000);
        let mut b = jpeg(900);
        b[2..898].iter_mut().for_each(|x| *x ^= 0x5A); // other bytes from the first chunk on
        let (fa, fb) = (frames(Mode::Chunk, &a), frames(Mode::Chunk, &b));
        assert_eq!(fa.len(), fb.len());
        let mut l = logger(Mode::Chunk, Some(8), None);
        for fr in fa.into_iter().chain(fb) {
            assert_eq!(l.feed(0.0, &rx(0, fr)).status, Status::New);
        }
        let (rs, run) = l.finish();
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].jpeg.as_deref(), Some(&a[..]));
        assert_eq!(rs[1].jpeg.as_deref(), Some(&b[..]));
        assert_eq!(run.loss_exact_pct, Some(0.0));
    }

    #[test]
    fn exact_loss_is_per_run_when_a_run_holds_several_images() {
        // two clean images in one run, 4 + 2 frames, sender sent 6: no loss
        let (a, b) = (jpeg(1000), jpeg(300));
        let mut l = logger(Mode::Chunk, Some(6), None);
        for (i, fr) in frames(Mode::Chunk, &a).into_iter().enumerate() {
            l.feed(0.0, &rx(i as u64, fr));
        }
        for (i, fr) in frames(Mode::Chunk, &b).into_iter().enumerate() {
            l.feed(0.0, &rx(100 + i as u64, fr));
        }
        let (rs, run) = l.finish();
        assert_eq!(rs.len(), 2);
        assert!(rs.iter().all(|r| r.loss_exact_pct.is_none()));
        assert_eq!(
            (run.good, run.sent, run.loss_exact_pct),
            (6, Some(6), Some(0.0))
        );
    }

    #[test]
    fn ssdv_images_with_one_id_but_two_callsigns_stay_apart() {
        let mk = |call: u32, id: u16, eoi: bool| {
            let mut p = ssdv::tests_support::packet(7, id, eoi);
            p[2..6].copy_from_slice(&call.to_be_bytes());
            let c = ssdv::crc32(&p[1..ssdv::PKT_LEN - 4]).to_be_bytes();
            p[ssdv::PKT_LEN - 4..].copy_from_slice(&c);
            ssdv::to_air(&p).unwrap().to_vec()
        };
        let mut l = logger(Mode::Ssdv, None, None);
        for (call, id, eoi) in [(1, 0, false), (1, 1, true), (2, 0, false), (2, 2, true)] {
            assert_eq!(l.feed(0.0, &rx(0, mk(call, id, eoi))).status, Status::New);
        }
        let (rs, _) = l.finish();
        assert_eq!(rs.len(), 2);
        assert!(rs[0].complete, "callsign 1 has packets 0 and 1");
        assert!(!rs[1].complete, "callsign 2 is missing packet 1");
        assert_ne!(rs[0].callsign, rs[1].callsign);
    }

    #[test]
    fn wrong_mode_frames_are_bad_not_fatal() {
        let j = jpeg(600);
        let mut l = logger(Mode::Ssdv, None, None);
        let row = l.feed(0.0, &rx(0, frames(Mode::Chunk, &j)[0].clone()));
        assert!(matches!(row.status, Status::Bad(_)));
        let mut l = logger(Mode::Chunk, None, None);
        assert!(matches!(
            l.feed(0.0, &rx(0, vec![1])).status,
            Status::Bad(_)
        ));
        assert!(matches!(
            l.feed(0.0, &rx(0, vec![0, 0])).status,
            Status::Bad(_)
        ));
        assert!(l.finish().0.is_empty());
    }

    #[test]
    fn feed_line_reads_the_stick_protocol() {
        let j = jpeg(300);
        let f = frames(Mode::Chunk, &j);
        let hex: String = f[0].iter().map(|b| format!("{b:02x}")).collect();
        let mut l = logger(Mode::Chunk, None, None);
        assert!(l.feed_line(1.5, &format!("R 42 -91 -10 {hex}")).is_some());
        assert!(l.feed_line(1.6, "X 43 crc").is_none());
        assert!(l.feed_line(1.7, "OK R None").is_none());
        assert_eq!((l.frames, l.rx_errors), (1, 1));
    }

    #[test]
    fn csv_rows_and_header_agree() {
        let row = Row {
            host_s: 1.25,
            t_us: 99,
            rssi: -90,
            snr_db: -2.5,
            len: 255,
            image: Some(3),
            index: Some(7),
            status: Status::Bad("a, b".into()),
            toa_ms: 399.616,
        };
        let line = row.csv();
        assert_eq!(line, "1.250,99,-90,-2.50,255,3,7,bad,399.62,a; b");
        assert_eq!(line.split(',').count(), RX_HEADER.split(',').count());
        let j = jpeg(300);
        let mut l = logger(Mode::Chunk, Some(2), None);
        for (i, fr) in frames(Mode::Chunk, &j).into_iter().enumerate() {
            l.feed(0.0, &rx(i as u64 * 1000, fr));
        }
        let csv = images_csv(&l.finish().0);
        let cols = csv.lines().nth(1).unwrap().split(',').count();
        assert_eq!(cols, IMAGES_HEADER.split(',').count());
    }
}
