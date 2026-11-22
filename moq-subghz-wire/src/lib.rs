//! Wire format for streaming live video over a <=255-byte-frame radio link (SX1276 FSK).
//!
//! The model follows MoQ (Media over QUIC Transport): a **track** is a sequence of
//! **groups**, a group starts at a keyframe (one H.264 GOP) and holds **objects**
//! (one encoded access unit / video frame each). Objects are split into radio frames
//! with a 12-byte header; optional XOR parity adds one repair frame per `k` data frames.
//! A receiver that loses an object it cannot repair drops the rest of that group and
//! resumes at the next group, exactly like a MoQ subscriber joining at a group boundary.
//!
//! Radio frame (big endian):
//! ```text
//!  0..2  seq     u16  link frame counter (loss accounting)
//!  2..4  group   u16  group id (GOP)
//!  4     obj     u8   object id within the group (0 = keyframe)
//!  5     idx     u8   data fragment index, or parity block index if PARITY
//!  6     nfrag   u8   number of data fragments in the object
//!  7     flags   u8   bit0 PARITY, bits1..3 version (5), bits4..7 k (FEC block, 0 = none)
//!  8..10 objlen  u16  object length in bytes
//! 10..12 crc     u16  CRC-16/CCITT-FALSE over the whole frame with this field zeroed
//! 12..   payload     <= 243 bytes; an object is cut into `nfrag = ceil(len/243)` equal-size
//!                    chunks, frames may be zero-padded up to a minimum length (see below)
//! ```
//! Object payload: 17-byte [`ObjHdr`] followed by the H.264 Annex-B access unit.

pub mod annexb;
pub mod objstream;
#[cfg(feature = "seal")]
pub mod seal;
pub mod stamp;

/// Longest RP2040-based device / replay line we will buffer. A 255-byte radio frame is about
/// 550 bytes once it is hex-encoded on a `R` line; anything longer is not a frame
/// this link can carry.
pub const MAX_SERIAL_LINE: usize = 1024;

/// One line from [`read_line_limited`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// Bytes up to and including the newline, when there was one.
    Data(Vec<u8>),
    /// The line exceeded `cap` and was discarded through its newline.
    TooLong,
}

/// Read one line without buffering more than `cap` bytes.
///
/// `Ok(None)` is EOF. Unlike [`std::io::BufRead::read_line`], a peer that never
/// sends a newline cannot grow memory without bound: the oversize input is
/// dropped and reported as [`Line::TooLong`].
pub fn read_line_limited(
    r: &mut impl std::io::BufRead,
    cap: usize,
) -> std::io::Result<Option<Line>> {
    let mut buf = Vec::new();
    let mut skipping = false;
    loop {
        let available = match r.fill_buf() {
            Ok(a) => a,
            // a signal, not an error: std's `read_line` retries too
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if available.is_empty() {
            if skipping || buf.is_empty() {
                return Ok(None);
            }
            return Ok(Some(Line::Data(buf)));
        }
        if let Some(i) = available.iter().position(|&b| b == b'\n') {
            let take = i + 1;
            if skipping || buf.len().saturating_add(take) > cap {
                r.consume(take);
                return Ok(Some(Line::TooLong));
            }
            buf.extend_from_slice(&available[..take]);
            r.consume(take);
            return Ok(Some(Line::Data(buf)));
        }
        if skipping || buf.len().saturating_add(available.len()) > cap {
            skipping = true;
            let n = available.len();
            r.consume(n);
            buf.clear();
            continue;
        }
        buf.extend_from_slice(available);
        let n = available.len();
        r.consume(n);
    }
}

/// Decode a hex string into bytes. `None` for odd length or any non-hex character.
///
/// Works on bytes, not `str` slices: serial lines are decoded lossily, so a stray
/// byte becomes a multi-byte U+FFFD and a `&s[2 * i..2 * i + 2]` slice would panic
/// on the char boundary. An empty string is an empty vector.
pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let b = s.as_bytes();
    if !b.len().is_multiple_of(2) {
        return None;
    }
    // Sized up front: this also decodes key files, and a growing Vec would leave
    // copies of the key in freed memory.
    let mut out = Vec::with_capacity(b.len() / 2);
    // Even length was checked above, so the remainder is empty.
    for p in b.as_chunks::<2>().0 {
        match (val(p[0]), val(p[1])) {
            (Some(hi), Some(lo)) => out.push(hi << 4 | lo),
            _ => {
                // what was decoded so far may be part of a key
                #[cfg(feature = "seal")]
                seal::wipe(&mut out);
                return None;
            }
        }
    }
    Some(out)
}

/// One received frame as reported by the RP2040-based device firmware, the Pi receiver thread or
/// the loopback file: `R <t_us> <rssi> <snr> <hex>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RLine {
    /// Timestamp in microseconds: the stick's clock for an RP2040-based device line, time since the run
    /// started for a loopback file.
    pub t_us: u64,
    /// RSSI in dBm.
    pub rssi: i32,
    /// SNR in quarter dB (the SX1276 reports 0.25 dB steps). Writers without an SNR put 0.
    pub snr: i32,
    pub frame: Vec<u8>,
}

/// Parse an `R` line, strictly: `R <t_us> <rssi> <snr> [<hex>]` with every number valid, no
/// extra token, and a hex field that is whole bytes and at most one radio frame. `None` for
/// anything else, so every tool accepts and rejects the same lines (the hex field may be absent
/// for an empty frame).
pub fn parse_r_line(l: &str) -> Option<RLine> {
    let mut it = l.strip_prefix("R ")?.split(' ');
    let t_us = it.next()?.parse().ok()?;
    let rssi = it.next()?.parse().ok()?;
    let snr = it.next()?.parse().ok()?;
    let hx = it.next().unwrap_or("");
    if it.next().is_some() || hx.len() > 2 * MAX_FRAME {
        return None;
    }
    Some(RLine {
        t_us,
        rssi,
        snr,
        frame: decode_hex(hx)?,
    })
}

/// `BITRATE:FDEV:PREAMBLE[:x]`. `x` is the only accepted suffix (datasheet limits off).
/// Bitrate must be non-zero so airtime math cannot divide by zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FskProfile {
    pub bitrate: u32,
    pub fdev: u32,
    pub preamble: u16,
    pub unchecked: bool,
}

pub fn parse_fsk_profile(s: &str) -> Result<FskProfile, &'static str> {
    let p: Vec<&str> = s.split(':').collect();
    if p.len() < 3 || p.len() > 4 || (p.len() == 4 && p[3] != "x") {
        return Err("profile must be BITRATE:FDEV:PREAMBLE[:x]");
    }
    let bitrate: u32 = p[0].parse().map_err(|_| "bad bitrate")?;
    let fdev: u32 = p[1].parse().map_err(|_| "bad fdev")?;
    let preamble: u16 = p[2].parse().map_err(|_| "bad preamble")?;
    if bitrate == 0 {
        return Err("bitrate must be non-zero");
    }
    Ok(FskProfile {
        bitrate,
        fdev,
        preamble,
        unchecked: p.len() == 4,
    })
}

pub const MAX_FRAME: usize = 255;
pub const HDR_LEN: usize = 12;
pub const MAX_CHUNK: usize = MAX_FRAME - HDR_LEN;
/// Frame format version. 4: sealed objects carry a per-run salt (`seal`). 5: the key label
/// became `moq-subghz object key v2` (the project rename), so tools of different versions refuse
/// each other's frames instead of failing every tag.
pub const VERSION: u8 = 5;
pub const MAX_OBJECT: usize = 255 * MAX_CHUNK; // 61965 B (fits u16)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHdr {
    pub seq: u16,
    pub group: u16,
    pub obj: u8,
    pub idx: u8,
    pub nfrag: u8,
    pub parity: bool,
    pub k: u8,
    pub objlen: u16,
}

/// CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF).
pub fn crc16(data: &[u8]) -> u16 {
    let mut c: u16 = 0xFFFF;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x1021
            } else {
                c << 1
            };
        }
    }
    c
}

/// End-to-end frame check. The radio's own CRC is not enough: when a packet fails CRC
/// the SX1276 silently clears its FIFO, and a polled reader that already drained part of
/// it can splice the start of the bad packet onto the next good one.
pub fn write_crc(frame: &mut [u8]) {
    frame[10] = 0;
    frame[11] = 0;
    let c = crc16(frame);
    frame[10..12].copy_from_slice(&c.to_be_bytes());
}

