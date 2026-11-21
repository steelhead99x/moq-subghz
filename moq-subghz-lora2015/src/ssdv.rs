//! SSDV no-FEC packets as Dave Akerman's LoRa gateway carried them. An SSDV packet is 256
//! bytes and the SX1276 sends at most 255, so the sender drops the leading 0x55 sync byte
//! and the receiver puts it back. The sync byte is not covered by the CRC.
//!
//! Format: codeberg.org/fsphil/ssdv (`ssdv.h`, `ssdv.c`). This module only frames,
//! checks and sorts packets. Turning a JPEG into packets and back stays with the `ssdv`
//! tool, so the test images here are decoded by the reference code.

/// A whole SSDV packet, sync byte included.
pub const PKT_LEN: usize = 256;
/// What goes on the air: the packet without its sync byte.
pub const AIR_LEN: usize = 255;
/// The first byte of every packet.
pub const SYNC: u8 = 0x55;
/// Packet type byte of a no-FEC packet (0x66 is the Reed-Solomon type, not supported here).
pub const TYPE_NOFEC: u8 = 0x67;
/// Header bytes after the sync byte and before the payload.
pub const HDR_LEN: usize = 14;
/// Payload bytes of a no-FEC packet.
pub const PAYLOAD_LEN: usize = PKT_LEN - 1 - HDR_LEN - 4;

/// The fields of the packet header the test needs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// The image this packet belongs to (the encoder's `-i`).
    pub image_id: u8,
    /// Packet number in the image, from 0.
    pub packet_id: u16,
    /// Image width in pixels.
    pub width: u16,
    /// Image height in pixels.
    pub height: u16,
    /// Set on the last packet of the image.
    pub eoi: bool,
    /// Callsign, base 40 coded as in the header.
    pub callsign: u32,
}

/// CRC-32 as the `ssdv` tool computes it (the usual 0xEDB88320 one).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// The callsign letters, as `ssdv -d` prints them.
pub fn callsign_string(code: u32) -> String {
    if code > 0xF423_FFFF {
        return String::new();
    }
    let mut c = code;
    let mut out = String::new();
    while c != 0 {
        let s = (c % 40) as u8;
        out.push(match s {
            0 => '-',
            1..=10 => (b'0' + s - 1) as char,
            11..=13 => '-',
            _ => (b'A' + s - 14) as char,
        });
        c /= 40;
    }
    out
}

/// Check a 256-byte packet and read its header. Errors on the sync byte, the type and the CRC.
pub fn parse(pkt: &[u8]) -> Result<Info, String> {
    if pkt.len() != PKT_LEN {
        return Err(format!("{} bytes, an SSDV packet is {PKT_LEN}", pkt.len()));
    }
    if pkt[0] != SYNC {
        return Err("sync byte is not 0x55".into());
    }
    if pkt[1] != TYPE_NOFEC {
        return Err(format!(
            "packet type 0x{:02x}, only no-FEC (0x67) is supported",
            pkt[1]
        ));
    }
    let want = u32::from_be_bytes(pkt[PKT_LEN - 4..].try_into().unwrap());
    if crc32(&pkt[1..PKT_LEN - 4]) != want {
        return Err("CRC mismatch".into());
    }
    Ok(Info {
        callsign: u32::from_be_bytes(pkt[2..6].try_into().unwrap()),
        image_id: pkt[6],
        packet_id: u16::from_be_bytes([pkt[7], pkt[8]]),
        width: (pkt[9] as u16) << 4,
        height: (pkt[10] as u16) << 4,
        eoi: pkt[11] & 4 != 0,
    })
}

/// The 255 bytes to transmit for a valid packet.
pub fn to_air(pkt: &[u8]) -> Result<[u8; AIR_LEN], String> {
    parse(pkt)?;
    Ok(pkt[1..].try_into().unwrap())
}

/// The packet back from 255 received bytes: the sync byte first, then the CRC check.
pub fn from_air(air: &[u8]) -> Result<[u8; PKT_LEN], String> {
    if air.len() != AIR_LEN {
        return Err(format!("{} bytes, an SSDV frame is {AIR_LEN}", air.len()));
    }
    let mut p = [SYNC; PKT_LEN];
    p[1..].copy_from_slice(air);
    parse(&p)?;
    Ok(p)
}

