//! [`EmuLink`]: a [`Link`] over the channel models. It carries real packets (what a sender hands
//! to `send`) through a seeded model of FSK, LoRa or HaLow and reports what the model says the
//! packet cost. Provenance is always [`Provenance::Modeled`].
//!
//! Where the delivered packets go depends on how the link was built:
//!
//! * [`EmuLink::pair`]: two ends in memory, for tests.
//! * [`EmuLink::udp_sender`] and [`EmuLink::udp_receiver`]: one UDP datagram per delivered packet,
//!   loopback addresses only. By default the sender's port is random and the receiver takes a
//!   datagram from any local process. Give the sender a fixed source address and the receiver
//!   that address as its peer, and the kernel drops everything else. Either way this is a test
//!   channel, not a security boundary; the receiver treats a foreign datagram as noise it has to
//!   survive (an empty or oversize datagram is an error, never a panic).
//! * [`EmuLink::file_sender`]: `R <t> 0 0 <hex>` lines (one per wire frame) that
//!   `moq-subghz-recv --replay` reads.
//!
//! Time is virtual unless [`EmuLink::realtime`] is set: a send then adds the busy time to the
//! link clock and returns at once. With real time a send sleeps until the channel is free again
//! and datagrams are released at their delivery time (busy time plus the modeled receive
//! latency and jitter).

use crate::model::{Channel, EmuCfg, PhyKind};
use crate::pack;
use moq_subghz_link::{
    check_packet, Link, LinkCaps, LinkError, LinkKind, Provenance, RxMeta, TxReport,
};
use std::collections::{BinaryHeap, VecDeque};
use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Most bytes one in-memory direction holds before a send is `Busy`, so a receiver that never
/// drains cannot grow memory without bound.
const QUEUE_BYTES: usize = 16 << 20;

#[derive(Default)]
struct State {
    /// Packets in flight, with the link-clock time each is delivered at.
    q: VecDeque<(Vec<u8>, u64)>,
    /// Bytes in `q`.
    queued: usize,
    closed: bool,
}

#[derive(Default)]
struct Shared {
    st: Mutex<State>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Releases datagrams at their due time.
struct Delay {
    tx: mpsc::Sender<(Instant, Vec<u8>)>,
    th: JoinHandle<()>,
}

enum Out {
    None,
    Mem(Arc<Shared>),
    Udp(Option<Delay>),
    File(std::io::BufWriter<std::fs::File>),
}

enum In {
    None,
    Mem(Arc<Shared>),
    Udp(UdpSocket),
}

/// A modeled link. See the module docs for the ways to build one.
pub struct EmuLink {
    caps: LinkCaps,
    chan: Channel,
    out: Out,
    inp: In,
    scratch: Vec<u8>,
    realtime: bool,
    t0: Instant,
    /// Link clock: when the channel is next free, in microseconds since `t0`.
    clock_us: u64,
}

fn io(e: std::io::Error) -> LinkError {
    LinkError::Io(e.to_string())
}

fn loopback_addr(addr: &str) -> Result<SocketAddr, LinkError> {
    let a = addr
        .to_socket_addrs()
        .map_err(io)?
        .next()
        .ok_or_else(|| LinkError::Io(format!("no address for {addr}")))?;
    if !a.ip().is_loopback() {
        return Err(LinkError::Io(format!(
            "{addr}: the emulated link only uses loopback addresses (127.0.0.1)"
        )));
    }
    Ok(a)
}

impl EmuLink {
    fn new(cfg: EmuCfg, tx: bool, rx: bool, out: Out, inp: In) -> Result<Self, LinkError> {
        cfg.validate().map_err(LinkError::Io)?;
        let (name, kind) = match &cfg.kind {
            PhyKind::Fsk(_) => ("fsk-emu", LinkKind::Emu),
            PhyKind::Lora(_) => ("lora-emu", LinkKind::Emu),
            PhyKind::Halow(_) => ("halow-emu", LinkKind::Emu),
        };
        let caps = LinkCaps {
            name,
            kind,
            max_payload: cfg.mtu(),
            max_frames_per_packet: cfg.max_frames_per_packet(),
            tx,
            rx,
            rx_needs_poll: false,
            reports_rssi: false,
            reports_snr: false,
            provenance: Provenance::Modeled,
        };
        Ok(EmuLink {
            caps,
            chan: Channel::new(cfg),
            out,
            inp,
            scratch: Vec::new(),
            realtime: false,
            t0: Instant::now(),
            clock_us: 0,
        })
    }

