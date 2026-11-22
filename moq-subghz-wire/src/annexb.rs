//! H.264 Annex-B access-unit splitting. The encoder is run with
//! `-bsf:v dump_extra=freq=keyframe,h264_metadata=aud=insert`, so every access unit
//! starts with an AUD (NAL type 9) and keyframes carry SPS/PPS in-band.

/// Offsets of every start code (`00 00 01`, or `00 00 00 01` where the leading zero is
/// included) together with the NAL type that follows.
pub fn nals(b: &[u8]) -> Vec<(usize, u8)> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 3 < b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            let start = if i > 0 && b[i - 1] == 0 { i - 1 } else { i };
            v.push((start, b[i + 3] & 0x1f));
            i += 3;
        } else {
            i += 1;
        }
    }
    v
}

pub fn is_key(au: &[u8]) -> bool {
    nals(au).iter().any(|&(_, t)| t == 5)
}
pub fn has_vcl(au: &[u8]) -> bool {
    nals(au).iter().any(|&(_, t)| (1..=5).contains(&t))
}

/// Accumulates encoder output and cuts it into access units at each AUD.
#[derive(Default)]
pub struct AuSplitter {
    buf: Vec<u8>,
}

impl AuSplitter {
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let cuts: Vec<usize> = nals(&self.buf)
            .into_iter()
            .filter(|&(p, t)| t == 9 && p > 0)
            .map(|(p, _)| p)
            .collect();
        let mut out = Vec::new();
        let mut last = 0;
        for c in cuts {
            if has_vcl(&self.buf[last..c]) {
                out.push(self.buf[last..c].to_vec());
                last = c;
            }
        }
        self.buf.drain(..last);
        // A tail with no AUD that is already too big to send will never become a
        // legal object. Drop it instead of letting a stuck encoder grow the buffer.
        if self.buf.len() > crate::MAX_OBJECT {
            self.buf.clear();
        }
        out
    }

    /// The last access unit, once the encoder's output has ended. A pause on the pipe is
    /// not an end: a pipe keeps bytes, not write boundaries, so only the next AUD or EOF
    /// closes a picture.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        if has_vcl(&self.buf) {
            Some(std::mem::take(&mut self.buf))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split() {
        let aud = [0, 0, 0, 1, 9, 0xf0];
        let sps = [0, 0, 0, 1, 0x67, 1, 2];
        let idr = [0, 0, 1, 0x65, 7, 7, 7];
        let p = [0, 0, 1, 0x41, 8, 8];
        let mut s = AuSplitter::default();
        let a1: Vec<u8> = [&aud[..], &sps, &idr].concat();
        let a2: Vec<u8> = [&aud[..], &p].concat();
        let all: Vec<u8> = [a1.clone(), a2.clone(), aud.to_vec()].concat();
        let mut got = vec![];
        for ch in all.chunks(3) {
            got.extend(s.push(ch));
        }
        assert_eq!(got, vec![a1.clone(), a2]);
        assert!(is_key(&a1));
        assert!(s.finish().is_none()); // AUD only
    }

    #[test]
    fn a_picture_split_across_reads_stays_one_access_unit() {
        let aud = [0, 0, 0, 1, 9, 0xf0];
        let slice1 = [0, 0, 1, 0x65, 0x88, 1, 1];
        let slice2 = [0, 0, 1, 0x65, 0x10, 2, 2];
        let p = [0, 0, 1, 0x41, 8, 8];
        let a1: Vec<u8> = [&aud[..], &slice1, &slice2].concat();
        let a2: Vec<u8> = [&aud[..], &p].concat();
        let mut s = AuSplitter::default();
        // the encoder's write of a1 arrives in two reads, with a pause after slice1
        let cut = aud.len() + slice1.len();
        assert!(s.push(&a1[..cut]).is_empty());
        assert!(s.push(&a1[cut..]).is_empty());
        assert_eq!(s.push(&a2), vec![a1]);
        assert_eq!(s.finish(), Some(a2));
    }

    #[test]
    fn splitter_drops_unbounded_tail() {
        let mut s = AuSplitter::default();
        let junk = vec![0u8; crate::MAX_OBJECT + 8];
        assert!(s.push(&junk).is_empty());
        assert!(s.finish().is_none());
        // resyncs at the next real access unit
        let aud = [0, 0, 0, 1, 9, 0xf0];
        let p = [0, 0, 1, 0x41, 8, 8];
        let au: Vec<u8> = [&aud[..], &p].concat();
        let mut got = s.push(&au);
        got.extend(s.finish());
        assert_eq!(got, vec![au]);
    }
}
