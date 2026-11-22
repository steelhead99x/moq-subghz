//! [`EmuLink`]: a [`Link`] over the channel models. It carries real packets (what a sender hands
//! to `send`) through a seeded model of FSK, LoRa or HaLow and reports what the model says the
//! packet cost. Provenance is always [`Provenance::Modeled`].
//!
//! Where the delivered packets go depends on how the link was built:
//!
//! * [`EmuLink::pair`]: two ends in memory, for tests.
//! * [`EmuLink::udp_sender`] and [`EmuLink::udp_receiver`]: one UDP datagram per delivered packet,
//!   loopback addresses only.
//! * [`EmuLink::file_sender`]: `R <t> 0 0 <hex>` lines (one per wire frame) that
//!   `moq-subghz-recv --replay` reads.
//!
//! Time is virtual unless [`EmuLink::realtime`] is set: a send then adds the busy time to the
//! link clock and returns at once. With real time a send sleeps until the channel is free again
//! and datagrams are released at their delivery time (busy time plus the modeled receive
//! latency and jitter).

use crate::model::{Channel, EmuCfg, PhyKind};
use crate::pack;
use moq_subghz_link::{Link, LinkCaps, LinkError, LinkKind, Provenance, RxMeta, TxReport};
use std::collections::{BinaryHeap, VecDeque};
use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    /// Packets in flight, with the link-clock time each is delivered at.
    q: VecDeque<(Vec<u8>, u64)>,
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

    /// Send-only: every delivered packet becomes one UDP datagram to `addr` (loopback only).
    pub fn udp_sender(cfg: EmuCfg, addr: &str) -> Result<Self, LinkError> {
        let to = loopback_addr(addr)?;
        let sock = UdpSocket::bind("127.0.0.1:0").map_err(io)?;
        sock.connect(to).map_err(io)?;
        let (tx, rx) = mpsc::channel::<(Instant, Vec<u8>)>();
        let th = std::thread::spawn(move || delay_loop(rx, sock));
        EmuLink::new(cfg, true, false, Out::Udp(Some(Delay { tx, th })), In::None)
    }

    /// Receive-only: read the datagrams of a [`EmuLink::udp_sender`] on `bind` (loopback only).
    /// `cfg` must match the sender's, so the receiver knows whether packets are packed.
    pub fn udp_receiver(cfg: EmuCfg, bind: &str) -> Result<Self, LinkError> {
        let sock = UdpSocket::bind(loopback_addr(bind)?).map_err(io)?;
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
        if packet.is_empty() {
            return Err(LinkError::Empty);
        }
        if packet.len() > self.caps.max_payload {
            return Err(LinkError::TooLong);
        }
        // Check a packed packet before it goes on the (modeled) air.
        let frames = if self.caps.max_frames_per_packet > 1 {
            Some(
                pack::unpack(packet)
                    .ok_or_else(|| LinkError::Io("packet is not whole packed frames".into()))?,
            )
        } else {
            None
        };
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
                        if n > buf.len() {
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
        let mut rx = EmuLink::udp_receiver(halow(LossModel::None, true), "127.0.0.1:0").unwrap();
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
        assert!(EmuLink::udp_sender(cfg.clone(), "192.0.2.1:9").is_err());
        assert!(EmuLink::udp_receiver(cfg, "0.0.0.0:0").is_err());
    }

    #[test]
    fn udp_round_trip_on_loopback() {
        let cfg = halow(LossModel::None, true);
        let mut rx = EmuLink::udp_receiver(cfg.clone(), "127.0.0.1:0").unwrap();
        let addr = rx.local_addr().unwrap().to_string();
        let mut tx = EmuLink::udp_sender(cfg, &addr).unwrap();
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
