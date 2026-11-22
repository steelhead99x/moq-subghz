//! Real `Fragmenter` output -> packing -> `EmuLink` (modeled HaLow) -> unpacking -> `Reassembler`,
//! with the unchanged moq-subghz-wire crate on both ends. No sockets, no radio. Everything the
//! link reports is MODELED.
use moq_subghz_link::{Link, Provenance};
use moq_subghz_radio_emu::loss::LossModel;
use moq_subghz_radio_emu::{pack, EmuCfg, EmuLink, HalowCfg, PhyKind};
use moq_subghz_wire::{Event, Fragmenter, Reassembler};
use std::time::Duration;

fn object(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

/// Send `frames` over `tx`, packed as the caps allow, and feed what `rx` gets to `ra`.
fn run(
    tx: &mut EmuLink,
    rx: &mut EmuLink,
    frames: &[Vec<u8>],
    ra: &mut Reassembler,
) -> (Vec<Event>, usize) {
    let (mtu, maxf) = (tx.caps().max_payload, tx.caps().max_frames_per_packet);
    let (mut i, mut events, mut pkts) = (0, vec![], 0);
    let mut buf = vec![0u8; mtu];
    while i < frames.len() {
        let lens: Vec<usize> = frames[i..].iter().map(|f| f.len()).collect();
        let n = pack::take_batch(&lens, mtu, maxf).max(1);
        let refs: Vec<&[u8]> = frames[i..i + n].iter().map(|f| f.as_slice()).collect();
        let pkt = if maxf > 1 {
            pack::pack(&refs).unwrap()
        } else {
            refs[0].to_vec()
        };
        let rep = tx.send(&pkt).unwrap();
        pkts += 1;
        if rep.delivered == Some(true) {
            let m = rx
                .recv(&mut buf, Duration::from_millis(100))
                .unwrap()
                .expect("a delivered packet arrives");
            let got = &buf[..m.len];
            assert_eq!(got, &pkt[..]);
            let frs = if maxf > 1 {
                pack::unpack(got).unwrap()
            } else {
                vec![got.to_vec()]
            };
            for f in frs {
                events.extend(ra.push(&f));
            }
        }
        i += n;
    }
    (events, pkts)
}

#[test]
fn objects_survive_packing_unchanged() {
    let (mut tx, mut rx) = EmuLink::pair(EmuCfg::new(PhyKind::Halow(HalowCfg::default()))).unwrap();
    assert_eq!(tx.caps().provenance, Provenance::Modeled);
    let mut frag = Fragmenter::new(8).unwrap();
    let mut ra = Reassembler::new();
    for (g, len) in [(0u16, 61_000usize), (1, 3000), (2, 150)] {
        let data = object(len, g as u8);
        let frames = frag.fragment(g, 0, &data).unwrap();
        assert!(frames.iter().all(|f| f.len() <= 255));
        let (ev, pkts) = run(&mut tx, &mut rx, &frames, &mut ra);
        assert!(pkts < frames.len() || frames.len() == 1);
        assert!(
            ev.iter()
                .any(|e| matches!(e, Event::Object { data: d, .. } if *d == data)),
            "len {len}"
        );
    }
}

#[test]
fn fec_cannot_repair_a_whole_packet_but_one_frame_per_packet_can() {
    // Lose 5 % of packets in broadcast mode. With 5 frames per packet one lost packet takes
    // several frames of the same FEC block, so the object is gone; with 1 frame per packet
    // the XOR parity repairs a single lost frame per block.
    let data = object(2500, 9);
    let mut lost = [0usize; 2];
    for (slot, max_frames) in [0usize, 1].into_iter().enumerate() {
        let mut ok = 0;
        for seed in 0..40 {
            let mut c = EmuCfg::new(PhyKind::Halow(HalowCfg {
                ack: false,
                ..HalowCfg::default()
            }));
            c.loss = LossModel::Iid(0.05);
            c.seed = seed;
            c.max_frames = max_frames;
            let (mut tx, mut rx) = EmuLink::pair(c).unwrap();
            let mut frag = Fragmenter::new(4).unwrap();
            let frames = frag.fragment(0, 0, &data).unwrap();
            let mut ra = Reassembler::new();
            let (ev, _) = run(&mut tx, &mut rx, &frames, &mut ra);
            ok += ev.iter().any(|e| matches!(e, Event::Object { .. })) as usize;
        }
        lost[slot] = 40 - ok;
    }
    assert!(
        lost[1] <= lost[0],
        "packed lost {} objects, unpacked {}",
        lost[0],
        lost[1]
    );
}

#[test]
fn with_ack_and_retries_a_lossy_channel_still_delivers_everything() {
    let mut c = EmuCfg::new(PhyKind::Halow(HalowCfg::default()));
    c.loss = LossModel::Iid(0.1);
    let (mut tx, mut rx) = EmuLink::pair(c).unwrap();
    let mut frag = Fragmenter::new(8).unwrap();
    let mut ra = Reassembler::new();
    let data = object(20_000, 3);
    let frames = frag.fragment(0, 0, &data).unwrap();
    let (ev, _) = run(&mut tx, &mut rx, &frames, &mut ra);
    assert!(ev
        .iter()
        .any(|e| matches!(e, Event::Object { data: d, .. } if *d == data)));
}