impl FrameHdr {
    /// Header with a zero CRC field; call [`write_crc`] once the payload is appended.
    pub fn encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.group.to_be_bytes());
        out.extend_from_slice(&[
            self.obj,
            self.idx,
            self.nfrag,
            (self.parity as u8) | (VERSION << 1) | (self.k << 4),
        ]);
        out.extend_from_slice(&self.objlen.to_be_bytes());
        out.extend_from_slice(&[0, 0]);
    }

    /// Parse and sanity-check a radio frame. `None` for anything that is not ours.
    ///
    /// Frames longer than [`MAX_FRAME`] are rejected before the CRC copy. The SX1276
    /// cannot deliver a longer payload; accepting one would let a hostile replay or
    /// serial peer force an unbounded allocation.
    pub fn decode(b: &[u8]) -> Option<(FrameHdr, &[u8])> {
        if b.len() < HDR_LEN + 1 || b.len() > MAX_FRAME || (b[7] >> 1) & 7 != VERSION {
            return None;
        }
        let mut z = b.to_vec();
        z[10] = 0;
        z[11] = 0;
        if crc16(&z) != u16::from_be_bytes([b[10], b[11]]) {
            return None;
        }
        let h = FrameHdr {
            seq: u16::from_be_bytes([b[0], b[1]]),
            group: u16::from_be_bytes([b[2], b[3]]),
            obj: b[4],
            idx: b[5],
            nfrag: b[6],
            parity: b[7] & 1 != 0,
            k: b[7] >> 4,
            objlen: u16::from_be_bytes([b[8], b[9]]),
        };
        if h.nfrag == 0 || (h.objlen as usize).div_ceil(MAX_CHUNK) != h.nfrag as usize {
            return None;
        }
        if h.parity {
            if h.k == 0 || h.idx as usize >= (h.nfrag as usize).div_ceil(h.k as usize) {
                return None;
            }
        } else if h.idx >= h.nfrag {
            return None;
        }
        Some((h, &b[HDR_LEN..]))
    }

    /// Data chunks are equal-sized (`ceil(objlen / nfrag)`), so no fragment is much shorter
    /// than the others: a very short frame right behind a long one is the frame most likely
    /// to be missed by a receiver that is still busy with the long one.
    pub fn chunk_size(&self) -> usize {
        if self.nfrag == 0 {
            return 0;
        }
        (self.objlen as usize).div_ceil(self.nfrag as usize)
    }
    pub fn chunk_len(&self, idx: usize) -> usize {
        let cs = self.chunk_size();
        if cs == 0 || idx >= self.nfrag as usize {
            return 0;
        }
        (self.objlen as usize)
            .saturating_sub(idx.saturating_mul(cs))
            .min(cs)
    }
}

/// Per-object header carried at the start of every object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjHdr {
    /// Source frame number (counts every captured frame).
    pub frame_no: u32,
    /// Capture time, sender wall clock, unix microseconds.
    pub cap_us: u64,
    /// Encoder delay (encoder output time - capture time), microseconds.
    pub enc_us: u32,
    pub key: bool,
}
pub const OBJ_HDR_LEN: usize = 17;