    /// Two ends in memory: what one sends, the other receives, minus what the model loses. The
    /// two directions use independent noise (the second end's seed is the first's with its low
    /// bit flipped).
    pub fn pair(cfg: EmuCfg) -> Result<(EmuLink, EmuLink), LinkError> {
        let (ab, ba) = (Arc::new(Shared::default()), Arc::new(Shared::default()));
        let mut cfg2 = cfg.clone();
        cfg2.seed ^= 1;
        let a = EmuLink::new(cfg, true, true, Out::Mem(ab.clone()), In::Mem(ba.clone()))?;
        let b = EmuLink::new(cfg2, true, true, Out::Mem(ba), In::Mem(ab))?;
        Ok((a, b))
    }

    /// Send-only: every delivered packet becomes one UDP datagram to `addr` (loopback only),
    /// sent from `from` (loopback only, so a receiver can name its peer) or a random port.
    pub fn udp_sender(cfg: EmuCfg, addr: &str, from: Option<&str>) -> Result<Self, LinkError> {
        let to = loopback_addr(addr)?;
        let from = from.map(loopback_addr).transpose()?;
        let sock = UdpSocket::bind(from.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0))))
            .map_err(io)?;
        sock.connect(to).map_err(io)?;
        let (tx, rx) = mpsc::channel::<(Instant, Vec<u8>)>();
        let th = std::thread::spawn(move || delay_loop(rx, sock));
        EmuLink::new(cfg, true, false, Out::Udp(Some(Delay { tx, th })), In::None)
    }

    /// Receive-only: read the datagrams of a [`EmuLink::udp_sender`] on `bind` (loopback only).
    /// `cfg` must match the sender's, so the receiver knows whether packets are packed. With
    /// `peer` (loopback only) it takes datagrams from that address and no other; without it,
    /// from any local process.
    pub fn udp_receiver(cfg: EmuCfg, bind: &str, peer: Option<&str>) -> Result<Self, LinkError> {
        let sock = UdpSocket::bind(loopback_addr(bind)?).map_err(io)?;
        if let Some(p) = peer {
            connect_and_drain(&sock, loopback_addr(p)?).map_err(io)?;
        }
        EmuLink::new(cfg, false, true, Out::None, In::Udp(sock))
    }

    /// Send-only: write each delivered wire frame as an `R` line to `path`.
    pub fn file_sender(cfg: EmuCfg, path: &str) -> Result<Self, LinkError> {
        let f = std::fs::File::create(path).map_err(io)?;
        EmuLink::new(
            cfg,
            true,
            false,
            Out::File(std::io::BufWriter::new(f)),
            In::None,
        )
    }

    /// Pace sends at the channel's busy time and release datagrams at their delivery time.
    /// Without it time is virtual and a send returns at once.
    pub fn realtime(mut self, on: bool) -> Self {
        self.realtime = on;
        self
    }

    /// The local address of a UDP receiver (useful after binding port 0).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        match &self.inp {
            In::Udp(s) => s.local_addr().ok(),
            _ => None,
        }
    }

    /// The model settings this link was built with.
    pub fn cfg(&self) -> &EmuCfg {
        &self.chan.cfg
    }
}

/// `connect` filters the datagrams that arrive after it, but Linux keeps those that were already
/// queued between `bind` and `connect`, from any sender. Throw them away, so only the peer's
/// datagrams are ever delivered.
fn connect_and_drain(sock: &UdpSocket, peer: SocketAddr) -> std::io::Result<()> {
    sock.connect(peer)?;
    sock.set_nonblocking(true)?;
    let mut sink = [0u8; 2048];
    let drained = loop {
        match sock.recv(&mut sink) {
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break Ok(()),
            // A connected socket can report an earlier ICMP error once; it is not ours to act on.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => continue,
            Err(e) => break Err(e),
        }
    };
    sock.set_nonblocking(false)?;
    drained
}

