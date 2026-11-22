use moq_subghz_link::{pair, Link, LinkError, LinkKind, LoopbackCfg, Loss, Provenance, MAX_FRAME};
use std::time::Duration;

const NOW: Duration = Duration::ZERO;

fn rx_all(l: &mut impl Link) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 512];
    while let Some(m) = l.recv(&mut buf, NOW).unwrap() {
        out.push(buf[..m.len].to_vec());
    }
    out
}

#[test]
fn max_frame_is_the_wire_limit() {
    assert_eq!(MAX_FRAME, 255);
    let (a, _b) = pair(LoopbackCfg::default());
    assert_eq!(a.caps().max_payload, 255);
}

#[test]
fn caps_describe_a_modeled_two_way_link() {
    let (a, _b) = pair(LoopbackCfg::default());
    let c = a.caps();
    assert_eq!((c.kind, c.provenance), (LinkKind::Emu, Provenance::Modeled));
    assert!(c.tx && c.rx && !c.rx_needs_poll);
    assert_eq!(c.max_frames_per_packet, 1);
}

#[test]
fn packets_arrive_in_order_both_ways() {
    let (mut a, mut b) = pair(LoopbackCfg::default());
    for i in 0..5u8 {
        a.send(&[i; 10]).unwrap();
    }
    b.send(&[9, 9]).unwrap();
    assert_eq!(
        rx_all(&mut b),
        (0..5u8).map(|i| vec![i; 10]).collect::<Vec<_>>()
    );
    assert_eq!(rx_all(&mut a), vec![vec![9, 9]]);
}

#[test]
fn timeout_is_none() {
    let (mut a, _b) = pair(LoopbackCfg::default());
    let mut buf = [0u8; 8];
    assert_eq!(a.recv(&mut buf, Duration::from_millis(5)).unwrap(), None);
}

#[test]
fn too_long_and_empty_are_refused() {
    let (mut a, mut b) = pair(LoopbackCfg::default());
    a.send(&[0; 255]).unwrap();
    assert_eq!(a.send(&[0; 256]), Err(LinkError::TooLong));
    assert_eq!(a.send(&[]), Err(LinkError::Empty));
    // nothing was queued for the refused sends
    assert_eq!(rx_all(&mut b).len(), 1);
}

#[test]
fn short_receive_buffer_is_an_error_not_a_truncation() {
    let (mut a, mut b) = pair(LoopbackCfg::default());
    a.send(&[7; 100]).unwrap();
    let mut small = [0u8; 99];
    assert_eq!(b.recv(&mut small, NOW), Err(LinkError::TooLong));
    assert_eq!(b.recv(&mut small, NOW).unwrap(), None);
}

#[test]
fn every_nth_is_dropped_and_reported() {
    let cfg = LoopbackCfg {
        loss: Loss::Every(3),
        ..Default::default()
    };
    let (mut a, mut b) = pair(cfg);
    let delivered: Vec<_> = (0..9u8)
        .map(|i| a.send(&[i]).unwrap().delivered.unwrap())
        .collect();
    assert_eq!(
        delivered,
        [true, true, false, true, true, false, true, true, false]
    );
    let got: Vec<u8> = rx_all(&mut b).iter().map(|p| p[0]).collect();
    assert_eq!(got, [0, 1, 3, 4, 6, 7]);
}

#[test]
fn random_loss_is_seeded_and_near_its_rate() {
    let run = |seed| {
        let cfg = LoopbackCfg {
            loss: Loss::Random { ppm: 100_000, seed },
            ..Default::default()
        };
        let (mut a, mut b) = pair(cfg);
        for i in 0..20_000u32 {
            a.send(&i.to_be_bytes()).unwrap();
        }
        rx_all(&mut b)
    };
    let (x, y, z) = (run(1), run(1), run(2));
    assert_eq!(x, y, "same seed, same drops");
    assert_ne!(x, z, "different seed, different drops");
    let lost = 20_000 - x.len();
    assert!(
        (1_800..2_200).contains(&lost),
        "lost {lost} of 20000 at 10%"
    );
}

#[test]
fn directions_drop_independently() {
    let cfg = LoopbackCfg {
        loss: Loss::Random {
            ppm: 500_000,
            seed: 7,
        },
        ..Default::default()
    };
    let (mut a, mut b) = pair(cfg);
    for i in 0..64u8 {
        a.send(&[i]).unwrap();
        b.send(&[i]).unwrap();
    }
    assert_ne!(rx_all(&mut a), rx_all(&mut b));
}

#[test]
fn virtual_clock_and_airtime() {
    let cfg = LoopbackCfg {
        bit_rate: 100_000,
        ..Default::default()
    };
    let (mut a, mut b) = pair(cfg);
    // 125 B at 100 kb/s is 10 ms
    let r = a.send(&[0; 125]).unwrap();
    assert_eq!((r.airtime_us, r.busy_us, r.attempts), (10_000, 10_000, 1));
    a.send(&[0; 125]).unwrap();
    let mut buf = [0u8; 255];
    assert_eq!(b.recv(&mut buf, NOW).unwrap().unwrap().t_us, 10_000);
    assert_eq!(b.recv(&mut buf, NOW).unwrap().unwrap().t_us, 20_000);
}

#[test]
fn dropped_peer_ends_the_link_after_the_queue_drains() {
    let (mut a, b) = pair(LoopbackCfg::default());
    let mut b = b;
    a.send(&[1]).unwrap();
    drop(a);
    let mut buf = [0u8; 8];
    assert_eq!(b.recv(&mut buf, NOW).unwrap().unwrap().len, 1);
    assert!(matches!(b.recv(&mut buf, NOW), Err(LinkError::Io(_))));
    assert!(matches!(b.send(&[1]), Err(LinkError::Io(_))));
}

#[test]
fn works_across_threads_as_a_boxed_link() {
    let (a, mut b) = pair(LoopbackCfg::default());
    let mut a: Box<dyn Link> = Box::new(a);
    let t = std::thread::spawn(move || {
        let mut buf = [0u8; 8];
        b.recv(&mut buf, Duration::from_secs(5))
            .unwrap()
            .unwrap()
            .len
    });
    a.send(&[1, 2, 3]).unwrap();
    assert_eq!(t.join().unwrap(), 3);
}
