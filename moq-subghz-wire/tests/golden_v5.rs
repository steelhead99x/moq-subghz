//! Golden test for wire v5: the exact frames that go on the air for a fixed set of synthetic
//! objects, at every FEC k from 0 to 15, sealed and unsealed, padded to a minimum frame size, and
//! across the 16-bit sequence wrap, are hashed with SHA-256 and compared with committed values.
//! A receive-side test feeds the same frames back through the `Reassembler`, with and without
//! single losses per FEC block, and checks every object comes back byte for byte. If one of these changes, a byte on the air changed: the published
//! results need measuring again, and the change must say so (CONTRIBUTING.md).
//!
//! Needs no radio and no ffmpeg: it uses the wire library only. Refactors that must not touch the
//! air (moving radio code behind a trait, feature gates) have to keep this green.

use moq_subghz_wire::seal::{Cipher, ObjectSealer, SALT_LEN};
use moq_subghz_wire::{plan, Event, Fragmenter, FrameHdr, ObjHdr, Reassembler, MAX_FRAME};

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

/// One way of sending `OBJECTS`: FEC block size, sealing, `min_frame` padding, first sequence number.
#[derive(Clone, Copy)]
struct Run {
    k: u8,
    sealed: bool,
    min_frame: usize,
    seq0: u16,
}

const fn run(k: u8, sealed: bool) -> Run {
    Run {
        k,
        sealed,
        min_frame: 0,
        seq0: 0,
    }
}

/// The frames the sender would put on the air for `OBJECTS`, as one length-prefixed byte stream.
fn stream(k: u8, sealed: bool) -> (Vec<u8>, usize) {
    prefixed(&frames(run(k, sealed)).0)
}

fn prefixed(frames: &[Vec<u8>]) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    for f in frames {
        out.push(f.len() as u8);
        out.extend_from_slice(f);
    }
    (out, frames.len())
}

/// An object as handed to the fragmenter: group, object number, bytes.
type Obj = (u16, u8, Vec<u8>);

/// The frames of `r`, and the bytes each object carried into the fragmenter (what a receiver
/// must give back).
fn frames(r: Run) -> (Vec<Vec<u8>>, Vec<Obj>) {
    let mut frag = Fragmenter::new(r.k).unwrap();
    frag.min_frame = r.min_frame;
    frag.seq = r.seq0;
    let sealed = r.sealed;
    let mut sealer = ObjectSealer::new(Cipher::ChaCha20Poly1305, &KEY).unwrap();
    let (mut out, mut objs) = (Vec::new(), Vec::new());
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
            out.push(f);
        }
        objs.push((group, obj, data));
    }
    (out, objs)
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

