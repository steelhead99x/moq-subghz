//! Chunked JPEG, the simplest 2015 way: cut the file into radio frames, each with a
//! two-byte header, and send them again until the receiver has them all.
//!
//! Header, big endian: image id (4 bits), chunk number (6 bits), chunk count minus one
//! (6 bits). So one image is at most 64 chunks.

/// Bytes of header in front of every chunk.
pub const HDR_LEN: usize = 2;
/// Largest radio frame the SX1276 sends.
pub const MAX_FRAME: usize = 255;
/// Most chunks one image may have.
pub const MAX_CHUNKS: usize = 64;

/// Which image a chunk belongs to and where it goes in it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Hdr {
    /// Image id, 0..=15.
    pub image: u8,
    /// Chunk number, 0..count.
    pub seq: u8,
    /// Chunks in the image, 1..=64.
    pub count: u8,
}

impl Hdr {
    /// The two header bytes, or an error for a field that does not fit.
    pub fn encode(&self) -> Result<[u8; HDR_LEN], String> {
        if self.image > 15 {
            return Err(format!("image id {} does not fit in 4 bits", self.image));
        }
        if self.count == 0 || self.count as usize > MAX_CHUNKS || self.seq >= self.count {
            return Err(format!("chunk {} of {} is not valid", self.seq, self.count));
        }
        let v = (self.image as u16) << 12 | (self.seq as u16) << 6 | (self.count as u16 - 1);
        Ok(v.to_be_bytes())
    }

    /// Parse a header from the front of `frame`. `None` for a chunk number past the count.
    pub fn decode(frame: &[u8]) -> Option<Hdr> {
        let v = u16::from_be_bytes([*frame.first()?, *frame.get(1)?]);
        let h = Hdr {
            image: (v >> 12) as u8,
            seq: ((v >> 6) & 0x3F) as u8,
            count: (v & 0x3F) as u8 + 1,
        };
        (h.seq < h.count).then_some(h)
    }
}

/// Cut `data` into frames of at most `frame_len` bytes, header included.
pub fn split(image: u8, data: &[u8], frame_len: usize) -> Result<Vec<Vec<u8>>, String> {
    if !(HDR_LEN + 1..=MAX_FRAME).contains(&frame_len) {
        return Err(format!("frame length {frame_len} is outside 3..=255"));
    }
    if data.is_empty() {
        return Err("empty image".into());
    }
    let per = frame_len - HDR_LEN;
    let count = data.len().div_ceil(per);
    if count > MAX_CHUNKS {
        return Err(format!(
            "{} bytes need {count} chunks of {per} bytes; the header allows {MAX_CHUNKS}",
            data.len()
        ));
    }
    data.chunks(per)
        .enumerate()
        .map(|(i, c)| {
            let h = Hdr {
                image,
                seq: i as u8,
                count: count as u8,
            }
            .encode()?;
            Ok([&h[..], c].concat())
        })
        .collect()
}

/// What `Assembly::push` did with a frame.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Push {
    /// A chunk not seen before.
    New,
    /// A chunk already held (a repeat pass).
    Duplicate,
}

/// The chunks of one image as they arrive, in any order and any number of times.
#[derive(Clone, Debug)]
pub struct Assembly {
    parts: Vec<Option<Vec<u8>>>,
}

impl Assembly {
    /// An image of `count` chunks with none received.
    pub fn new(count: u8) -> Self {
        Assembly {
            parts: vec![None; count as usize],
        }
    }

    /// Chunks in the image.
    pub fn count(&self) -> usize {
        self.parts.len()
    }

    /// Distinct chunks received.
    pub fn have(&self) -> usize {
        self.parts.iter().flatten().count()
    }

    /// Store a chunk. A repeat with different bytes is an error: the sender never does that.
    pub fn push(&mut self, seq: u8, data: &[u8]) -> Result<Push, String> {
        let count = self.parts.len();
        let slot = self
            .parts
            .get_mut(seq as usize)
            .ok_or_else(|| format!("chunk {seq} of {count}"))?;
        match slot {
            Some(old) if old == data => Ok(Push::Duplicate),
            Some(_) => Err(format!("chunk {seq} repeated with different bytes")),
            None => {
                *slot = Some(data.to_vec());
                Ok(Push::New)
            }
        }
    }