/// Split the output of `ssdv -e -n` into checked packets.
pub fn read_packets(file: &[u8]) -> Result<Vec<[u8; PKT_LEN]>, String> {
    if file.is_empty() || !file.len().is_multiple_of(PKT_LEN) {
        return Err(format!(
            "{} bytes is not a whole number of {PKT_LEN}-byte packets",
            file.len()
        ));
    }
    file.chunks(PKT_LEN)
        .enumerate()
        .map(|(i, c)| {
            parse(c).map_err(|e| format!("packet {i}: {e}"))?;
            Ok(c.try_into().unwrap())
        })
        .collect()
}

/// The packets of one image as they arrive, in any order and any number of times.
#[derive(Clone, Debug, Default)]
pub struct Assembly {
    packets: std::collections::BTreeMap<u16, [u8; PKT_LEN]>,
    last: Option<u16>,
    info: Option<Info>,
}

impl Assembly {
    /// Store a checked packet: `Ok(true)` for a new one, `Ok(false)` for a repeat. A
    /// repeat with different bytes, or a second end packet at another index, means two
    /// images share this id; that is an error and the packet is not stored.
    pub fn push(&mut self, pkt: [u8; PKT_LEN], info: Info) -> Result<bool, String> {
        if let Some(old) = self.packets.get(&info.packet_id) {
            return if *old == pkt {
                Ok(false)
            } else {
                Err(format!(
                    "packet {} repeated with different bytes",
                    info.packet_id
                ))
            };
        }
        if info.eoi {
            match self.last {
                Some(l) if l != info.packet_id => {
                    return Err(format!(
                        "end packet {} after end packet {l}",
                        info.packet_id
                    ))
                }
                _ => self.last = Some(info.packet_id),
            }
        }
        self.info.get_or_insert(info);
        self.packets.insert(info.packet_id, pkt);
        Ok(true)
    }

    /// Distinct packets held.
    pub fn have(&self) -> usize {
        self.packets.len()
    }

    /// Packets in the image, known once the last one has been heard.
    pub fn count(&self) -> Option<usize> {
        self.last.map(|l| l as usize + 1)
    }

    /// Header of the first packet heard.
    pub fn info(&self) -> Option<Info> {
        self.info
    }

    /// True when the last packet and every one before it are held, and nothing past it.
    pub fn complete(&self) -> bool {
        self.last.is_some_and(|l| {
            self.packets.len() == l as usize + 1 && self.packets.keys().copied().eq(0..=l)
        })
    }

    /// The held packets in order, as a file `ssdv -d` reads. Gaps stay gaps.
    pub fn file(&self) -> Vec<u8> {
        self.packets.values().flatten().copied().collect()
    }
}