impl ObjHdr {
    pub fn encode(&self) -> [u8; OBJ_HDR_LEN] {
        let mut b = [0u8; OBJ_HDR_LEN];
        b[0..4].copy_from_slice(&self.frame_no.to_be_bytes());
        b[4..12].copy_from_slice(&self.cap_us.to_be_bytes());
        b[12..16].copy_from_slice(&self.enc_us.to_be_bytes());
        b[16] = self.key as u8;
        b
    }
    pub fn decode(b: &[u8]) -> Option<(ObjHdr, &[u8])> {
        if b.len() < OBJ_HDR_LEN {
            return None;
        }
        Some((
            ObjHdr {
                frame_no: u32::from_be_bytes(b[0..4].try_into().unwrap()),
                cap_us: u64::from_be_bytes(b[4..12].try_into().unwrap()),
                enc_us: u32::from_be_bytes(b[12..16].try_into().unwrap()),
                key: b[16] != 0,
            },
            &b[OBJ_HDR_LEN..],
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum WireError {
    Empty,
    TooBig(usize),
    BadK,
}

/// Splits objects into radio frames (+ XOR parity).
pub struct Fragmenter {
    pub seq: u16,
    /// FEC block size: one parity frame after every `k` data frames (0 = no FEC, max 15).
    pub k: u8,
    /// Zero-pad shorter frames to this many bytes (receivers truncate by `objlen`).
    pub min_frame: usize,
}

impl Fragmenter {
    pub fn new(k: u8) -> Result<Self, WireError> {
        if k > 15 {
            return Err(WireError::BadK);
        }
        Ok(Fragmenter {
            seq: 0,
            k,
            min_frame: 0,
        })
    }

    pub fn fragment(
        &mut self,
        group: u16,
        obj: u8,
        data: &[u8],
    ) -> Result<Vec<Vec<u8>>, WireError> {
        if data.is_empty() {
            return Err(WireError::Empty);
        }
        if data.len() > MAX_OBJECT {
            return Err(WireError::TooBig(data.len()));
        }
        let cs = data.len().div_ceil(data.len().div_ceil(MAX_CHUNK));
        let chunks: Vec<&[u8]> = data.chunks(cs).collect();
        let nfrag = chunks.len() as u8;
        let blk = if self.k == 0 {
            chunks.len()
        } else {
            self.k as usize
        };
        let mut out = Vec::with_capacity(chunks.len() + chunks.len().div_ceil(blk));
        for (b, block) in chunks.chunks(blk).enumerate() {
            for (j, c) in block.iter().enumerate() {
                out.push(self.frame(group, obj, (b * blk + j) as u8, nfrag, false, data.len(), c));
            }
            if self.k > 0 {
                let mut p = vec![0u8; block[0].len()];
                for c in block {
                    for (x, y) in p.iter_mut().zip(c.iter()) {
                        *x ^= y;
                    }
                }
                out.push(self.frame(group, obj, b as u8, nfrag, true, data.len(), &p));
            }
        }
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn frame(
        &mut self,
        group: u16,
        obj: u8,
        idx: u8,
        nfrag: u8,
        parity: bool,
        objlen: usize,
        pl: &[u8],
    ) -> Vec<u8> {
        let mut f = Vec::with_capacity(HDR_LEN + pl.len());
        FrameHdr {
            seq: self.seq,
            group,
            obj,
            idx,
            nfrag,
            parity,
            k: self.k,
            objlen: objlen as u16,
        }
        .encode(&mut f);
        self.seq = self.seq.wrapping_add(1);
        f.extend_from_slice(pl);
        if f.len() < self.min_frame.min(MAX_FRAME) {
            f.resize(self.min_frame.min(MAX_FRAME), 0);
        }
        write_crc(&mut f);
        f
    }
}

/// The frame plan for one object: the length in bytes of each frame
/// [`Fragmenter::fragment`] makes for an object of `len` bytes at FEC block size `k` and
/// padding `min_frame`, in the order it sends them (data frames of a block, then its parity).
/// Pure arithmetic over the frame constants, for airtime and overhead math without data.
pub fn plan(len: usize, k: u8, min_frame: usize) -> Result<Vec<usize>, WireError> {
    if k > 15 {
        return Err(WireError::BadK);
    }
    if len == 0 {
        return Err(WireError::Empty);
    }
    if len > MAX_OBJECT {
        return Err(WireError::TooBig(len));
    }
    let n = len.div_ceil(MAX_CHUNK);
    let cs = len.div_ceil(n);
    // chunk `i` of `n`: all are `cs` bytes except the last
    let chunk = |i: usize| if i + 1 < n { cs } else { len - cs * (n - 1) };
    let frame = |payload: usize| (HDR_LEN + payload).max(min_frame.min(MAX_FRAME));
    let blk = if k == 0 { n } else { k as usize };
    let mut out = Vec::with_capacity(n + n.div_ceil(blk));
    for b in 0..n.div_ceil(blk) {
        let r = b * blk..((b + 1) * blk).min(n);
        out.extend(r.clone().map(|i| frame(chunk(i))));
        if k > 0 {
            // parity is as long as the block's first chunk
            out.push(frame(chunk(r.start)));
        }
    }
    Ok(out)
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Complete object (`recovered` = data fragments rebuilt from parity).
    Object {
        group: u16,
        obj: u8,
        data: Vec<u8>,
        recovered: u32,
    },
    /// Object abandoned: `have` of `need` data fragments.
    Lost {
        group: u16,
        obj: u8,
        have: u32,
        need: u32,
    },
}

struct Partial {
    h: FrameHdr,
    chunks: Vec<Option<Vec<u8>>>,
    parity: Vec<Option<Vec<u8>>>,
    recovered: u32,
}

impl Partial {
    fn new(h: FrameHdr) -> Self {
        let nb = if h.k == 0 {
            0
        } else {
            (h.nfrag as usize).div_ceil(h.k as usize)
        };
        Partial {
            h,
            chunks: vec![None; h.nfrag as usize],
            parity: vec![None; nb],
            recovered: 0,
        }
    }
    fn try_recover(&mut self) {
        let k = self.h.k as usize;
        for b in 0..self.parity.len() {
            let Some(p) = &self.parity[b] else { continue };
            let range = b * k..((b + 1) * k).min(self.chunks.len());
            let missing: Vec<usize> = range
                .clone()
                .filter(|&i| self.chunks[i].is_none())
                .collect();
            if missing.len() != 1 {
                continue;
            }
            let m = missing[0];
            let want = self.h.chunk_len(m);
            if p.len() < want {
                continue;
            }
            let mut r = p.clone();
            let mut intact = true;
            for i in range.filter(|&i| i != m) {
                let Some(chunk) = self.chunks[i].as_ref() else {
                    intact = false;
                    break;
                };
                for (x, y) in r.iter_mut().zip(chunk.iter()) {
                    *x ^= y;
                }
            }
            if !intact {
                continue;
            }
            r.truncate(want);
            self.chunks[m] = Some(r);
            self.recovered += 1;
        }
    }
    fn have(&self) -> u32 {
        self.chunks.iter().filter(|c| c.is_some()).count() as u32
    }
    fn complete(&self) -> bool {
        self.chunks.iter().all(|c| c.is_some())
    }
    fn into_event(mut self) -> Event {
        self.try_recover();
        if self.complete() {
            Event::Object {
                group: self.h.group,
                obj: self.h.obj,
                recovered: self.recovered,
                data: self.chunks.into_iter().flatten().flatten().collect(),
            }
        } else {
            Event::Lost {
                group: self.h.group,
                obj: self.h.obj,
                have: self.have(),
                need: self.h.nfrag as u32,
            }
        }
    }
}

/// Largest forward step one frame may make on its own, in frames (about 0.45 s at
/// 300 kbit/s). A larger step moves the baseline only when the next frame continues from
/// it, so one garbled, injected or replayed sequence number can't hide loss, and can add
/// at most `SEQ_STEP - 1` frames to it (a step that small looks like honest loss).
///
/// Limits: sequence numbers are not authenticated, so two consecutive forged frames far
/// ahead are a confirmed gap (headers stay clear by design; see REVIEW.md, "Radio
/// headers and FEC are outside the AEAD"); and a 16-bit counter can't tell a frame more than 32768
/// behind (about 4 minutes at 300 kbit/s) from one ahead, so consecutive replays that old
/// count as a gap too.
pub const SEQ_STEP: u16 = 64;

/// How far behind the newest a frame counts as `late` (reordered or replayed) rather
/// than `stray`. Neither moves the baseline.
pub const SEQ_WINDOW: u16 = 1024;

/// Rebuilds objects from radio frames. Objects arrive one after another from a single
/// sender, so a frame from a new object finalizes the previous one.
#[derive(Default)]
pub struct Reassembler {
    cur: Option<Partial>,
    done: Option<(u16, u8)>,
    last_seq: Option<u16>,
    /// The last two frames far ahead of the baseline, either of which a following frame
    /// may confirm as a real gap; two, so one injected frame can't displace a real one.
    jumps: [Option<u16>; 2],
    /// Before a baseline exists: the last two frames, either of which a following frame
    /// may confirm as the start.
    starts: [Option<u16>; 2],
    /// Frames that fit their object (`ok`) or contradicted it (`bad`). Every CRC-valid frame
    /// that is not a duplicate lands in exactly one of `ok`, `bad` and `stale`, once it is
    /// no longer held; a `late` or `stray` frame is also counted in one of them.
    pub frames_ok: u64,
    /// Sequence numbers skipped. Loss assumes in-order delivery, which the radio gives: a
    /// frame that arrives late was already counted missing when its gap was seen.
    pub frames_missing: u64,
    pub frames_bad: u64,
    pub frames_dup: u64,
    /// Frames a little behind the newest (within [`SEQ_WINDOW`]): reordered or replayed.
    pub frames_late: u64,
    /// Frames further behind, or far ahead with no frame continuing from them: garbled,
    /// injected or replayed sequence numbers, not a gap. The baseline never moves back,
    /// so a sender that restarts its counter mid-run is not followed: restart the
    /// receiver with it, as the bench scripts do.
    pub frames_stray: u64,
    /// Frames for an object behind the newest one started (replayed or very late), or far
    /// ahead and dropped from the hold unconfirmed (also at [`Reassembler::finish`]). They
    /// are not reassembled, so a replay shorter than [`RECOVER_RUN`] frames can't deliver
    /// an old object again or finalize the object in progress.
    pub frames_stale: u64,
    pub fec_recovered: u64,
    /// The newest object a frame was placed for.
    newest: Option<(u16, u8)>,
    /// Frames far ahead of `newest` (or any frame before the first is placed), held until
    /// a frame confirms the jump.
    held: [Option<Vec<u8>>; 2],
    /// `newest` came from a frame released on idle with nothing confirming it, so a
    /// confirmed pair of frames anywhere may replace it.
    tentative: bool,
    /// The last stale frame behind `newest`, and how many stale frames in a row have
    /// each continued from the one before.
    behind: Option<(u16, u8)>,
    behind_run: u32,
}

/// Stale frames in a row, each continuing from the last, after which the receiver
/// follows them: if two injected frames confirmed a jump to the wrong place, the real
/// stream takes over again within this many frames (a few objects) instead of never.
/// A replay shorter than this stays stale.
pub const RECOVER_RUN: u32 = 32;

/// Largest jump in group id a frame may make on its own. With [`OBJ_NEAR`], this bounds
/// what one injected frame can cost to the rest of the current group and the next one
/// or two: their frames then count as stale. A frame further ahead is held until the
/// next frame confirms it, so a long honest outage costs nothing and an injected frame
/// can't stall the stream.
pub const GROUP_STEP: u16 = 2;

/// Highest object id a frame from a later group may carry and be placed without
/// confirmation (a new group starts at object 0; this allows for its first few lost).
pub const OBJ_NEAR: u8 = 8;

/// Where object `b` stands relative to object `a`.
#[derive(PartialEq)]
enum Pos {
    Same,
    /// The same group further on, or an early object up to `GROUP_STEP` groups ahead.
    Ahead,
    FarAhead,
    Behind,
}

fn pos(a: (u16, u8), b: (u16, u8)) -> Pos {
    let d = b.0.wrapping_sub(a.0);
    match d {
        0 if b.1 == a.1 => Pos::Same,
        0 if b.1 > a.1 => Pos::Ahead,
        0 => Pos::Behind,
        d if d <= GROUP_STEP && b.1 < OBJ_NEAR => Pos::Ahead,
        d if d < 0x8000 => Pos::FarAhead,
        _ => Pos::Behind,
    }
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, frame: &[u8]) -> Vec<Event> {
        let mut ev = Vec::new();
        let Some((h, pl)) = FrameHdr::decode(frame) else {
            self.frames_bad += 1;
            return ev;
        };
        if !self.track_seq(h.seq) {
            return ev; // duplicate
        }
        let key = (h.group, h.obj);
        // Nothing is placed until a second frame confirms the first, so one injected
        // frame can't set where the stream starts.
        let at = self.newest.map_or(Pos::FarAhead, |n| pos(n, key));
        let at = if self.tentative && at == Pos::Behind {
            Pos::FarAhead
        } else {
            at
        };
        // A frame that continues from a held one confirms it, even if it is also close
        // enough to `newest` to be placed on its own: the held frame goes first.
        let held_key = |f: &Vec<u8>| FrameHdr::decode(f).map(|(x, _)| (x.group, x.obj));
        let confirms = (at != Pos::Behind)
            .then(|| {
                self.held.iter().position(|f| {
                    f.as_ref()
                        .and_then(held_key)
                        .is_some_and(|k| matches!(pos(k, key), Pos::Same | Pos::Ahead))
                })
            })
            .flatten();
        if let Some(i) = confirms {
            // the jump is real: place the held frame first, then this one
            let held = self.held[i].take().unwrap();
            self.frames_stale += self.held.iter().flatten().count() as u64;
            self.held = [None, None];
            if self.tentative {
                self.newest = None;
            }
            (self.tentative, self.behind, self.behind_run) = (false, None, 0);
            let (hh, hpl) = FrameHdr::decode(&held).unwrap();
            self.place_counted(hh, hpl, &mut ev);
            self.place_counted(h, pl, &mut ev);
            return ev;
        }
        match at {
            Pos::Same | Pos::Ahead => {
                (self.tentative, self.behind, self.behind_run) = (false, None, 0);
                self.frames_stale += self.held.iter().flatten().count() as u64;
                self.held = [None, None];
                self.place_counted(h, pl, &mut ev);
            }
            Pos::Behind => {
                let continues = self
                    .behind
                    .is_some_and(|b| matches!(pos(b, key), Pos::Same | Pos::Ahead));
                self.behind_run = if continues { self.behind_run + 1 } else { 1 };
                self.behind = Some(key);
                if self.behind_run >= RECOVER_RUN {
                    (self.newest, self.behind, self.behind_run) = (None, None, 0);
                    self.frames_stale += self.held.iter().flatten().count() as u64;
                    self.held = [None, None];
                    self.place_counted(h, pl, &mut ev);
                } else {
                    self.frames_stale += 1;
                }
            }
            Pos::FarAhead => {
                self.frames_stale += self.held[0].is_some() as u64;
                self.held = [self.held[1].take(), Some(frame.to_vec())];
            }
        }
        ev
    }

    /// Place a frame and count it in exactly one of frames_ok and frames_bad.
    fn place_counted(&mut self, h: FrameHdr, pl: &[u8], ev: &mut Vec<Event>) {
        let key = (h.group, h.obj);
        if self
            .newest
            .is_none_or(|n| matches!(pos(n, key), Pos::Ahead | Pos::FarAhead))
        {
            self.newest = Some(key);
        }
        if self.place(h, pl, ev) {
            self.frames_ok += 1;
        } else {
            self.frames_bad += 1;
        }
    }

    /// Loss accounting for one frame's sequence number; false for a duplicate.
    fn track_seq(&mut self, seq: u16) -> bool {
        let Some(last) = self.last_seq else {
            // The baseline starts only where a frame follows an earlier one, so a stray
            // first frame can't make the honest stream look like a long gap.
            if self.starts.contains(&Some(seq)) {
                self.frames_dup += 1;
                return false;
            }
            let follows = self
                .starts
                .iter()
                .flatten()
                .copied()
                .find(|&c| (1..=SEQ_STEP).contains(&seq.wrapping_sub(c)));
            match follows {
                Some(c) => {
                    self.frames_missing += seq.wrapping_sub(c) as u64 - 1;
                    self.frames_stray +=
                        self.starts.iter().flatten().filter(|&&x| x != c).count() as u64;
                    self.last_seq = Some(seq);
                    self.starts = [None, None];
                }
                None => {
                    if self.starts[0].is_some() {
                        self.frames_stray += 1; // the oldest candidate was never followed
                    }
                    self.starts = [self.starts[1], Some(seq)];
                }
            }
            return true;
        };
        let d = seq.wrapping_sub(last);
        if d == 0 {
            self.frames_dup += 1;
            return false;
        } else if d <= SEQ_STEP {
            // the stream went on from the baseline: pending jumps were strays
            self.frames_stray += self.jumps.iter().flatten().count() as u64;
            self.jumps = [None, None];
            self.frames_missing += d as u64 - 1;
            self.last_seq = Some(seq);
        } else if d >= 0x8000 {
            // behind: late if close, stray if far; never moves the baseline back
            if d >= SEQ_WINDOW.wrapping_neg() {
                self.frames_late += 1;
            } else {
                self.frames_stray += 1;
            }
        } else {
            let follows = self
                .jumps
                .iter()
                .flatten()
                .copied()
                .find(|&j| (1..=SEQ_STEP).contains(&seq.wrapping_sub(j)));
            match follows {
                Some(j) => {
                    // confirmed by a frame that follows it: a real gap up to the jump,
                    // and any frames between the jump and this one
                    self.frames_missing += j.wrapping_sub(last) as u64 - 1;
                    self.frames_missing += seq.wrapping_sub(j) as u64 - 1;
                    self.frames_stray +=
                        self.jumps.iter().flatten().filter(|&&x| x != j).count() as u64;
                    self.last_seq = Some(seq);
                    self.jumps = [None, None];
                }
                None => {
                    if self.jumps.contains(&Some(seq)) {
                        self.frames_dup += 1;
                        return false;
                    }
                    if self.jumps[0].is_some() {
                        self.frames_stray += 1; // the oldest was never followed
                    }
                    self.jumps = [self.jumps[1], Some(seq)];
                }
            }
        }
        true
    }

    /// Store a CRC-valid frame in its object; false when it contradicts the object.
    fn place(&mut self, h: FrameHdr, pl: &[u8], ev: &mut Vec<Event>) -> bool {
        let key = (h.group, h.obj);
        if self.done == Some(key) {
            return true; // trailing parity of an object already delivered
        }
        if self
            .cur
            .as_ref()
            .is_some_and(|p| (p.h.group, p.h.obj) != key)
        {
            ev.push(self.cur.take().unwrap().into_event());
        }
        let p = self.cur.get_or_insert_with(|| Partial::new(h));
        if p.h.objlen != h.objlen || p.h.k != h.k {
            return false;
        }
        if h.parity {
            let Some(slot) = p.parity.get_mut(h.idx as usize) else {
                return false;
            };
            let n = pl.len().min(h.chunk_size());
            slot.get_or_insert_with(|| pl[..n].to_vec());
        } else {
            let n = h.chunk_len(h.idx as usize);
            if pl.len() < n {
                return false;
            }
            let Some(slot) = p.chunks.get_mut(h.idx as usize) else {
                return false;
            };
            slot.get_or_insert_with(|| pl[..n].to_vec());
        }
        p.try_recover();
        if p.complete() {
            let p = self.cur.take().unwrap();
            self.done = Some(key);
            self.fec_recovered += p.recovered as u64;
            ev.push(p.into_event());
        }
        true
    }

    /// End of stream: count frames still held as stale and finalize the object in
    /// progress.
    pub fn finish(&mut self) -> Option<Event> {
        if self.newest.is_some() {
            self.frames_stale += self.held.iter().flatten().count() as u64;
            self.held = [None, None];
        }
        self.flush()
    }

    /// Finalize the object in progress (call when the link has been idle a while). Before
    /// anything is placed, the last held frame is placed first, tentatively: a stream
    /// that starts with a one-frame object and then pauses still delivers it.
    pub fn flush(&mut self) -> Option<Event> {
        if self.newest.is_none() {
            if let Some(f) = self.held[1].take().or_else(|| self.held[0].take()) {
                self.frames_stale += self.held.iter().flatten().count() as u64;
                self.held = [None, None];
                let (h, pl) = FrameHdr::decode(&f).expect("held frames decode");
                let mut ev = Vec::new();
                self.place_counted(h, pl, &mut ev);
                self.tentative = true;
                if let Some(e) = ev.pop() {
                    return Some(e); // a one-frame object, complete already
                }
            }
        }
        let p = self.cur.take()?;
        self.done = Some((p.h.group, p.h.obj));
        let e = p.into_event();
        if let Event::Object { recovered, .. } = &e {
            self.fec_recovered += *recovered as u64;
        }
        Some(e)
    }
}

/// Largest jump in source frame number between two consecutive objects that still
/// counts as one stretch of video: about 17 s at 15 fps.
pub const FRAME_STEP: u32 = 256;

/// Largest step from one object to the next that may start the range or end it.
pub const FRAME_EDGE_STEP: u32 = 8;

/// The range of source frame numbers a run covered, for the expected-frame count.
///
/// `frame_no` is unauthenticated without a key, so one garbled or injected object must
/// not stretch the range (a frame number of `u32::MAX` once made a run expect 2^32
/// frames). A frame number joins the range only when the next object follows it (a
/// larger number, at most [`FRAME_STEP`] ahead), so a lone wrong number in the middle of
/// a run changes nothing. The start needs a step of at most [`FRAME_EDGE_STEP`], and the
/// last object counts if it is at most that far past the range: a wrong number right
/// before the first or after the last real one can stretch the range by at most
/// `FRAME_EDGE_STEP`. Real objects more than that apart at either edge (a lone first or
/// last object beside an outage) are left out, which can understate loss, never
/// overstate it.
#[derive(Default)]
pub struct FrameRange {
    first: Option<u32>,
    max: u32,
    /// Recent frame numbers past the range, any of which the next object may confirm.
    pending: [Option<u32>; 3],
    /// Frame numbers never confirmed: garbled, injected, or before the range.
    pub rejected: u64,
}

impl FrameRange {
    /// Note one decoded object's frame number.
    pub fn observe(&mut self, f: u32) {
        if let Some(a) = self.first {
            if f < a {
                self.rejected += 1;
                return;
            }
            if f <= self.max {
                return; // a repeat or a late object inside the range
            }
        }
        if self.pending.contains(&Some(f)) {
            return;
        }
        let step = if self.first.is_some() {
            FRAME_STEP
        } else {
            FRAME_EDGE_STEP
        };
        let confirmed = self
            .pending
            .iter()
            .flatten()
            .copied()
            .filter(|&c| f > c && f - c <= step)
            .max();
        match confirmed {
            Some(c) => {
                self.first.get_or_insert(c);
                self.max = self.max.max(c);
                let max = self.max;
                self.rejected += self.pending.iter().flatten().filter(|&&x| x > max).count() as u64;
                self.pending = [None, None, Some(f)];
            }
            None => {
                let max = self.max;
                let past = |x: u32| self.first.is_none() || x > max;
                self.rejected += self.pending[0].is_some_and(past) as u64;
                self.pending = [self.pending[1], self.pending[2], Some(f)];
            }
        }
    }

    /// The last frame number of the range, including the last object if it follows.
    fn end(&self) -> Option<u32> {
        let max = self.max;
        self.first?;
        let tail = self
            .pending
            .iter()
            .flatten()
            .copied()
            .filter(|&p| p > max && p - max <= FRAME_EDGE_STEP)
            .min();
        Some(tail.unwrap_or(max))
    }

    /// Whether `f` lies in the confirmed range.
    pub fn contains(&self, f: u32) -> bool {
        match (self.first, self.end()) {
            (Some(a), Some(b)) => (a..=b).contains(&f),
            _ => false,
        }
    }

    /// Frames the source captured over the confirmed range.
    pub fn expected(&self) -> u64 {
        match (self.first, self.end()) {
            (Some(a), Some(b)) => (b - a) as u64 + 1,
            _ => 0,
        }
    }
}

/// MoQ-style delivery gate: passes objects in order, and after a lost object (or a gap in
/// object ids) drops the rest of that group until the next group's object 0.
#[derive(Default)]
pub struct GroupGate {
    next: Option<(u16, u8)>,
    broken: Option<u16>,
    pub groups_cut: u64,
    pub objects_skipped: u64,
}

impl GroupGate {
    /// Returns true when the object should be handed to the decoder.
    pub fn accept(&mut self, group: u16, obj: u8) -> bool {
        let ok = obj == 0 || (self.next == Some((group, obj)) && self.broken != Some(group));
        if !ok {
            if self.broken != Some(group) {
                self.groups_cut += 1;
            }
            self.broken = Some(group);
            self.objects_skipped += 1;
        } else if obj == 0 {
            self.broken = None;
        }
        self.next = Some((group, obj.wrapping_add(1)));
        ok
    }
    /// Report an unrecoverable object.
    pub fn lost(&mut self, group: u16, obj: u8) {
        if self.broken != Some(group) {
            self.groups_cut += 1;
        }
        self.broken = Some(group);
        self.next = Some((group, obj.wrapping_add(1)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_crc_for_test(f: &mut [u8]) {
        f[10] = 0;
        f[11] = 0;
        let c = crc16(f);
        f[10..12].copy_from_slice(&c.to_be_bytes());
    }

    fn obj(n: usize, seed: u8) -> Vec<u8> {
        (0..n)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn roundtrip_no_fec() {
        let mut f = Fragmenter::new(0).unwrap();
        let mut r = Reassembler::new();
        let mut got = vec![];
        for (o, n) in [1usize, 242, 243, 244, 5000, MAX_OBJECT].iter().enumerate() {
            let d = obj(*n, o as u8);
            for fr in f.fragment(3, o as u8, &d).unwrap() {
                assert!(fr.len() <= MAX_FRAME);
                got.extend(r.push(&fr));
            }
            if o == 0 {
                // the very first frame waits for a second to confirm it, or for idle
                got.extend(r.flush());
            }
            assert_eq!(
                got.pop(),
                Some(Event::Object {
                    group: 3,
                    obj: o as u8,
                    data: d,
                    recovered: 0
                })
            );
        }
        assert_eq!(r.frames_missing, 0);
    }

    #[test]
    fn fec_repairs_one_per_block() {
        for k in [1u8, 2, 4, 8] {
            for n in [10usize, 243, 700, 2000, 7000] {
                let d = obj(n, k);
                let mut f = Fragmenter::new(k).unwrap();
                let frames = f.fragment(9, 4, &d).unwrap();
                // drop the first data frame of every block (incl. short last chunks)
                let nfrag = n.div_ceil(MAX_CHUNK);
                let mut r = Reassembler::new();
                let mut ev = vec![];
                for fr in &frames {
                    let (h, _) = FrameHdr::decode(fr).unwrap();
                    if !h.parity && (h.idx as usize).is_multiple_of(k as usize) {
                        continue;
                    }
                    ev.extend(r.push(fr));
                }
                ev.extend(r.flush());
                match &ev[0] {
                    Event::Object {
                        data, recovered, ..
                    } => {
                        assert_eq!(data, &d, "k={k} n={n}");
                        assert_eq!(*recovered as usize, nfrag.div_ceil(k as usize));
                    }
                    e => panic!("k={k} n={n}: {e:?}"),
                }
            }
        }
    }

    #[test]
    fn fec_repairs_short_last_chunk() {
        let d = obj(MAX_CHUNK * 3 + 17, 1);
        let mut f = Fragmenter::new(4).unwrap();
        let mut r = Reassembler::new();
        let mut ev = vec![];
        for fr in f.fragment(0, 0, &d).unwrap() {
            let (h, _) = FrameHdr::decode(&fr).unwrap();
            if !h.parity && h.idx == 3 {
                continue;
            }
            ev.extend(r.push(&fr));
        }
        assert_eq!(
            ev,
            vec![Event::Object {
                group: 0,
                obj: 0,
                data: d,
                recovered: 1
            }]
        );
    }

    #[test]
    fn loss_reported_and_seq_gap() {
        let mut f = Fragmenter::new(0).unwrap();
        let mut r = Reassembler::new();
        let a = f.fragment(1, 0, &obj(1000, 0)).unwrap();
        let b = f.fragment(1, 1, &obj(300, 1)).unwrap();
        let mut ev = vec![];
        for (i, fr) in a.iter().enumerate() {
            if i != 2 {
                ev.extend(r.push(fr));
            }
        }
        for fr in &b {
            ev.extend(r.push(fr));
        }
        assert_eq!(
            ev[0],
            Event::Lost {
                group: 1,
                obj: 0,
                have: 4,
                need: 5
            }
        );
        assert!(matches!(ev[1], Event::Object { obj: 1, .. }));
        assert_eq!(r.frames_missing, 1);
    }

    #[test]
    fn equal_chunks_and_padding() {
        let mut f = Fragmenter::new(2).unwrap();
        f.min_frame = 96;
        let mut r = Reassembler::new();
        for (o, n) in [1usize, 30, 246, 1000, 1100].iter().enumerate() {
            let d = obj(*n, 7);
            let frames = f.fragment(1, o as u8, &d).unwrap();
            let lens: Vec<usize> = frames.iter().map(|x| x.len()).collect();
            assert!(
                lens.iter().all(|l| (96..=MAX_FRAME).contains(l)),
                "{lens:?}"
            );
            assert!(
                lens.iter().max().unwrap() - lens.iter().filter(|&&l| l > 96).min().unwrap_or(&96)
                    <= 1
                    || *n < 96,
                "{lens:?}"
            );
            let mut ev = vec![];
            for (i, fr) in frames.iter().enumerate() {
                if i != 0 {
                    ev.extend(r.push(fr));
                }
            }
            ev.extend(r.flush());
            assert_eq!(
                ev.last(),
                Some(&Event::Object {
                    group: 1,
                    obj: o as u8,
                    data: d,
                    recovered: 1
                }),
                "n={n}"
            );
        }
    }

    /// Re-stamp a frame's sequence number (and its CRC), as a replaying peer would.
    fn with_seq(fr: &[u8], seq: u16) -> Vec<u8> {
        let mut f = fr.to_vec();
        f[0..2].copy_from_slice(&seq.to_be_bytes());
        write_crc_for_test(&mut f);
        f
    }

    #[test]
    fn backward_seq_does_not_skew_loss() {
        let mut f = Fragmenter::new(0).unwrap();
        let frames: Vec<Vec<u8>> = (0..4)
            .flat_map(|o| f.fragment(1, o, &obj(100, o)).unwrap())
            .collect(); // seq 0..=3, one frame per object
        let mut r = Reassembler::new();
        let mut ev = vec![];
        for fr in &frames[..3] {
            ev.extend(r.push(fr));
        }
        // a stale copy of frame 2 stamped far in the past
        ev.extend(r.push(&with_seq(&frames[2], 2u16.wrapping_sub(0x7000))));
        ev.extend(r.push(&frames[3]));
        ev.extend(r.flush());
        assert_eq!(
            r.frames_missing, 0,
            "one old seq made honest frames look lost"
        );
        assert_eq!(r.frames_stray, 1);
        assert_eq!(
            ev.iter()
                .filter(|e| matches!(e, Event::Object { .. }))
                .count(),
            4
        );

        // a late frame still completes its object
        let mut f = Fragmenter::new(0).unwrap();
        let a = f.fragment(2, 0, &obj(500, 1)).unwrap(); // 3 frames, seq 0..=2
        let mut r = Reassembler::new();
        let mut ev = vec![];
        for fr in [&a[0], &a[2], &a[1]] {
            ev.extend(r.push(fr));
        }
        assert!(matches!(ev.last(), Some(Event::Object { .. })), "{ev:?}");
        assert_eq!((r.frames_late, r.frames_missing), (1, 1));
    }

    /// Frames with sequence numbers `seqs`, each a one-frame object.
    fn seq_frames(seqs: &[u16]) -> Vec<Vec<u8>> {
        let mut f = Fragmenter::new(0).unwrap();
        seqs.iter()
            .enumerate()
            .map(|(i, &s)| with_seq(&f.fragment(1, i as u8, &obj(50, 1)).unwrap()[0], s))
            .collect()
    }
    fn account(seqs: &[u16]) -> Reassembler {
        let mut r = Reassembler::new();
        for fr in seq_frames(seqs) {
            r.push(&fr);
        }
        r
    }

    #[test]
    fn one_forward_seq_can_neither_fake_nor_hide_loss() {
        // spoofed far ahead, then honest frames: no gap, and later loss still counts
        let r = account(&[0, 0x7000, 1, 2, 4, 5]);
        assert_eq!((r.frames_missing, r.frames_stray, r.frames_late), (1, 1, 0));
    }

    #[test]
    fn a_real_long_gap_is_counted_once_confirmed() {
        // an outage of 5000 frames: the frame after the gap confirms it
        let r = account(&[0, 1, 5002, 5003, 5004]);
        assert_eq!((r.frames_missing, r.frames_stray), (5000, 0));
        // a gap at the very end, never confirmed, is not counted (one frame of doubt)
        assert_eq!(account(&[0, 1, 5002]).frames_missing, 0);
    }

    #[test]
    fn a_sender_restart_behind_is_not_followed() {
        // the baseline never moves back: restart the receiver with the sender
        let r = account(&[20_000, 20_001, 0, 1, 2, 4]);
        assert_eq!((r.frames_missing, r.frames_stray), (0, 4));
    }

    #[test]
    fn single_spoofs_inside_the_old_window_fake_no_loss() {
        let mut seqs: Vec<u16> = (0..50).collect();
        seqs.push(1049); // 1000 ahead
        seqs.extend(50..60);
        seqs.extend(70..80); // a real gap of 10
        let r = account(&seqs);
        assert_eq!(
            (r.frames_missing, r.frames_stray, r.frames_late),
            (10, 1, 0)
        );
        // chained single spoofs, each 1024 ahead
        let mut seqs: Vec<u16> = (0..50).collect();
        seqs.extend((1..=5).map(|k| 49 + 1024 * k));
        seqs.extend(50..60);
        assert_eq!(account(&seqs).frames_missing, 0);
        // across the wrap
        let mut seqs: Vec<u16> = (0xFFF0..=0xFFFF).collect();
        seqs.push(0x0300);
        seqs.extend(0..20);
        assert_eq!(account(&seqs).frames_missing, 0);
        // at the start
        assert_eq!(account(&[0, 1000, 1, 2, 3]).frames_missing, 0);
        assert_eq!(account(&[3000, 3500, 0, 1, 2, 3]).frames_missing, 0);
    }

    #[test]
    fn an_injection_while_a_real_gap_is_pending_changes_nothing() {
        let r = account(&[0, 1, 200, 5000, 201, 202]);
        assert_eq!((r.frames_missing, r.frames_stray), (198, 1));
    }

    #[test]
    fn replays_from_far_behind_fake_no_loss() {
        let mut seqs: Vec<u16> = (0..3000).collect();
        seqs.extend(100..110); // ten consecutive frames replayed from 2900 behind
        seqs.extend(3000..3010);
        let r = account(&seqs);
        assert_eq!((r.frames_missing, r.frames_stray), (0, 10));
    }

    #[test]
    fn replayed_runs_and_a_stray_first_frame_fake_no_loss() {
        // 10 ascending replays just behind, then honest frames: nothing counted missing
        let mut seqs: Vec<u16> = (0..300).collect();
        seqs.extend(100..110);
        seqs.extend(300..310);
        let r = account(&seqs);
        assert_eq!((r.frames_missing, r.frames_late), (0, 10));
        // a spoofed first frame far from the honest stream is a stray, not a gap
        let r = account(&[0, 12_000, 12_001, 12_002]);
        assert_eq!((r.frames_missing, r.frames_stray), (0, 1));
        // the first two frames still count a real gap between them
        assert_eq!(account(&[5, 7, 8]).frames_missing, 1);
    }

    /// One-frame objects, group g object o, numbered by `Fragmenter` from `seq0`.
    fn objs(keys: &[(u16, u8)]) -> Vec<Vec<u8>> {
        let mut f = Fragmenter::new(0).unwrap();
        keys.iter()
            .map(|&(g, o)| f.fragment(g, o, &obj(40, o)).unwrap().remove(0))
            .collect()
    }
    fn delivered(r: &mut Reassembler, frames: &[Vec<u8>]) -> Vec<(u16, u8)> {
        let mut ev = vec![];
        for fr in frames {
            ev.extend(r.push(fr));
        }
        ev.extend(r.finish());
        ev.iter()
            .filter_map(|e| match e {
                Event::Object { group, obj, .. } => Some((*group, *obj)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn replayed_old_frames_neither_break_nor_reenter_reassembly() {
        // groups 0..4 of 3 objects; frames from group 0 replayed into group 3
        let keys: Vec<(u16, u8)> = (0..5u16)
            .flat_map(|g| (0..3u8).map(move |o| (g, o)))
            .collect();
        let honest = objs(&keys);
        let mut frames = honest[..10].to_vec();
        for (i, old) in honest.iter().take(3).enumerate() {
            frames.push(with_seq(old, 100 + i as u16)); // old objects, near seq
        }
        frames.extend(honest[10..].iter().cloned());
        let mut r = Reassembler::new();
        let got = delivered(&mut r, &frames);
        assert_eq!(got, keys, "an old object re-entered or broke the stream");
        assert_eq!(r.frames_stale, 3);
    }

    #[test]
    fn one_far_future_frame_does_not_stop_the_stream() {
        let keys: Vec<(u16, u8)> = (0..4u16)
            .flat_map(|g| (0..3u8).map(move |o| (g, o)))
            .collect();
        let honest = objs(&keys);
        let mut frames = honest[..5].to_vec();
        let mut far = objs(&[(20_000, 0)]).remove(0);
        far = with_seq(&far, 5); // fits the seq stream; only its object key is wrong
        frames.push(far);
        frames.extend(honest[5..].iter().map(|f| {
            let s = u16::from_be_bytes([f[0], f[1]]) + 1;
            with_seq(f, s)
        }));
        let mut r = Reassembler::new();
        assert_eq!(delivered(&mut r, &frames), keys);
        assert_eq!(r.frames_stale, 1);
    }

    /// Honest frames for `keys` with one frame for `inject` slipped in at `at`, the
    /// sequence numbers continuing across it.
    fn with_injection(keys: &[(u16, u8)], at: usize, inject: (u16, u8)) -> Vec<Vec<u8>> {
        let mut frames: Vec<Vec<u8>> = objs(keys);
        frames.insert(at, objs(&[inject]).remove(0));
        frames
            .iter()
            .enumerate()
            .map(|(i, f)| with_seq(f, i as u16))
            .collect()
    }

    #[test]
    fn one_injected_frame_before_the_stream_does_not_wedge_it() {
        let keys: Vec<(u16, u8)> = (0..6u16)
            .flat_map(|g| (0..3u8).map(move |o| (g, o)))
            .collect();
        for inject in [(20_000, 0), (100, 0), (3, 0), (0, 2)] {
            let mut r = Reassembler::new();
            let got = delivered(&mut r, &with_injection(&keys, 0, inject));
            assert_eq!(got, keys, "injected {inject:?}");
            assert_eq!(r.frames_stale, 1, "injected {inject:?}");
        }
    }

    #[test]
    fn one_injected_frame_mid_run_costs_at_most_a_few_groups() {
        let keys: Vec<(u16, u8)> = (0..12u16)
            .flat_map(|g| (0..3u8).map(move |o| (g, o)))
            .collect();
        for inject in [(66, 0), (9, 0), (2, 255), (40_000, 1)] {
            let mut r = Reassembler::new();
            let got = delivered(&mut r, &with_injection(&keys, 7, inject));
            // groups 0..2 then frames from 2 onward: everything from group 5 arrives
            assert!(
                keys.iter().filter(|k| k.0 >= 5).all(|k| got.contains(k)),
                "injected {inject:?}: {got:?}"
            );
            assert!(got.len() + 9 >= keys.len(), "injected {inject:?}: {got:?}");
        }
        // one far ahead is held and dropped: nothing lost
        let mut r = Reassembler::new();
        assert_eq!(delivered(&mut r, &with_injection(&keys, 7, (66, 0))), keys);
        assert_eq!(r.frames_stale, 1);
    }

    #[test]
    fn an_injected_frame_released_on_idle_does_not_wedge_the_stream() {
        let keys: Vec<(u16, u8)> = (0..6u16)
            .flat_map(|g| (0..3u8).map(move |o| (g, o)))
            .collect();
        let frames = with_injection(&keys, 0, (20_000, 0));
        let mut r = Reassembler::new();
        r.push(&frames[0]);
        let mut got: Vec<(u16, u8)> = r
            .flush()
            .into_iter()
            .filter_map(|e| match e {
                Event::Object { group, obj, .. } => Some((group, obj)),
                _ => None,
            })
            .collect();
        got.extend(delivered(&mut r, &frames[1..]));
        assert_eq!(got[1..], keys[..], "{got:?}");
    }

    #[test]
    fn the_real_stream_takes_over_after_a_confirmed_wrong_jump() {
        let keys: Vec<(u16, u8)> = (0..30u16)
            .flat_map(|g| (0..3u8).map(move |o| (g, o)))
            .collect();
        let mut frames = objs(&keys);
        frames.splice(7..7, objs(&[(40_000, 0), (40_000, 1)]));
        let frames: Vec<Vec<u8>> = frames
            .iter()
            .enumerate()
            .map(|(i, f)| with_seq(f, i as u16))
            .collect();
        let mut r = Reassembler::new();
        let got = delivered(&mut r, &frames);
        let lost = keys.iter().filter(|k| !got.contains(k)).count();
        assert!(lost <= RECOVER_RUN as usize, "lost {lost}");
        assert!(keys.iter().filter(|k| k.0 >= 15).all(|k| got.contains(k)));
    }

    #[test]
    fn a_held_frame_is_kept_when_the_next_one_is_near_newest_too() {
        // (1,9) is held (a later group, obj >= OBJ_NEAR); (2,0) is near (0,2) on its own
        // but continues from (1,9), so both are placed
        let keys = [(0, 0), (0, 1), (0, 2), (1, 9), (2, 0), (2, 1)];
        let mut r = Reassembler::new();
        assert_eq!(delivered(&mut r, &objs(&keys)), keys);
        assert_eq!(r.frames_stale, 0);
    }

    #[test]
    fn a_frame_still_held_at_the_end_counts_as_stale() {
        let keys = [(0, 0), (0, 1), (0, 2)];
        let mut r = Reassembler::new();
        assert_eq!(delivered(&mut r, &with_injection(&keys, 3, (900, 0))), keys);
        assert_eq!((r.frames_ok, r.frames_stale), (3, 1));
    }

    #[test]
    fn a_long_outage_is_confirmed_and_its_first_frame_kept() {
        // group 0, then nothing until group 500 (beyond GROUP_STEP): both objects arrive
        let keys = [(0, 0), (0, 1), (500, 0), (500, 1)];
        let mut r = Reassembler::new();
        assert_eq!(delivered(&mut r, &objs(&keys)), keys);
        assert_eq!(r.frames_stale, 0);
    }

    #[test]
    fn frame_counted_once() {
        let mut f = Fragmenter::new(0).unwrap();
        let a = f.fragment(1, 0, &obj(600, 2)).unwrap();
        let mut r = Reassembler::new();
        r.push(&a[0]);
        // same object id, different objlen: CRC-valid, inconsistent
        let mut g = Fragmenter::new(0).unwrap();
        g.seq = 1;
        let b = g.fragment(1, 0, &obj(300, 3)).unwrap();
        r.push(&b[1]);
        assert_eq!(
            (r.frames_ok, r.frames_bad),
            (1, 1),
            "bad frame also counted ok"
        );
    }

    #[test]
    fn crc_rejects_corruption() {
        let mut f = Fragmenter::new(0).unwrap();
        let fr = f.fragment(0, 0, &obj(300, 3)).unwrap();
        assert!(FrameHdr::decode(&fr[0]).is_some());
        let mut bad = fr[0].clone();
        bad[100] ^= 0x10;
        assert!(FrameHdr::decode(&bad).is_none());
        assert_eq!(crc16(b"123456789"), 0x29B1);
    }

    #[test]
    fn group_gate() {
        let mut g = GroupGate::default();
        assert!(g.accept(0, 0));
        assert!(g.accept(0, 1));
        g.lost(0, 2);
        assert!(!g.accept(0, 3));
        assert!(!g.accept(0, 4));
        assert!(g.accept(1, 0));
        assert!(g.accept(1, 1));
        assert!(!g.accept(1, 3)); // gap
        assert!(g.accept(5, 0)); // groups dropped at the sender
        assert_eq!(g.groups_cut, 2);
    }

    fn range(fs: &[u32]) -> FrameRange {
        let mut r = FrameRange::default();
        for &f in fs {
            r.observe(f);
        }
        r
    }

    #[test]
    fn frame_range_ignores_one_wrong_frame_number() {
        let honest: Vec<u32> = (0..30).collect();
        assert_eq!(range(&honest).expected(), 30);
        // one object stamped u32::MAX (the old sender bug) or garbled
        let mut fs = honest.clone();
        fs.insert(10, u32::MAX);
        let r = range(&fs);
        assert_eq!((r.expected(), r.rejected), (30, 1));
        // a repeated frame number adds nothing
        let mut fs = honest.clone();
        fs.push(29);
        assert_eq!(range(&fs).expected(), 30);
        // a wrong first frame number
        let r = range(&[5_000_000, 0, 1, 2]);
        assert_eq!(
            (r.expected(), r.rejected, r.contains(5_000_000)),
            (3, 1, false)
        );
        // an object from before the range
        let r = range(&[100, 101, 102, 3]);
        assert_eq!((r.expected(), r.rejected), (3, 1));
    }

    #[test]
    fn frame_range_bounds_a_wrong_number_near_real_ones() {
        // mid-run, one step past the range: rejected
        let mut fs: Vec<u32> = (0..10).collect();
        fs.push(265);
        fs.extend(10..30);
        let r = range(&fs);
        assert_eq!((r.expected(), r.rejected), (30, 1));
        // before the first real frame: rejected, or at most FRAME_EDGE_STEP - 1 extra
        assert_eq!(range(&[0, 100, 101, 102]).expected(), 3);
        assert_eq!(range(&[200, 300, 301, 302]).expected(), 3);
        assert!(range(&[92, 100, 101, 102]).expected() <= 3 + FRAME_EDGE_STEP as u64);
        assert!(range(&[0, 1, 2, 10]).expected() <= 3 + FRAME_EDGE_STEP as u64);
        // after the last real frame
        let r = range(&[0, 1, 2, 3, 200]);
        assert_eq!((r.expected(), r.contains(200)), (4, false));
    }

    #[test]
    fn frame_range_counts_real_gaps() {
        // a GOP dropped at the sender: a step of 15
        assert_eq!(range(&[0, 1, 2, 18, 19]).expected(), 20);
        // an outage longer than FRAME_STEP, confirmed by the next object
        let r = range(&[0, 1, 2, 1000, 1001]);
        assert_eq!((r.expected(), r.rejected), (1002, 0));
        // an injection while that jump is pending changes nothing
        let r = range(&[0, 1, 2, 1000, 9_000_000, 1001]);
        assert_eq!((r.expected(), r.rejected), (1002, 1));
    }

    #[test]
    fn obj_hdr_roundtrip() {
        let h = ObjHdr {
            frame_no: 77,
            cap_us: 1_794_900_000_123_456,
            enc_us: 33_000,
            key: true,
        };
        let mut b = h.encode().to_vec();
        b.extend_from_slice(b"xyz");
        assert_eq!(ObjHdr::decode(&b), Some((h, &b"xyz"[..])));
    }

    #[test]
    fn decode_rejects_oversize_and_truncated() {
        assert!(FrameHdr::decode(&[]).is_none());
        assert!(FrameHdr::decode(&[0u8; HDR_LEN]).is_none());
        assert!(FrameHdr::decode(&vec![0u8; MAX_FRAME + 1]).is_none());
        assert!(FrameHdr::decode(&vec![0x11u8; 64 * 1024]).is_none());
        let mut f = Fragmenter::new(0).unwrap();
        let fr = f.fragment(1, 0, &obj(40, 1)).unwrap().pop().unwrap();
        assert!(fr.len() <= MAX_FRAME);
        assert!(FrameHdr::decode(&fr).is_some());
        assert!(FrameHdr::decode(&fr[..HDR_LEN]).is_none());
    }

    #[test]
    fn chunk_lengths_sum_to_objlen() {
        for objlen in 1usize..=MAX_OBJECT {
            let nfrag = objlen.div_ceil(MAX_CHUNK);
            let h = FrameHdr {
                seq: 0,
                group: 0,
                obj: 0,
                idx: 0,
                nfrag: nfrag as u8,
                parity: false,
                k: 0,
                objlen: objlen as u16,
            };
            assert!(h.chunk_size() <= MAX_CHUNK, "objlen {objlen}");
            let sum: usize = (0..nfrag).map(|i| h.chunk_len(i)).sum();
            assert_eq!(sum, objlen, "objlen {objlen}");
            assert_eq!(h.chunk_len(nfrag), 0);
        }
        let zero = FrameHdr {
            seq: 0,
            group: 0,
            obj: 0,
            idx: 0,
            nfrag: 0,
            parity: false,
            k: 0,
            objlen: 10,
        };
        assert_eq!(zero.chunk_size(), 0);
        assert_eq!(zero.chunk_len(0), 0);
    }

    #[test]
    fn hostile_frames_do_not_panic() {
        let mut rng = 0x1234_5678_u64;
        let mut next = || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            (rng >> 33) as u8
        };
        let mut r = Reassembler::new();
        for _ in 0..500 {
            let n = (next() as usize) * 4; // 0..=1020, includes oversize
            let frame: Vec<u8> = (0..n).map(|_| next()).collect();
            let _ = r.push(&frame);
        }
        let _ = r.flush();
        // A well-formed object with one flipped header field must not panic either.
        let mut f = Fragmenter::new(8).unwrap();
        f.min_frame = 96;
        let frames = f.fragment(2, 1, &obj(900, 9)).unwrap();
        for fr in &frames {
            let mut bad = fr.clone();
            bad[4] ^= 0x7f;
            bad[6] ^= 0xff;
            let _ = r.push(&bad);
            let mut long = fr.clone();
            long.extend_from_slice(&[0u8; 300]);
            let _ = r.push(&long);
        }
        let _ = r.flush();
    }

    #[test]
    fn profile_parser() {
        assert_eq!(
            parse_fsk_profile("300000:100000:6").unwrap(),
            FskProfile {
                bitrate: 300_000,
                fdev: 100_000,
                preamble: 6,
                unchecked: false
            }
        );
        assert!(parse_fsk_profile("350000:100000:14:x").unwrap().unchecked);
        assert!(parse_fsk_profile("0:100000:6").is_err());
        assert!(parse_fsk_profile("300000:100000:6:y").is_err());
        assert!(parse_fsk_profile("300000:100000").is_err());
        assert!(parse_fsk_profile("300000:100000:65536").is_err());
    }

    #[test]
    fn plan_matches_the_fragmenter() {
        // every chunk-count boundary up to six frames, then a few large objects
        let sizes = (1..=6 * MAX_CHUNK + 2)
            .step_by(7)
            .chain([MAX_CHUNK, MAX_CHUNK + 1, 2 * MAX_CHUNK, 2 * MAX_CHUNK + 1])
            .chain([30_001, MAX_OBJECT]);
        for len in sizes {
            for k in 0..=15u8 {
                for min_frame in [0, 96, MAX_FRAME + 40] {
                    let mut f = Fragmenter::new(k).unwrap();
                    f.min_frame = min_frame;
                    let real: Vec<usize> = f
                        .fragment(3, 2, &obj(len, 7))
                        .unwrap()
                        .iter()
                        .map(Vec::len)
                        .collect();
                    assert_eq!(
                        plan(len, k, min_frame).unwrap(),
                        real,
                        "len {len} k {k} min_frame {min_frame}"
                    );
                }
            }
        }
    }

    #[test]
    fn plan_refuses_what_fragment_refuses() {
        assert_eq!(plan(0, 0, 0), Err(WireError::Empty));
        assert_eq!(
            plan(MAX_OBJECT + 1, 0, 0),
            Err(WireError::TooBig(MAX_OBJECT + 1))
        );
        assert_eq!(plan(10, 16, 0), Err(WireError::BadK));
        // a 3-frame object at k = 2: two data frames, parity, one data frame, parity
        assert_eq!(plan(2 * MAX_CHUNK + 1, 2, 0).unwrap().len(), 5);
    }

    #[test]
    fn hex_and_r_lines() {
        assert_eq!(decode_hex(""), Some(vec![]));
        assert_eq!(decode_hex("0aFf"), Some(vec![0x0a, 0xff]));
        assert_eq!(decode_hex("abc"), None);
        assert_eq!(decode_hex("zz"), None);
        assert_eq!(decode_hex("+1"), None); // from_str_radix would accept a sign
        assert_eq!(decode_hex("a\u{fffd}b"), None); // used to panic on a char boundary
        assert_eq!(decode_hex("\u{e9}"), None);
        assert_eq!(
            parse_r_line("R 12 -50 0 0102"),
            Some(RLine {
                t_us: 12,
                rssi: -50,
                snr: 0,
                frame: vec![1, 2]
            })
        );
        let r = parse_r_line("R 1234567 -91 -22 ff").unwrap();
        assert_eq!((r.t_us, r.rssi, r.snr), (1234567, -91, -22));
        // strict: a field that does not parse, a missing field or an extra token is not an R line
        assert_eq!(parse_r_line("R x -91 y ff"), None);
        assert_eq!(parse_r_line("R 12 x 0 01"), None);
        assert_eq!(parse_r_line("R 5 -60"), None);
        assert_eq!(parse_r_line("R 1 -50 4 aa extra"), None);
        assert_eq!(parse_r_line("R 1 -50 4 aa "), None);
        assert_eq!(parse_r_line("R 18446744073709551616 0 0 aa"), None);
        // an empty frame, with or without the empty hex field
        assert_eq!(parse_r_line("R 12 -50 0").unwrap().frame, Vec::<u8>::new());
        assert_eq!(parse_r_line("R 12 -50 0 ").unwrap().frame, Vec::<u8>::new());
        assert_eq!(parse_r_line("R 0 -50 0 a\u{e9}"), None);
        assert_eq!(parse_r_line("R 0 -50 0 abc"), None);
        assert_eq!(parse_r_line("X 0 Crc"), None);
        assert!(parse_r_line(&format!("R 0 0 0 {}", "00".repeat(MAX_FRAME))).is_some());
        assert!(parse_r_line(&format!("R 0 0 0 {}", "00".repeat(MAX_FRAME + 1))).is_none());
    }

    #[test]
    fn read_line_limited_caps_and_keeps_short_lines() {
        let mut input = std::io::Cursor::new(b"ok\n".to_vec());
        assert_eq!(
            read_line_limited(&mut input, 8).unwrap(),
            Some(Line::Data(b"ok\n".to_vec()))
        );
        assert_eq!(read_line_limited(&mut input, 8).unwrap(), None);

        let mut big = vec![b'A'; 50];
        big.push(b'\n');
        big.extend_from_slice(b"next\n");
        let mut input = std::io::Cursor::new(big);
        assert_eq!(
            read_line_limited(&mut input, 16).unwrap(),
            Some(Line::TooLong)
        );
        assert_eq!(
            read_line_limited(&mut input, 16).unwrap(),
            Some(Line::Data(b"next\n".to_vec()))
        );

        // A signal during a read is retried, not reported as an error.
        struct Eintr(u8);
        impl std::io::Read for Eintr {
            fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
                self.0 += 1;
                match self.0 {
                    1 => Err(std::io::ErrorKind::Interrupted.into()),
                    2 => {
                        b[..3].copy_from_slice(b"ok\n");
                        Ok(3)
                    }
                    _ => Ok(0),
                }
            }
        }
        let mut input = std::io::BufReader::new(Eintr(0));
        assert_eq!(
            read_line_limited(&mut input, 8).unwrap(),
            Some(Line::Data(b"ok\n".to_vec()))
        );

        // No newline, and longer than the cap: do not return the bytes.
        let mut input = std::io::Cursor::new(vec![b'B'; 100]);
        assert_eq!(read_line_limited(&mut input, 16).unwrap(), None);
    }
}