fn delay_loop(rx: mpsc::Receiver<(Instant, Vec<u8>)>, sock: UdpSocket) {
    type Item = std::cmp::Reverse<(Instant, u64, Vec<u8>)>;
    let mut heap: BinaryHeap<Item> = BinaryHeap::new();
    let mut n = 0u64;
    loop {
        let wait = heap
            .peek()
            .map(|std::cmp::Reverse((d, _, _))| d.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_millis(50));
        match rx.recv_timeout(wait) {
            Ok((d, b)) => {
                n += 1;
                heap.push(std::cmp::Reverse((d, n, b)));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // flush what is still due, in order, then stop
                while let Some(std::cmp::Reverse((d, _, b))) = heap.pop() {
                    std::thread::sleep(d.saturating_duration_since(Instant::now()));
                    let _ = sock.send(&b);
                }
                return;
            }
        }
        while heap
            .peek()
            .is_some_and(|std::cmp::Reverse((d, _, _))| *d <= Instant::now())
        {
            if let Some(std::cmp::Reverse((_, _, b))) = heap.pop() {
                let _ = sock.send(&b);
            }
        }
    }
}

impl Link for EmuLink {
    fn caps(&self) -> &LinkCaps {
        &self.caps
    }

    fn send(&mut self, packet: &[u8]) -> Result<TxReport, LinkError> {
        if !self.caps.tx {
            return Err(LinkError::Unsupported);
        }
        check_packet(self.caps.max_payload, packet)?;
        // Check a packed packet before it goes on the (modeled) air.
        let frames = if self.caps.max_frames_per_packet > 1 {
            Some(
                pack::unpack(packet)
                    .ok_or_else(|| LinkError::Io("packet is not whole packed frames".into()))?,
            )
        } else {
            None
        };
        if let Out::Mem(ch) = &self.out {
            let st = ch.lock();
            if st.closed {
                return Err(LinkError::Io("peer closed".into()));
            }
            if st.queued + packet.len() > QUEUE_BYTES {
                return Err(LinkError::Busy);
            }
        }
        let o = self.chan.transmit(packet.len());
        let start_us = if self.realtime {
            (self.t0.elapsed().as_micros() as u64).max(self.clock_us)
        } else {
            self.clock_us
        };
        self.clock_us = start_us + o.busy_us;
        if self.realtime {
            let free_at = self.t0 + Duration::from_micros(self.clock_us);
            let now = Instant::now();
            if free_at > now {
                std::thread::sleep(free_at - now);
            }
        }
        if o.delivered {
            let due_us = start_us + o.deliver_after_us;
            match &mut self.out {
                Out::None => return Err(LinkError::Unsupported),
                Out::Mem(ch) => {
                    let mut st = ch.lock();
                    if st.closed {
                        return Err(LinkError::Io("peer closed".into()));
                    }
                    st.queued += packet.len();
                    st.q.push_back((packet.to_vec(), due_us));
                    drop(st);
                    ch.cv.notify_one();
                }
                Out::Udp(d) => {
                    let d = d
                        .as_ref()
                        .ok_or_else(|| LinkError::Io("link is idle".into()))?;
                    let due = if self.realtime {
                        self.t0 + Duration::from_micros(due_us)
                    } else {
                        Instant::now()
                    };
                    d.tx.send((due, packet.to_vec()))
                        .map_err(|_| LinkError::Io("delay thread stopped".into()))?;
                }
                Out::File(f) => {
                    let one = [packet.to_vec()];
                    for fr in frames.as_deref().unwrap_or(&one) {
                        let hex: String = fr.iter().map(|b| format!("{b:02x}")).collect();
                        writeln!(f, "R {due_us} 0 0 {hex}").map_err(io)?;
                    }
                }
            }
        }
        Ok(TxReport {
            busy_us: o.busy_us,
            airtime_us: o.airtime_us,
            attempts: o.attempts,
            delivered: Some(o.delivered),
            acked: o.acked,
            deliver_after_us: Some(o.deliver_after_us),
        })
    }

    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Result<Option<RxMeta>, LinkError> {
        match &self.inp {
            In::None => Err(LinkError::Unsupported),
            In::Mem(ch) => {
                let st = ch.lock();
                let (mut st, _) = ch
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
                st.queued -= p.len();
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
            In::Udp(sock) => {
                // a zero timeout is an error for a socket; 1 ms is the shortest wait
                sock.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))
                    .map_err(io)?;
                if self.scratch.is_empty() {
                    self.scratch = vec![0u8; 65536];
                }
                match sock.recv(&mut self.scratch) {
                    Ok(n) => {
                        // anything on the port may be foreign: refuse what no link sends
                        if n == 0 {
                            return Err(LinkError::Empty);
                        }
                        if n > self.caps.max_payload || n > buf.len() {
                            return Err(LinkError::TooLong);
                        }
                        buf[..n].copy_from_slice(&self.scratch[..n]);
                        Ok(Some(RxMeta {
                            len: n,
                            t_us: self.t0.elapsed().as_micros() as u64,
                            rssi_dbm: None,
                            snr_db: None,
                        }))
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                    {
                        Ok(None)
                    }
                    Err(e) => Err(io(e)),
                }
            }
        }
    }

