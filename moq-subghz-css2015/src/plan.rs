//! The frames one pass of the test puts on the air, for each of the three modes.

use crate::{chunk, framed, ssdv};

/// How the image is framed.
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// JPEG cut into 255 byte frames with a two byte header.
    Chunk,
    /// SSDV no-FEC packets, 255 bytes (sync byte dropped).
    Ssdv,
    /// Chunked JPEG inside framed-uplink 1.0 unconfirmed uplinks. No network behind it.
    Framed,
}

/// Application payload of a framed uplink, named after the US915 data rate whose
/// size limit it uses. Both are 242 bytes when no repeater is assumed (222 if one is).
#[derive(Copy, Clone, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Dr {
    /// US915 DR3 size limit (SF7/125 kHz).
    Dr3,
    /// US915 DR4 size limit (SF8/500 kHz).
    Dr4,
}

impl Dr {
    /// Application payload bytes per uplink.
    pub fn payload_len(self) -> usize {
        match self {
            Dr::Dr3 | Dr::Dr4 => framed::MAX_PAYLOAD,
        }
    }
}

/// What to send and how.
#[derive(Clone, Debug)]
pub struct Plan<'a> {
    /// Framing.
    pub mode: Mode,
    /// The file: a JPEG for `Chunk` and `Framed`, the output of `ssdv -e -n` for `Ssdv`.
    pub file: &'a [u8],
    /// Image id: 0..=15 for the chunk header; the SSDV id is inside the packets.
    pub image: u8,
    /// framed payload size.
    pub dr: Dr,
    /// Use this payload size instead of the data rate's (`Framed` only).
    pub payload: Option<usize>,
    /// First framed frame counter of the pass.
    pub fcnt: u16,
}

impl Plan<'_> {
    /// The radio frames of one pass, in the order they are sent.
    pub fn frames(&self) -> Result<Vec<Vec<u8>>, String> {
        match self.mode {
            Mode::Chunk => {
                if !chunk::looks_like_jpeg(self.file) {
                    return Err("not a JPEG (no FFD8 start or FFD9 end)".into());
                }
                chunk::split(self.image, self.file, chunk::MAX_FRAME)
            }
            Mode::Ssdv => ssdv::read_packets(self.file)?
                .iter()
                .map(|p| ssdv::to_air(p).map(|a| a.to_vec()))
                .collect(),
            Mode::Framed => {
                if !chunk::looks_like_jpeg(self.file) {
                    return Err("not a JPEG (no FFD8 start or FFD9 end)".into());
                }
                let per = self.payload.unwrap_or(self.dr.payload_len());
                if !(1..=framed::MAX_PAYLOAD).contains(&per) {
                    return Err(format!(
                        "framed payload {per} is outside 1..={}",
                        framed::MAX_PAYLOAD
                    ));
                }
                chunk::split(self.image, self.file, per)?
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let up = framed::Uplink {
                            dev_addr: framed::TEST_DEV_ADDR,
                            fcnt: self.fcnt.wrapping_add(i as u16),
                            fport: 1,
                        };
                        framed::encode(&framed::TEST_KEY, &up, c)
                    })
                    .collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(n: usize) -> Vec<u8> {
        let mut v = vec![0x11; n];
        v[..2].copy_from_slice(&[0xFF, 0xD8]);
        v[n - 2..].copy_from_slice(&[0xFF, 0xD9]);
        v
    }

    fn plan(mode: Mode, file: &[u8]) -> Plan<'_> {
        Plan {
            mode,
            file,
            image: 2,
            dr: Dr::Dr3,
            payload: None,
            fcnt: 10,
        }
    }

    #[test]
    fn chunk_frames_are_full_255_byte_frames() {
        let j = jpeg(8553);
        let f = plan(Mode::Chunk, &j).frames().unwrap();
        assert_eq!(f.len(), 34);
        assert!(f[..33].iter().all(|x| x.len() == 255));
        assert_eq!(f.concat().len(), 8553 + 34 * chunk::HDR_LEN);
    }

    #[test]
    fn framed_frames_carry_the_chunks_and_count_up() {
        let j = jpeg(8553);
        let f = plan(Mode::Framed, &j).frames().unwrap();
        assert_eq!(f.len(), 8553usize.div_ceil(240));
        assert!(f[..f.len() - 1].iter().all(|x| x.len() == 255));
        let mut back = Vec::new();
        for (i, fr) in f.iter().enumerate() {
            let (up, payload) = framed::decode(&framed::TEST_KEY, fr).unwrap();
            assert_eq!(up.fcnt, 10 + i as u16);
            let h = chunk::Hdr::decode(&payload).unwrap();
            assert_eq!((h.image, h.seq as usize, h.count as usize), (2, i, f.len()));
            back.extend_from_slice(&payload[chunk::HDR_LEN..]);
        }
        assert_eq!(back, j);
    }

    #[test]
    fn framed_payload_can_follow_the_repeater_compatible_limit() {
        let j = jpeg(1000);
        let mut p = plan(Mode::Framed, &j);
        p.payload = Some(222);
        let f = p.frames().unwrap();
        assert_eq!(f.len(), 1000usize.div_ceil(220));
        assert!(f[..f.len() - 1].iter().all(|x| x.len() == 222 + 13));
        p.payload = Some(243);
        assert!(p.frames().is_err());
        p.payload = Some(2);
        assert!(p.frames().is_err()); // no room for data after the 2 byte chunk header
    }

    #[test]
    fn ssdv_frames_drop_the_sync_byte() {
        let pkts = [
            ssdv::tests_support::packet(1, 0, false),
            ssdv::tests_support::packet(1, 1, true),
        ];
        let file = pkts.concat();
        let f = plan(Mode::Ssdv, &file).frames().unwrap();
        assert_eq!(f.len(), 2);
        assert!(f.iter().all(|x| x.len() == 255 && x[0] == ssdv::TYPE_NOFEC));
        assert_eq!(ssdv::from_air(&f[1]).unwrap(), pkts[1]);
    }

    #[test]
    fn wrong_files_are_refused_not_sent() {
        assert!(plan(Mode::Chunk, &[1, 2, 3]).frames().is_err());
        assert!(plan(Mode::Framed, &[1, 2, 3]).frames().is_err());
        assert!(plan(Mode::Ssdv, &jpeg(600)).frames().is_err());
        // a JPEG too big for 64 chunks
        assert!(plan(Mode::Chunk, &jpeg(20_000)).frames().is_err());
    }
}
