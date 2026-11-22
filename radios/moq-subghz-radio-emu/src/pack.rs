//! Packing of several 255 B wire frames into one PHY packet.
//!
//! The wire format is untouched: a wire frame is at most `MAX_FRAME` (255) bytes, with `idx`
//! and `nfrag` as `u8`. A 802.11ah packet can carry far more than that, so the sender glues
//! whole wire frames together as `[len u8][frame bytes]` records. `len` is 1..=255, so no
//! record can be mistaken for another and the unpacker needs no count field. Each frame keeps
//! its own CRC-16, so a corrupted record is rejected by the normal frame check.
use moq_subghz_wire::MAX_FRAME;

/// Bytes the packing adds per frame.
pub const RECORD_OVERHEAD: usize = 1;

/// How many of the leading frames (lengths in `lens`) fit into one packet of at most
/// `mtu` payload bytes and at most `max_frames` frames. Always at least 1 when `lens` is
/// not empty and the first frame fits on its own; 0 otherwise.
pub fn take_batch(lens: &[usize], mtu: usize, max_frames: usize) -> usize {
    let mut used = 0;
    let mut n = 0;
    for &l in lens.iter().take(max_frames) {
        if l == 0 || l > MAX_FRAME || used + l + RECORD_OVERHEAD > mtu {
            break;
        }
        used += l + RECORD_OVERHEAD;
        n += 1;
    }
    n
}

/// Concatenate frames into one packet. Frames must be 1..=255 bytes.
pub fn pack(frames: &[&[u8]]) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::with_capacity(frames.iter().map(|f| f.len() + RECORD_OVERHEAD).sum());
    for f in frames {
        if f.is_empty() || f.len() > MAX_FRAME {
            return Err("frame length must be 1..=255");
        }
        out.push(f.len() as u8);
        out.extend_from_slice(f);
    }
    Ok(out)
}

/// Split a packet back into wire frames. `None` if the records do not tile the packet
/// exactly (truncated or zero-length record).
pub fn unpack(pkt: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < pkt.len() {
        let l = pkt[i] as usize;
        i += 1;
        if l == 0 || i + l > pkt.len() {
            return None;
        }
        out.push(pkt[i..i + l].to_vec());
        i += l;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_mixed_lengths() {
        let a = vec![1u8; 255];
        let b = vec![2u8; 96];
        let c = vec![3u8; 13];
        let p = pack(&[&a, &b, &c]).unwrap();
        assert_eq!(p.len(), 255 + 96 + 13 + 3);
        assert_eq!(unpack(&p).unwrap(), vec![a, b, c]);
    }

    #[test]
    fn rejects_malformed() {
        assert_eq!(unpack(&[0]), None);
        assert_eq!(unpack(&[5, 1, 2]), None);
        assert_eq!(unpack(&[]), Some(vec![]));
        assert!(pack(&[&[]]).is_err());
        assert!(pack(&[&vec![0u8; 256]]).is_err());
    }

    #[test]
    fn batch_respects_mtu_and_cap() {
        let lens = [255; 10];
        assert_eq!(take_batch(&lens, 1472, 99), 5); // 5 * 256 = 1280, 6 * 256 = 1536
        assert_eq!(take_batch(&lens, 1472, 3), 3);
        assert_eq!(take_batch(&lens, 255, 99), 0); // no room for the length byte
        assert_eq!(take_batch(&lens, 256, 99), 1);
        assert_eq!(take_batch(&[], 1472, 9), 0);
    }
}
