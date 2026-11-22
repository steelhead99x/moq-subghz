//! Golden test for wire v5: the exact frames that go on the air for a fixed set of synthetic
//! objects, at FEC k = 0, 4 and 8, sealed and unsealed, are hashed with SHA-256 and compared
//! with committed values. If one of these changes, a byte on the air changed: the published
//! results need measuring again, and the change must say so (CONTRIBUTING.md).
//!
//! Needs no radio and no ffmpeg: it uses the wire library only. Refactors that must not touch the
//! air (moving radio code behind a trait, feature gates) have to keep this green.

use moq_subghz_wire::seal::{Cipher, ObjectSealer, SALT_LEN};
use moq_subghz_wire::{Fragmenter, ObjHdr, MAX_FRAME};

// Minimal SHA-256 (FIPS 180-4), so the test adds no dependency. Checked against known vectors below.
fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes(c.try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for (x, y) in h.iter_mut().zip(v) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (o, x) in out.chunks_exact_mut(4).zip(h) {
        o.copy_from_slice(&x.to_be_bytes());
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Deterministic pseudo-random bytes (xorshift), so the objects are fixed without a dependency.
fn synth(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// (group, obj, key, access-unit length): two groups, a first object big enough to need several
/// frames, short objects (one frame), sizes that leave a short last block, and the largest object.
const OBJECTS: &[(u16, u8, bool, usize)] = &[
    (0, 0, true, 3000),
    (0, 1, false, 200),
    (0, 2, false, 1),
    (0, 3, false, 1300),
    (0, 4, false, 238),
    (0, 5, false, 239),
    (1, 0, true, 9000),
    (1, 1, false, 60),
    (1, 2, false, 2400),
    (2, 0, true, 61900),
];

const KEY: [u8; 32] = *b"moq-subghz golden test key 32 B!";
const SALT: [u8; SALT_LEN] = *b"golden-salt\0";

/// The frames the sender would put on the air for `OBJECTS`, as one length-prefixed byte stream.
fn stream(k: u8, sealed: bool) -> (Vec<u8>, usize) {
    let mut frag = Fragmenter::new(k).unwrap();
    let mut sealer = ObjectSealer::new(Cipher::ChaCha20Poly1305, &KEY).unwrap();
    let (mut out, mut n) = (Vec::new(), 0);
    for (i, &(group, obj, key, len)) in OBJECTS.iter().enumerate() {
        let hdr = ObjHdr {
            frame_no: i as u32,
            cap_us: 1_000_000 + 33_333 * i as u64,
            enc_us: 4_000 + i as u32,
            key,
        };
        let mut data = hdr.encode().to_vec();
        data.extend_from_slice(&synth(len, 0x9e37_79b9 + i as u64));
        if sealed {
            data = sealer.seal(&SALT, group, obj, &data);
        }
        for f in frag.fragment(group, obj, &data).unwrap() {
            assert!(f.len() <= MAX_FRAME);
            out.push(f.len() as u8);
            out.extend_from_slice(&f);
            n += 1;
        }
    }
    (out, n)
}

#[test]
fn sha256_known_vectors() {
    assert_eq!(
        hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(&sha256(&[b'a'; 1000])),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}

/// (k, sealed, frame count, SHA-256 of the length-prefixed frame stream)
const GOLDEN: &[(u8, bool, usize, &str)] = &[
    (
        0,
        false,
        329,
        "a26b1fcb86da518bb8a1a13fbd4c0bd668a719e955a41d013a736223cb3fcfa5",
    ),
    (
        0,
        true,
        330,
        "f0ddbac1cd3e3f0abe0da440bd4bdc509b663b969aefe3f583f98fc71182a31d",
    ),
    (
        4,
        false,
        417,
        "96409224fd402ca9028e272be340fea227c648fe515c84efa271a3e6f1601af2",
    ),
    (
        4,
        true,
        418,
        "947462fd0006db79662952cc84797f2a04697be6d5df18ce2130f37c67481ac5",
    ),
    (
        8,
        false,
        376,
        "0e0f81a3a16e8a8c7c290f9eacfb250f8bf9d722b125a64e22827663732546aa",
    ),
    (
        8,
        true,
        377,
        "c62921c5e917a31a80c5d95e1f9375cb72aaa5584634e7aa1ad752c8c921a05d",
    ),
];

#[test]
fn on_air_frame_stream_is_unchanged() {
    for &(k, sealed, count, hash) in GOLDEN {
        let (s, n) = stream(k, sealed);
        assert_eq!(n, count, "frame count, k={k} sealed={sealed}");
        assert_eq!(
            hex(&sha256(&s)),
            hash,
            "frame stream, k={k} sealed={sealed}"
        );
    }
}

/// Run `cargo test -p moq-subghz-wire --test golden_v5 -- --ignored --nocapture` to print the
/// table for `GOLDEN`. Only update it for a deliberate change to what goes on the air.
#[test]
#[ignore]
fn print_golden() {
    for k in [0, 4, 8] {
        for sealed in [false, true] {
            let (s, n) = stream(k, sealed);
            println!("    ({k}, {sealed}, {n}, \"{}\"),", hex(&sha256(&s)));
        }
    }
}