/// Packet builders shared with the other modules' tests.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// A valid packet with the given id, built by hand from the format in `ssdv.c`.
    pub(crate) fn packet(image: u8, id: u16, eoi: bool) -> [u8; PKT_LEN] {
        let mut p = [0u8; PKT_LEN];
        p[0] = SYNC;
        p[1] = TYPE_NOFEC;
        p[2..6].copy_from_slice(&0x0001_0203u32.to_be_bytes());
        p[6] = image;
        p[7..9].copy_from_slice(&id.to_be_bytes());
        p[9] = 20;
        p[10] = 15;
        p[11] = if eoi { 4 } else { 0 };
        for (i, b) in p[15..PKT_LEN - 4].iter_mut().enumerate() {
            *b = (i as u16 ^ id) as u8;
        }
        let c = crc32(&p[1..PKT_LEN - 4]).to_be_bytes();
        p[PKT_LEN - 4..].copy_from_slice(&c);
        p
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::packet;
    use super::*;

    #[test]
    fn crc_is_the_usual_one() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn sizes_match_the_format() {
        assert_eq!(PAYLOAD_LEN, 237);
        assert_eq!(1 + HDR_LEN + PAYLOAD_LEN + 4, PKT_LEN);
    }

    #[test]
    fn dropping_the_sync_byte_round_trips() {
        let p = packet(9, 300, true);
        let air = to_air(&p).unwrap();
        assert_eq!(air.len(), 255);
        assert_eq!(air[0], TYPE_NOFEC);
        assert_eq!(from_air(&air).unwrap(), p);
        let i = parse(&p).unwrap();
        assert_eq!(
            (i.image_id, i.packet_id, i.width, i.height, i.eoi),
            (9, 300, 320, 240, true)
        );
    }

    #[test]
    fn bad_packets_fail_loud() {
        let p = packet(1, 2, false);
        let mut q = p;
        q[100] ^= 1;
        assert_eq!(parse(&q), Err("CRC mismatch".into()));
        assert!(to_air(&q).is_err());
        let mut air = to_air(&p).unwrap();
        air[200] ^= 0x10;
        assert!(from_air(&air).is_err());
        assert!(from_air(&air[..254]).is_err());
        let mut fec = p;
        fec[1] = 0x66;
        assert!(parse(&fec).unwrap_err().contains("no-FEC"));
        let mut nosync = p;
        nosync[0] = 0;
        assert!(parse(&nosync).is_err());
        assert!(parse(&p[..255]).is_err());
    }

    #[test]
    fn callsign_decodes() {
        // "TEST01" as the ssdv tool encodes it: base 40, last character first
        let mut code = 0u32;
        for c in "TEST01".chars().rev() {
            let v = match c {
                '0'..='9' => c as u32 - '0' as u32 + 1,
                _ => c as u32 - 'A' as u32 + 14,
            };
            code = code * 40 + v;
        }
        assert_eq!(callsign_string(code), "TEST01");
        assert_eq!(callsign_string(0xFFFF_FFFF), "");
    }

    #[test]
    fn file_reading_checks_every_packet() {
        let a = packet(1, 0, false);
        let b = packet(1, 1, true);
        let f = [a, b].concat();
        assert_eq!(read_packets(&f).unwrap(), vec![a, b]);
        assert!(read_packets(&f[..511]).is_err());
        assert!(read_packets(&[]).is_err());
        let mut bad = f.clone();
        bad[300] ^= 1;
        assert!(read_packets(&bad).unwrap_err().starts_with("packet 1"));
    }

    #[test]
    fn assembly_sorts_counts_repeats_and_keeps_gaps() {
        let ids = [(0u16, false), (1, false), (2, false), (3, true)];
        let mut a = Assembly::default();
        assert!(!a.complete());
        for &(id, eoi) in ids.iter().rev().filter(|x| x.0 != 1) {
            let p = packet(5, id, eoi);
            assert_eq!(a.push(p, parse(&p).unwrap()), Ok(true));
        }
        let p3 = packet(5, 3, true);
        assert_eq!(a.push(p3, parse(&p3).unwrap()), Ok(false));
        assert_eq!((a.have(), a.count(), a.complete()), (3, Some(4), false));
        assert_eq!(a.file().len(), 3 * PKT_LEN);
        assert_eq!(a.file()[PKT_LEN + 8], 2);
        let p1 = packet(5, 1, false);
        a.push(p1, parse(&p1).unwrap()).unwrap();
        assert!(a.complete());
        let f = a.file();
        assert_eq!(read_packets(&f).unwrap().len(), 4);
        assert_eq!(a.info().unwrap().image_id, 5);
    }

    #[test]
    fn no_end_packet_means_no_count() {
        let mut a = Assembly::default();
        let p = packet(1, 0, false);
        a.push(p, parse(&p).unwrap()).unwrap();
        assert_eq!(a.count(), None);
        assert!(!a.complete());
    }

    #[test]
    fn a_stray_packet_past_the_end_is_not_a_complete_image() {
        // packets 0, 1, 3 of a 4-packet image plus packet 5 of a longer one: four held,
        // end at 3, but packet 2 is missing
        let mut a = Assembly::default();
        for (id, eoi) in [(0, false), (1, false), (5, false), (3, true)] {
            let p = packet(5, id, eoi);
            a.push(p, parse(&p).unwrap()).unwrap();
        }
        assert_eq!((a.have(), a.count()), (4, Some(4)));
        assert!(!a.complete());
    }

    #[test]
    fn conflicting_repeats_and_two_ends_are_refused() {
        let mut a = Assembly::default();
        let p = packet(5, 2, false);
        a.push(p, parse(&p).unwrap()).unwrap();
        let other = packet(6, 2, false); // same index, other bytes
        assert!(a.push(other, parse(&other).unwrap()).is_err());
        assert_eq!(a.file(), p.to_vec(), "the first packet is kept");
        let e = packet(5, 3, true);
        a.push(e, parse(&e).unwrap()).unwrap();
        let e2 = packet(5, 9, true);
        assert!(a.push(e2, parse(&e2).unwrap()).is_err());
        assert_eq!(a.count(), Some(4));
    }
}