    /// Flush the file, release the datagrams still in the delay queue, and stop. The model never
    /// drives a radio, so there is nothing else to put in standby.
    fn idle(&mut self) -> Result<(), LinkError> {
        match &mut self.out {
            Out::File(f) => f.flush().map_err(io)?,
            Out::Udp(d) => {
                if let Some(Delay { tx, th }) = d.take() {
                    drop(tx);
                    let _ = th.join();
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl Drop for EmuLink {
    fn drop(&mut self) {
        let _ = self.idle();
        for sh in [&self.out_mem(), &self.in_mem()].into_iter().flatten() {
            sh.lock().closed = true;
            sh.cv.notify_all();
        }
    }
}

impl EmuLink {
    fn out_mem(&self) -> Option<Arc<Shared>> {
        match &self.out {
            Out::Mem(s) => Some(s.clone()),
            _ => None,
        }
    }
    fn in_mem(&self) -> Option<Arc<Shared>> {
        match &self.inp {
            In::Mem(s) => Some(s.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loss::LossModel;
    use crate::model::{FskCfg, HalowCfg};

    fn halow(loss: LossModel, ack: bool) -> EmuCfg {
        let mut c = EmuCfg::new(PhyKind::Halow(HalowCfg {
            ack,
            ..HalowCfg::default()
        }));
        c.loss = loss;
        c
    }

    #[test]
    fn caps_say_modeled_and_what_the_model_allows() {
        let (a, _b) = EmuLink::pair(halow(LossModel::None, true)).unwrap();
        let c = a.caps();
        assert_eq!(c.provenance, Provenance::Modeled);
        assert_eq!(c.kind, LinkKind::Emu);
        assert_eq!((c.max_payload, c.max_frames_per_packet), (1472, 5));
        let (f, _g) = EmuLink::pair(EmuCfg::new(PhyKind::Fsk(FskCfg::default()))).unwrap();
        assert_eq!(
            (f.caps().max_payload, f.caps().max_frames_per_packet),
            (255, 1)
        );
    }

    #[test]
    fn passes_the_link_conformance_checks() {
        use moq_subghz_link::conformance::{round_trip, tx};
        let fsk = || EmuCfg::new(PhyKind::Fsk(FskCfg::default()));
        let (mut a, mut b) = EmuLink::pair(fsk()).unwrap();
        round_trip(&mut a, &mut b, &[1, 2, 3]);
        let (mut a, mut b) = EmuLink::pair(halow(LossModel::None, true)).unwrap();
        round_trip(&mut a, &mut b, &pack::pack(&[&[7u8; 40][..]]).unwrap());
        let (mut a, _b) = EmuLink::pair(halow(LossModel::None, true)).unwrap();
        tx(&mut a, &pack::pack(&[&[7u8; 40][..]]).unwrap());
        let mut f = EmuLink::file_sender(fsk(), "/dev/null").unwrap();
        tx(&mut f, &[9, 9]);
        let mut rx = EmuLink::udp_receiver(fsk(), "127.0.0.1:0", None).unwrap();
        let addr = rx.local_addr().unwrap().to_string();
        let mut u = EmuLink::udp_sender(fsk(), &addr, None).unwrap();
        round_trip(&mut u, &mut rx, &[4, 5, 6]);
        tx(&mut u, &[1]);
    }

    #[test]
    fn udp_receiver_survives_foreign_datagrams() {
        let mut rx = EmuLink::udp_receiver(
            EmuCfg::new(PhyKind::Fsk(FskCfg::default())),
            "127.0.0.1:0",
            None,
        )
        .unwrap();
        let to = rx.local_addr().unwrap();
        let s = UdpSocket::bind("127.0.0.1:0").unwrap();
        s.send_to(&[], to).unwrap();
        s.send_to(&[1u8; 300], to).unwrap();
        s.send_to(&[2u8; 5], to).unwrap();
        let mut buf = [0u8; 512];
        let t = Duration::from_secs(2);
        assert_eq!(rx.recv(&mut buf, t), Err(LinkError::Empty));
        assert_eq!(rx.recv(&mut buf, t), Err(LinkError::TooLong));
        assert_eq!(rx.recv(&mut buf, t).unwrap().unwrap().len, 5);
    }

    #[test]
    fn datagrams_queued_before_connect_are_dropped() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let to = rx.local_addr().unwrap();
        let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
        stranger.send_to(&[9u8; 5], to).unwrap();
        stranger.send_to(&[8u8; 3000], to).unwrap();
        let peer = UdpSocket::bind("127.0.0.1:0").unwrap();
        connect_and_drain(&rx, peer.local_addr().unwrap()).unwrap();
        rx.set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut b = [0u8; 16];
        assert!(rx.recv(&mut b).is_err(), "the queue is empty after connect");
        peer.send_to(&[1, 2], to).unwrap();
        assert_eq!(rx.recv(&mut b).unwrap(), 2);
    }

    #[test]
    fn a_receiver_with_a_peer_ignores_everyone_else() {
        use moq_subghz_link::conformance::round_trip;
        let cfg = || EmuCfg::new(PhyKind::Fsk(FskCfg::default()));
        // pick a free port for the sender, then release it
        let from = UdpSocket::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .to_string();
        let mut rx = EmuLink::udp_receiver(cfg(), "127.0.0.1:0", Some(&from)).unwrap();
        let to = rx.local_addr().unwrap();
        let stranger = UdpSocket::bind("127.0.0.1:0").unwrap();
        stranger.send_to(&[9u8; 5], to).unwrap();
        let mut tx = EmuLink::udp_sender(cfg(), &to.to_string(), Some(&from)).unwrap();
        // the stranger's datagram never arrives, the peer's do (and `round_trip` checks the
        // queue is empty afterwards)
        round_trip(&mut tx, &mut rx, &[1, 2, 3]);
    }

    #[test]
    fn an_undrained_pair_makes_send_busy() {
        let (mut a, mut b) = EmuLink::pair(EmuCfg::new(PhyKind::Fsk(FskCfg::default()))).unwrap();
        let mut sent = 0usize;
        while a.send(&[0; 255]).is_ok() {
            sent += 1;
            assert!(sent < 1 << 20);
        }
        assert_eq!(a.send(&[0; 255]), Err(LinkError::Busy));
        assert_eq!(sent, (16 << 20) / 255);
        b.recv(&mut [0u8; 255], Duration::ZERO).unwrap().unwrap();
        a.send(&[0; 255]).unwrap();
    }

    #[test]
    fn pair_delivers_bytes_and_reports_cost() {
        let (mut a, mut b) = EmuLink::pair(EmuCfg::new(PhyKind::Fsk(FskCfg::default()))).unwrap();
        let pkt: Vec<u8> = (0..255u32).map(|i| i as u8).collect();
        let r = a.send(&pkt).unwrap();
        assert_eq!(r.attempts, 1);
        assert_eq!(r.delivered, Some(true));
        assert_eq!(r.busy_us, FskCfg::default().airtime_us(255));
        assert_eq!(r.airtime_us, r.busy_us);
        let mut buf = [0u8; 255];
        let m = b
            .recv(&mut buf, Duration::from_millis(100))
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..m.len], &pkt[..]);
        assert_eq!(m.t_us, r.busy_us);
        assert_eq!(b.recv(&mut buf, Duration::from_millis(5)), Ok(None));
    }

    #[test]
    fn refuses_bad_packets_and_wrong_direction() {
        let (mut a, _b) = EmuLink::pair(halow(LossModel::None, true)).unwrap();
        assert_eq!(a.send(&[]), Err(LinkError::Empty));
        assert_eq!(a.send(&vec![1u8; 1473]), Err(LinkError::TooLong));
        // 3 bytes are not whole packed records: [len 9][1 2] is truncated
        assert!(matches!(a.send(&[9, 1, 2]), Err(LinkError::Io(_))));
        let mut rx =
            EmuLink::udp_receiver(halow(LossModel::None, true), "127.0.0.1:0", None).unwrap();
        assert_eq!(rx.send(&[1]), Err(LinkError::Unsupported));
        let mut tx =
            EmuLink::file_sender(EmuCfg::new(PhyKind::Fsk(FskCfg::default())), "/dev/null")
                .unwrap();
        assert_eq!(
            tx.recv(&mut [0u8; 8], Duration::ZERO),
            Err(LinkError::Unsupported)
        );
        let bad = EmuCfg::new(PhyKind::Halow(HalowCfg {
            mtu: 10,
            ..HalowCfg::default()
        }));
        assert!(EmuLink::pair(bad).is_err());
    }

    #[test]
    fn udp_refuses_non_loopback_addresses() {
        let cfg = EmuCfg::new(PhyKind::Fsk(FskCfg::default()));
        assert!(EmuLink::udp_sender(cfg.clone(), "192.0.2.1:9", None).is_err());
        assert!(EmuLink::udp_sender(cfg.clone(), "127.0.0.1:9", Some("192.0.2.1:0")).is_err());
        assert!(EmuLink::udp_receiver(cfg.clone(), "0.0.0.0:0", None).is_err());
        assert!(EmuLink::udp_receiver(cfg, "127.0.0.1:0", Some("192.0.2.1:9")).is_err());
    }

    #[test]
    fn udp_round_trip_on_loopback() {
        let cfg = halow(LossModel::None, true);
        let mut rx = EmuLink::udp_receiver(cfg.clone(), "127.0.0.1:0", None).unwrap();
        let addr = rx.local_addr().unwrap().to_string();
        let mut tx = EmuLink::udp_sender(cfg, &addr, None).unwrap();
        let pkt = pack::pack(&[&[7u8; 255][..], &[8u8; 40][..]]).unwrap();
        tx.send(&pkt).unwrap();
        tx.idle().unwrap();
        let mut buf = vec![0u8; 1472];
        let m = rx.recv(&mut buf, Duration::from_secs(2)).unwrap().unwrap();
        assert_eq!(&buf[..m.len], &pkt[..]);
        assert_eq!(rx.recv(&mut buf, Duration::from_millis(5)), Ok(None));
    }

    #[test]
    fn same_seed_gives_byte_equal_output_files() {
        let dir = std::env::temp_dir().join(format!("moq-emu-det-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |name: &str, seed: u64| {
            let mut cfg = halow(LossModel::Iid(0.3), false);
            cfg.seed = seed;
            let path = dir.join(name);
            let mut l = EmuLink::file_sender(cfg, path.to_str().unwrap()).unwrap();
            let mut reports = vec![];
            for i in 0..200u8 {
                let a = vec![i; 255];
                let b = vec![i ^ 0x55; 100];
                reports.push(l.send(&pack::pack(&[&a, &b]).unwrap()).unwrap());
            }
            l.idle().unwrap();
            (std::fs::read(&path).unwrap(), reports)
        };
        let (f1, r1) = run("a", 5);
        let (f2, r2) = run("b", 5);
        let (f3, _) = run("c", 6);
        assert_eq!(f1, f2);
        assert_eq!(r1, r2);
        assert_ne!(f1, f3);
        // one R line per wire frame, not per packet
        let lines = f1.split(|b| *b == b'\n').filter(|l| !l.is_empty()).count();
        assert_eq!(lines % 2, 0);
        assert!(lines > 100 && lines < 400);
        std::fs::remove_file(dir.join("a")).unwrap();
        std::fs::remove_file(dir.join("b")).unwrap();
        std::fs::remove_file(dir.join("c")).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }

    #[test]
    fn virtual_clock_adds_busy_time() {
        let mut c = EmuCfg::new(PhyKind::Halow(HalowCfg::default()));
        c.expected = true;
        let (mut a, mut b) = EmuLink::pair(c).unwrap();
        let p = pack::pack(&[&[1u8; 255][..]]).unwrap();
        let r1 = a.send(&p).unwrap();
        let r2 = a.send(&p).unwrap();
        let mut buf = [0u8; 1472];
        let t1 = b
            .recv(&mut buf, Duration::from_millis(50))
            .unwrap()
            .unwrap()
            .t_us;
        let t2 = b
            .recv(&mut buf, Duration::from_millis(50))
            .unwrap()
            .unwrap()
            .t_us;
        assert_eq!(t2 - t1, r1.busy_us);
        assert_eq!(t2, r1.busy_us + r2.deliver_after_us.unwrap());
    }
}
