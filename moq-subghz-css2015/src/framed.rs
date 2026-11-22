//! framed-uplink 1.0.x unconfirmed uplink framing: MHDR, DevAddr, FCtrl, FCnt, FPort, encrypted
//! payload and a four-byte MIC. This builds and checks the bytes only. There is no join,
//! no gateway and no network server, so a run with it is "framed, no network".
//!
//! The keys here are test keys, public on purpose.

use aes::cipher::{BlockCipherEncrypt, KeyInit};
use aes::Aes128;
use cmac::{Cmac, Mac};

/// MHDR of an unconfirmed data uplink (MType 010, major 00).
pub const MHDR_UNCONFIRMED_UP: u8 = 0x40;
/// Bytes around the payload: MHDR, DevAddr, FCtrl, FCnt, FPort, MIC.
pub const OVERHEAD: usize = 13;
/// Largest application payload that fits a 255 byte frame.
pub const MAX_PAYLOAD: usize = 255 - OVERHEAD;
/// Device address used by the test.
pub const TEST_DEV_ADDR: u32 = 0x0102_0304;
/// NwkSKey and AppSKey used by the test: the RFC 4493 example key.
pub const TEST_KEY: [u8; 16] = [
    0x2B, 0x7E, 0x15, 0x16, 0x28, 0xAE, 0xD2, 0xA6, 0xAB, 0xF7, 0x15, 0x88, 0x09, 0xCF, 0x4F, 0x3C,
];

/// What the frame says about itself.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Uplink {
    /// Device address.
    pub dev_addr: u32,
    /// Frame counter. Only the low 16 bits are on the air.
    pub fcnt: u16,
    /// Port, 1..=223.
    pub fport: u8,
}

/// Block A or B0: kind, four zeros, direction (0, uplink), DevAddr, FCnt as 32 bits, zero, last byte.
fn block(kind: u8, dev_addr: u32, fcnt: u16, last: u8) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0] = kind;
    b[6..10].copy_from_slice(&dev_addr.to_le_bytes());
    b[10..12].copy_from_slice(&fcnt.to_le_bytes());
    b[15] = last;
    b
}

/// The payload cipher (spec 4.3.3.1): XOR with AES of numbered blocks. It is its own inverse.
fn crypt(key: &[u8; 16], dev_addr: u32, fcnt: u16, data: &[u8]) -> Vec<u8> {
    let aes = Aes128::new(key.into());
    data.chunks(16)
        .enumerate()
        .flat_map(|(i, c)| {
            let mut s = block(0x01, dev_addr, fcnt, i as u8 + 1).into();
            aes.encrypt_block(&mut s);
            c.iter().zip(s).map(|(d, k)| d ^ k).collect::<Vec<_>>()
        })
        .collect()
}

/// The MIC (spec 4.4): the first four bytes of AES-CMAC over B0 and the frame without its MIC.
fn mic(key: &[u8; 16], dev_addr: u32, fcnt: u16, msg: &[u8]) -> [u8; 4] {
    let mut m = Cmac::<Aes128>::new(key.into());
    m.update(&block(0x49, dev_addr, fcnt, msg.len() as u8));
    m.update(msg);
    m.finalize().into_bytes()[..4].try_into().unwrap()
}

/// Build an unconfirmed uplink carrying `payload`.
pub fn encode(key: &[u8; 16], up: &Uplink, payload: &[u8]) -> Result<Vec<u8>, String> {
    if !(1..=223).contains(&up.fport) {
        return Err(format!("FPort {} is outside 1..=223", up.fport));
    }
    if payload.len() > MAX_PAYLOAD {
        return Err(format!(
            "{} byte payload, a frame carries {MAX_PAYLOAD} at most",
            payload.len()
        ));
    }
    let mut f = vec![MHDR_UNCONFIRMED_UP];
    f.extend_from_slice(&up.dev_addr.to_le_bytes());
    f.push(0); // FCtrl: no ADR, no ACK, no FOpts
    f.extend_from_slice(&up.fcnt.to_le_bytes());
    f.push(up.fport);
    f.extend(crypt(key, up.dev_addr, up.fcnt, payload));
    let m = mic(key, up.dev_addr, up.fcnt, &f);
    f.extend_from_slice(&m);
    Ok(f)
}

