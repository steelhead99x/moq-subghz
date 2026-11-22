//! Checks every [`Link`] backend must pass. Each backend's tests call these on a link they built,
//! so the contract in the trait docs is tested the same way for the loopback, the emulation, the
//! CatWAN stick and the file sink, not once per copy of the same three lines.
//!
//! Both functions panic with a message on a violation, like an assertion.

use crate::{Link, LinkError};
use std::time::Duration;

/// The transmit side: an empty or oversize packet is refused and nothing goes out, a valid packet
/// `ok` is sent and reported sanely, and `idle` succeeds. `ok` must be a packet this link accepts.
/// Do not send on the link afterwards: `idle` may close its output.
pub fn tx(link: &mut dyn Link, ok: &[u8]) {
    let max = link.caps().max_payload;
    assert!(link.caps().tx, "tx() needs a transmit link");
    assert!(
        !ok.is_empty() && ok.len() <= max,
        "`ok` is not a valid packet"
    );
    assert_eq!(link.send(&[]), Err(LinkError::Empty), "empty packet");
    assert_eq!(
        link.send(&vec![0u8; max + 1]),
        Err(LinkError::TooLong),
        "one byte over max_payload"
    );
    assert_eq!(
        link.send(&vec![0u8; max.saturating_mul(2).max(max + 1024)]),
        Err(LinkError::TooLong),
        "far over max_payload"
    );
    let r = link.send(ok).expect("a valid packet is sent");
    assert!(r.attempts >= 1, "a send made at least one attempt");
    assert!(
        r.busy_us >= r.airtime_us,
        "the channel is busy at least as long as the packet is on air"
    );
    if r.acked == Some(true) {
        assert_eq!(r.delivered, Some(true), "an ACKed packet was delivered");
    }
    assert_eq!(link.take_tx_errors(), 0, "no transmit errors so far");
    link.idle().expect("idle succeeds on a healthy link");
}

/// The receive side, with `tx` its lossless peer: a packet sent arrives intact, `recv` with the
/// longest possible timeout returns it instead of panicking, a too-small buffer is `TooLong` and
/// not a truncation, and a short timeout with nothing queued is `Ok(None)`. `pkt` must be a valid
/// packet of at least 2 bytes.
pub fn round_trip(tx: &mut dyn Link, rx: &mut dyn Link, pkt: &[u8]) {
    assert!(pkt.len() >= 2, "`pkt` needs 2 bytes or more");
    let mut buf = vec![0u8; rx.caps().max_payload.max(pkt.len())];
    tx.send(pkt).expect("send");
    let m = rx
        .recv(&mut buf, Duration::MAX)
        .expect("recv with Duration::MAX")
        .expect("the packet arrives");
    assert_eq!(&buf[..m.len], pkt, "bytes arrive intact");
    tx.send(pkt).expect("send");
    let mut small = vec![0u8; pkt.len() - 1];
    assert_eq!(
        rx.recv(&mut small, Duration::MAX),
        Err(LinkError::TooLong),
        "a short buffer is an error, not a truncation"
    );
    assert_eq!(
        rx.recv(&mut buf, Duration::from_millis(5)),
        Ok(None),
        "nothing queued is a timeout"
    );
}