/// The rest of the air interface, one row per run: every FEC k not in `GOLDEN` (unsealed), sealed
/// at the smallest and largest k, `min_frame` padding (every frame at least that long, so short
/// last chunks and parity frames are padded) with and without FEC, and runs whose 16-bit frame
/// sequence number wraps from 65535 to 0 inside the stream and starts on 65535.
/// (run, frame count, SHA-256 of the length-prefixed frame stream)
const EXTRA: &[(Run, usize, &str)] = &[
    (
        Run {
            k: 1,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        658,
        "a0b40af0d2e0d0d70692bdf6b925e57c3e8301bf6cc14e63e0e9b2af4de441e2",
    ),
    (
        Run {
            k: 2,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        496,
        "21948bf0bb25318dfb405722e615a95213d3c1e9640fc46c341492701c8db695",
    ),
    (
        Run {
            k: 3,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        443,
        "67d884595eec5fa14638d0c93b97f030313348726a6688d6f4754eb3129f1789",
    ),
    (
        Run {
            k: 5,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        400,
        "f997759edad2f6a606067c328e290a841473477b19cc8de0f2f1019b6864afd1",
    ),
    (
        Run {
            k: 6,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        390,
        "10408e1b7db8f3550aeeaa1aaa8bedcab293ef84113b95201fe1771be468b4b0",
    ),
    (
        Run {
            k: 7,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        382,
        "496c5354efc61c23c9690b00abeca6c6a5e588148663993886067b719cf3863a",
    ),
    (
        Run {
            k: 9,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        373,
        "26a265b229ccc549761dd5e2f3a4cd6752c96380c774ee44267040ac33219a1b",
    ),
    (
        Run {
            k: 10,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        368,
        "9606f218275cddaf4949bc8ffe4173099b552a970de1cb2245249e876026c6c8",
    ),
    (
        Run {
            k: 11,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        366,
        "48bd1bb33829e70bf25c58bdfb027d5f039e80873fce95ae493b72f36353070b",
    ),
    (
        Run {
            k: 12,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        364,
        "07c8572616e50a5d0c045587078aab94ef9f42f8a360feed5c38885c6ddc70cb",
    ),
    (
        Run {
            k: 13,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        360,
        "f2c1afaf82ca8be096c5ca1c99d9b32eda52360da223aea39ccd62d14d45c50d",
    ),
    (
        Run {
            k: 14,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        359,
        "d52fde335ede55f21cf4167aa5bb84fa7e18c199316628574d2f71bc055a3594",
    ),
    (
        Run {
            k: 15,
            sealed: false,
            min_frame: 0,
            seq0: 0,
        },
        357,
        "d233362a2bec4283045d1bc84d2dd4fcd50177090f70d717aad3347627f0a506",
    ),
    (
        Run {
            k: 1,
            sealed: true,
            min_frame: 0,
            seq0: 0,
        },
        660,
        "c46440586f3950583b1f958e8f2d7e8fbd022d64a9aca3edc4e8c0d28fb10853",
    ),
    (
        Run {
            k: 15,
            sealed: true,
            min_frame: 0,
            seq0: 0,
        },
        358,
        "e265c95306d13dd9a1207eff6b473bc13b74fef690d1c026165922e6ed11205a",
    ),
    (
        Run {
            k: 0,
            sealed: false,
            min_frame: 255,
            seq0: 0,
        },
        329,
        "e51562d1731566f8dc170eec407562afdcb193d3a33540bd7df1a95fd1247a7c",
    ),
    (
        Run {
            k: 4,
            sealed: false,
            min_frame: 255,
            seq0: 0,
        },
        417,
        "395d03cb9b0f2f711eab7504c0e4ea3866a6c4715a7bcedb5e1193fffd14f09b",
    ),
    (
        Run {
            k: 4,
            sealed: true,
            min_frame: 100,
            seq0: 0,
        },
        418,
        "703b7dbd5a2dbc6d8a3f8d891f3efd1a427912b640f56720b26957b369fe689c",
    ),
    (
        Run {
            k: 4,
            sealed: false,
            min_frame: 0,
            seq0: 65500,
        },
        417,
        "dfdada40f19f77b1ca9d5cfc3f0ad8e28bc0c360f17cfa5ef2471dfd66f5dd5a",
    ),
    (
        Run {
            k: 0,
            sealed: true,
            min_frame: 0,
            seq0: 65535,
        },
        330,
        "20eefebb1247fa74be3b334c67c1fa3ce52a3b36fed6a2eca9ca2f097b040b63",
    ),
    (
        Run {
            k: 15,
            sealed: false,
            min_frame: 0,
            seq0: 65530,
        },
        357,
        "4068a97ff6dc5283c14a1acca948cb3683dda6fcdbc9eb32358837e41999b745",
    ),
];

#[test]
fn the_rest_of_the_air_interface_is_unchanged() {
    let want: Vec<Run> = extra_runs();
    assert_eq!(want.len(), EXTRA.len(), "EXTRA and extra_runs() disagree");
    for (r, &(g, count, hash)) in want.iter().zip(EXTRA) {
        assert_eq!(
            (r.k, r.sealed, r.min_frame, r.seq0),
            (g.k, g.sealed, g.min_frame, g.seq0)
        );
        let (s, n) = prefixed(&frames(*r).0);
        let what = format!(
            "k={} sealed={} min_frame={} seq0={}",
            r.k, r.sealed, r.min_frame, r.seq0
        );
        assert_eq!(n, count, "frame count, {what}");
        assert_eq!(hex(&sha256(&s)), hash, "frame stream, {what}");
    }
}

/// The hashes pin today's bytes; this checks them against the format's own arithmetic: `plan`
/// gives the length of every frame without touching data, the sequence numbers run on by one and
/// wrap, parity frames are flagged, and `min_frame` pads every frame.
#[test]
fn frames_match_the_plan_and_the_sequence_runs_on() {
    let mut runs: Vec<Run> = (0..=15).map(|k| run(k, false)).collect();
    runs.extend(extra_runs());
    for r in runs {
        let (fs, objs) = frames(r);
        let mut want = Vec::new();
        for (_, _, data) in &objs {
            want.extend(plan(data.len(), r.k, r.min_frame).unwrap());
        }
        assert_eq!(
            fs.iter().map(Vec::len).collect::<Vec<_>>(),
            want,
            "plan, k={}",
            r.k
        );
        let mut seq = r.seq0;
        for f in &fs {
            let (h, _) = FrameHdr::decode(f).expect("every frame decodes");
            assert_eq!(h.seq, seq);
            assert_eq!(h.k, r.k);
            assert!(!h.parity || r.k > 0, "parity frames need k > 0");
            seq = seq.wrapping_add(1);
        }
        if r.seq0 >= 65_500 {
            assert!(fs.len() > (65_536 - r.seq0 as usize), "the run wraps");
        }
    }
}

/// The objects a receiver must produce, in order: every `OBJECTS` entry, byte for byte.
fn expect_all(events: &[Event], objs: &[Obj], what: &str) -> u32 {
    let mut recovered = 0;
    let got: Vec<_> = events
        .iter()
        .map(|e| match e {
            Event::Object {
                group,
                obj,
                data,
                recovered: r,
            } => {
                recovered += r;
                (*group, *obj, data.clone())
            }
            Event::Lost {
                group,
                obj,
                have,
                need,
            } => {
                panic!("{what}: object {group}/{obj} lost ({have} of {need})")
            }
        })
        .collect();
    assert_eq!(got.len(), objs.len(), "{what}: object count");
    for (g, o) in got.iter().zip(objs) {
        assert_eq!(g, o, "{what}: object {}/{}", o.0, o.1);
    }
    recovered
}

fn receive(frames: impl IntoIterator<Item = Vec<u8>>) -> (Reassembler, Vec<Event>) {
    let mut re = Reassembler::new();
    let mut ev = Vec::new();
    for f in frames {
        ev.extend(re.push(&f));
    }
    ev.extend(re.finish());
    (re, ev)
}

#[test]
fn the_receiver_gets_every_object_back() {
    let mut runs: Vec<Run> = (0..=15).map(|k| run(k, false)).collect();
    runs.extend(extra_runs());
    for r in runs {
        let (fs, objs) = frames(r);
        let what = format!(
            "k={} sealed={} min_frame={} seq0={}",
            r.k, r.sealed, r.min_frame, r.seq0
        );
        let n = fs.len() as u64;
        let (re, ev) = receive(fs);
        assert_eq!(
            expect_all(&ev, &objs, &what),
            0,
            "{what}: nothing needed recovering"
        );
        // the sequence wrap is not a loss, and no frame was refused
        assert_eq!(
            (re.frames_ok, re.frames_missing, re.frames_bad),
            (n, 0, 0),
            "{what}"
        );
    }
}

#[test]
fn one_loss_per_fec_block_is_recovered_and_without_fec_it_is_not() {
    for k in 1..=15u8 {
        let r = run(k, false);
        let (fs, objs) = frames(r);
        // drop the first data frame of every block that has its parity
        let kept: Vec<Vec<u8>> = fs
            .iter()
            .filter(|f| {
                let (h, _) = FrameHdr::decode(f).unwrap();
                h.parity || h.idx % k != 0
            })
            .cloned()
            .collect();
        let dropped = fs.len() - kept.len();
        assert!(dropped > 0);
        let (_, ev) = receive(kept);
        let recovered = expect_all(&ev, &objs, &format!("k={k}"));
        assert_eq!(
            recovered as usize, dropped,
            "k={k}: every drop rebuilt from parity"
        );
    }
    // k = 0: dropping one frame of the 3000 byte first object loses exactly that object
    let (fs, objs) = frames(run(0, false));
    let kept: Vec<Vec<u8>> = fs
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != 3)
        .map(|(_, f)| f.clone())
        .collect();
    let (_, ev) = receive(kept);
    let lost: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            Event::Lost { group, obj, .. } => Some((*group, *obj)),
            _ => None,
        })
        .collect();
    assert_eq!(lost, [(objs[0].0, objs[0].1)]);
    assert_eq!(ev.len(), objs.len());
}

/// Run `cargo test -p moq-subghz-wire --test golden_v5 -- --ignored --nocapture` to print the
/// tables for `GOLDEN` and `EXTRA`. Only update them for a deliberate change to what goes on the
/// air.
#[test]
#[ignore]
fn print_golden() {
    for k in [0, 4, 8] {
        for sealed in [false, true] {
            let (s, n) = stream(k, sealed);
            println!("    ({k}, {sealed}, {n}, \"{}\"),", hex(&sha256(&s)));
        }
    }
    for r in extra_runs() {
        let (s, n) = prefixed(&frames(r).0);
        println!(
            "    (Run {{ k: {}, sealed: {}, min_frame: {}, seq0: {} }}, {n}, \"{}\"),",
            r.k,
            r.sealed,
            r.min_frame,
            r.seq0,
            hex(&sha256(&s))
        );
    }
}

fn extra_runs() -> Vec<Run> {
    let mut v: Vec<Run> = [1u8, 2, 3, 5, 6, 7, 9, 10, 11, 12, 13, 14, 15]
        .into_iter()
        .map(|k| run(k, false))
        .collect();
    v.push(run(1, true));
    v.push(run(15, true));
    for (k, sealed, min_frame) in [(0, false, 255), (4, false, 255), (4, true, 100)] {
        v.push(Run {
            k,
            sealed,
            min_frame,
            seq0: 0,
        });
    }
    // 417 frames from 65500 wrap past 65535; starting on 65535 wraps at the second frame
    for (k, sealed, seq0) in [(4, false, 65_500), (0, true, 65_535), (15, false, 65_530)] {
        v.push(Run {
            k,
            sealed,
            min_frame: 0,
            seq0,
        });
    }
    v
}