    /// The whole image once every chunk is here.
    pub fn assemble(&self) -> Option<Vec<u8>> {
        self.parts
            .iter()
            .map(|p| p.as_deref())
            .collect::<Option<Vec<_>>>()
            .map(|v| v.concat())
    }
}

/// True when `data` starts like a JPEG and ends with its end-of-image marker.
pub fn looks_like_jpeg(data: &[u8]) -> bool {
    data.starts_with(&[0xFF, 0xD8]) && data.ends_with(&[0xFF, 0xD9])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(n: usize) -> Vec<u8> {
        let mut v: Vec<u8> = (0..n).map(|i| (i * 7 + 3) as u8).collect();
        v[0] = 0xFF;
        v[1] = 0xD8;
        v[n - 2] = 0xFF;
        v[n - 1] = 0xD9;
        v
    }

    #[test]
    fn header_round_trips_at_the_edges() {
        for (image, seq, count) in [(0, 0, 1), (15, 63, 64), (7, 0, 64), (1, 63, 64)] {
            let h = Hdr { image, seq, count };
            assert_eq!(Hdr::decode(&h.encode().unwrap()), Some(h));
        }
    }

    #[test]
    fn header_rejects_what_does_not_fit() {
        let bad = |image, seq, count| Hdr { image, seq, count }.encode().is_err();
        assert!(bad(16, 0, 1));
        assert!(bad(0, 0, 0));
        assert!(bad(0, 0, 65));
        assert!(bad(0, 3, 3));
        // chunk number 5 of a 3-chunk image
        let v = (5u16 << 6 | 2).to_be_bytes();
        assert_eq!(Hdr::decode(&v), None);
        assert_eq!(Hdr::decode(&[0x01]), None);
    }

    #[test]
    fn split_fills_frames_and_reassembles_in_any_order() {
        let data = jpeg(1000);
        let frames = split(3, &data, 255).unwrap();
        assert_eq!(frames.len(), 4);
        assert!(frames[..3].iter().all(|f| f.len() == 255));
        assert_eq!(frames[3].len(), HDR_LEN + 1000 - 3 * 253);
        let mut a = Assembly::new(frames.len() as u8);
        for f in frames.iter().rev() {
            let h = Hdr::decode(f).unwrap();
            assert_eq!((h.image, h.count), (3, 4));
            assert_eq!(a.assemble(), None);
            assert_eq!(a.push(h.seq, &f[HDR_LEN..]), Ok(Push::New));
        }
        assert_eq!(a.assemble().unwrap(), data);
        assert!(looks_like_jpeg(&a.assemble().unwrap()));
    }

    #[test]
    fn split_limits() {
        // exactly 64 chunks is fine, one byte more is not
        assert_eq!(split(0, &vec![1; 64 * 253], 255).unwrap().len(), 64);
        assert!(split(0, &vec![1; 64 * 253 + 1], 255).is_err());
        assert!(split(0, &[], 255).is_err());
        assert!(split(0, &[1], 256).is_err());
        assert!(split(0, &[1], 2).is_err());
        assert_eq!(split(0, &[1, 2], 3).unwrap().len(), 2);
    }

    #[test]
    fn repeats_are_counted_and_conflicts_fail() {
        let mut a = Assembly::new(2);
        assert_eq!(a.push(0, b"ab"), Ok(Push::New));
        assert_eq!(a.push(0, b"ab"), Ok(Push::Duplicate));
        assert!(a.push(0, b"zz").is_err());
        assert!(a.push(2, b"zz").is_err());
        assert_eq!((a.have(), a.count()), (1, 2));
        assert_eq!(a.assemble(), None);
    }

    #[test]
    fn jpeg_check_needs_both_markers() {
        assert!(!looks_like_jpeg(&[0xFF, 0xD8]));
        assert!(!looks_like_jpeg(&[0xFF, 0xD8, 1, 2, 0xFF]));
        assert!(looks_like_jpeg(&[0xFF, 0xD8, 0xFF, 0xD9]));
    }
}
