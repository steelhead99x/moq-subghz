//! An in-memory link pair with deterministic drops, for tests and replays that need no radio.
//!
//! Time is virtual: a send adds its airtime to the sender's clock and never sleeps, and the
//! receiver's `t_us` is that clock when the packet left, so runs are repeatable.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use crate::{Link, LinkCaps, LinkError, LinkKind, Provenance, RxMeta, TxReport, MAX_FRAME};

/// Which packets a [`LoopbackLink`] drops on send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loss {
    None,
    /// Drop every `n`th packet (the `n`th, `2n`th, ...). `n` must be at least 1.
    Every(u32),
    /// Drop each packet with probability `ppm` per million, from a seeded generator.
    Random {
        ppm: u32,
        seed: u64,
    },
}

/// Settings for [`pair`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoopbackCfg {
    pub max_payload: usize,
    /// Bits per second, used for the airtime of a send; 0 makes every send take no time.
    pub bit_rate: u32,
    pub loss: Loss,
}

impl Default for LoopbackCfg {
    fn default() -> Self {
        LoopbackCfg {
            max_payload: MAX_FRAME,
            bit_rate: 100_000,
            loss: Loss::None,
        }
    }
}

#[derive(Default)]
struct State {
    /// Packets in flight, with the sender's clock when each left.
    q: VecDeque<(Vec<u8>, u64)>,
    /// One end was dropped: no more sends, and receives end once the queue is empty.
    closed: bool,
}

#[derive(Default)]
struct Chan {
    st: Mutex<State>,
    cv: Condvar,
}

impl Chan {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// One end of a [`pair`].
pub struct LoopbackLink {
    caps: LinkCaps,
    cfg: LoopbackCfg,
    tx: Arc<Chan>,
    rx: Arc<Chan>,
    sent: u64,
    rng: u64,
    clock_us: u64,
}

/// Two linked ends: what one sends, the other receives, minus what `cfg.loss` drops. The two
/// directions drop independently (the second end's seed is the first's, flipped in its low bit).
pub fn pair(cfg: LoopbackCfg) -> (LoopbackLink, LoopbackLink) {
    assert!(
        !matches!(cfg.loss, Loss::Every(0)),
        "Loss::Every(0) is not a period"
    );
    let (ab, ba) = (Arc::new(Chan::default()), Arc::new(Chan::default()));
    let end = |tx: &Arc<Chan>, rx: &Arc<Chan>, flip: u64| {
        let seed = match cfg.loss {
            Loss::Random { seed, .. } => seed ^ flip,
            _ => 0,
        };
        LoopbackLink {
            caps: LinkCaps {
                name: "loopback",
                kind: LinkKind::Emu,
                max_payload: cfg.max_payload,
                max_frames_per_packet: 1,
                tx: true,
                rx: true,
                rx_needs_poll: false,
                reports_rssi: false,
                reports_snr: false,
                provenance: Provenance::Modeled,
            },
            cfg,
            tx: tx.clone(),
            rx: rx.clone(),
            sent: 0,
            rng: seed,
            clock_us: 0,
        }
    };
    (end(&ab, &ba, 0), end(&ba, &ab, 1))
}

impl LoopbackLink {
    /// splitmix64
    fn next_rand(&mut self) -> u64 {
        self.rng = self.rng.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn drops(&mut self) -> bool {
        match self.cfg.loss {
            Loss::None => false,
            Loss::Every(n) => self.sent.is_multiple_of(n as u64),
            Loss::Random { ppm, .. } => self.next_rand() % 1_000_000 < ppm as u64,
        }
    }
}

impl Link for LoopbackLink {
    fn caps(&self) -> &LinkCaps {
        &self.caps
    }

    fn send(&mut self, packet: &[u8]) -> Result<TxReport, LinkError> {
        if packet.is_empty() {
            return Err(LinkError::Empty);
        }
        if packet.len() > self.caps.max_payload {
            return Err(LinkError::TooLong);
        }
        if self.tx.lock().closed {
            return Err(LinkError::Io("peer closed".into()));
        }
        self.sent += 1;
        let airtime_us = match self.cfg.bit_rate {
            0 => 0,
            r => packet.len() as u64 * 8_000_000 / r as u64,
        };
        self.clock_us += airtime_us;
        let dropped = self.drops();
        if !dropped {
            self.tx.lock().q.push_back((packet.to_vec(), self.clock_us));
            self.tx.cv.notify_one();
        }
        Ok(TxReport {
            busy_us: airtime_us,
            airtime_us,
            attempts: 1,
            delivered: Some(!dropped),
            deliver_after_us: None,
        })
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Result<Option<RxMeta>, LinkError> {
        let st = self.rx.lock();
        let (mut st, _) = self
            .rx
            .cv
            .wait_timeout_while(st, timeout, |s| s.q.is_empty() && !s.closed)
            .unwrap_or_else(|e| e.into_inner());
        let Some((p, t_us)) = st.q.pop_front() else {
            return if st.closed {
                Err(LinkError::Io("peer closed".into()))
            } else {
                Ok(None)
            };
        };
        if p.len() > buf.len() {
            return Err(LinkError::TooLong);
        }
        buf[..p.len()].copy_from_slice(&p);
        Ok(Some(RxMeta {
            len: p.len(),
            t_us,
            rssi_dbm: None,
            snr_db: None,
        }))
    }
}

impl Drop for LoopbackLink {
    fn drop(&mut self) {
        for c in [&self.tx, &self.rx] {
            c.lock().closed = true;
            c.cv.notify_all();
        }
    }
}