/// Check the MIC and unwrap a frame built by [`encode`].
pub fn decode(key: &[u8; 16], frame: &[u8]) -> Result<(Uplink, Vec<u8>), String> {
    if frame.len() < OVERHEAD {
        return Err(format!(
            "{} bytes, shorter than the {OVERHEAD} byte frame",
            frame.len()
        ));
    }
    if frame[0] != MHDR_UNCONFIRMED_UP {
        return Err(format!(
            "MHDR 0x{:02x}, not an unconfirmed uplink",
            frame[0]
        ));
    }
    if frame[5] != 0 {
        return Err("FCtrl is not zero, FOpts are not supported".into());
    }
    let (msg, got) = frame.split_at(frame.len() - 4);
    let up = Uplink {
        dev_addr: u32::from_le_bytes(frame[1..5].try_into().unwrap()),
        fcnt: u16::from_le_bytes([frame[6], frame[7]]),
        fport: frame[8],
    };
    if mic(key, up.dev_addr, up.fcnt, msg) != got {
        return Err("MIC mismatch".into());
    }
    Ok((up, crypt(key, up.dev_addr, up.fcnt, &msg[9..])))
}

#[cfg(test)]
mod tests {
    use super::*;

    const UP: Uplink = Uplink {
        dev_addr: 0x0102_0304,
        fcnt: 5,
        fport: 1,
    };

    // Computed with Python `cryptography` (CMAC and AES-ECB) from the spec's block layouts,
    // not with this code.
    const PAYLOAD: [u8; 20] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20,
    ];
    const FRAME: &str = "4004030201000500012a1fbf6ba648991ee8675a4bb34e93a26ef8ac1b9817ad58";

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    #[test]
    fn matches_the_independent_vector() {
        assert_eq!(hex(&encode(&TEST_KEY, &UP, &PAYLOAD).unwrap()), FRAME);
    }

    #[test]
    fn cmac_primitive_matches_rfc4493() {
        let mut m = Cmac::<Aes128>::new((&TEST_KEY).into());
        m.update(&[]);
        assert_eq!(
            hex(&m.finalize().into_bytes()),
            "bb1d6929e95937287fa37d129b756746"
        );
    }

    #[test]
    fn round_trips_and_sizes() {
        for n in [0, 1, 16, 17, 100, MAX_PAYLOAD] {
            let p: Vec<u8> = (0..n).map(|i| i as u8 ^ 0xA5).collect();
            let f = encode(&TEST_KEY, &UP, &p).unwrap();
            assert_eq!(f.len(), n + OVERHEAD);
            assert!(f.len() <= 255);
            assert_eq!(decode(&TEST_KEY, &f).unwrap(), (UP, p));
        }
        assert_eq!(MAX_PAYLOAD, 242);
    }

    #[test]
    fn tampering_and_wrong_keys_fail() {
        let f = encode(&TEST_KEY, &UP, &PAYLOAD).unwrap();
        for i in 0..f.len() {
            let mut g = f.clone();
            g[i] ^= 0x01;
            assert!(decode(&TEST_KEY, &g).is_err(), "flip in byte {i}");
        }
        let mut other = TEST_KEY;
        other[0] ^= 1;
        assert_eq!(decode(&other, &f), Err("MIC mismatch".into()));
        assert!(decode(&TEST_KEY, &f[..12]).is_err());
    }

    #[test]
    fn counter_changes_the_ciphertext_and_the_mic() {
        let a = encode(&TEST_KEY, &UP, &PAYLOAD).unwrap();
        let b = encode(&TEST_KEY, &Uplink { fcnt: 6, ..UP }, &PAYLOAD).unwrap();
        assert_ne!(a[9..29], b[9..29]);
        assert_ne!(a[29..], b[29..]);
    }

    #[test]
    fn refuses_what_a_frame_cannot_hold() {
        assert!(encode(&TEST_KEY, &UP, &[0; MAX_PAYLOAD + 1]).is_err());
        assert!(encode(&TEST_KEY, &Uplink { fport: 0, ..UP }, &[1]).is_err());
        assert!(encode(&TEST_KEY, &Uplink { fport: 224, ..UP }, &[1]).is_err());
    }
}
