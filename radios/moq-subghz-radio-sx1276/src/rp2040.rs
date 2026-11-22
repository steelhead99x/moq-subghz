//! The RP2040-based device as a transmit link: one `Q <hex>` line per packet. The stick stops
//! reading USB while its 12-frame TX queue is full, so a write blocks, which is the backpressure.
//! Moved from `moq-subghz-send` as it was; only the `Link` wrapper at the end is new.

use anyhow::{bail, Context, Result};
use moq_subghz_licence::TxPermit;
use moq_subghz_link::{
    check_packet, Link, LinkCaps, LinkError, LinkKind, Provenance, RxMeta, TxReport,
};
use moq_subghz_wire::{read_line_limited, Line, MAX_SERIAL_LINE};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

fn open_serial(path: &str) -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(path)
        .with_context(|| format!("open {path}"))?;
    // SAFETY: plain termios calls on a valid fd.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(f.as_raw_fd(), &mut t) != 0 {
            bail!("tcgetattr failed");
        }
        libc::cfmakeraw(&mut t);
        t.c_cflag |= libc::CLOCAL | libc::CREAD;
        libc::tcsetattr(f.as_raw_fd(), libc::TCSANOW, &t);
        libc::tcflush(f.as_raw_fd(), libc::TCIOFLUSH);
    }
    Ok(f)
}

/// How long one write to the RP2040-based device may wait for the stick to take it. The stick stops
/// reading USB only while its 12-frame queue is full, and a slot frees once a frame is
/// on air, so allow three frame times, and at least a second for fast profiles.
fn rp2040_write_timeout(frame_toa_us: u64) -> Duration {
    Duration::from_micros(3 * frame_toa_us).max(Duration::from_secs(1))
}

/// Timeout for the stick's setup commands, before the profile's frame time matters.
const RP2040_SETUP_TIMEOUT: Duration = Duration::from_secs(1);

/// One command line for the stick. After a timed-out write the stick may hold the
/// front of a line; the stick runs every line ending in a newline, and would send a
/// truncated `Q` with even-length hex as a short frame. `resync` prefixes `x\n`, so that
/// leftover fails the stick's hex check and is dropped (a bare `x` line gets an `ERR`).
fn rp2040_line(resync: bool, body: &[u8]) -> Vec<u8> {
    let mut l = Vec::with_capacity(3 + body.len());
    if resync {
        l.extend_from_slice(b"x\n");
    }
    l.extend_from_slice(body);
    l.push(b'\n');
    l
}

/// Write all of `buf` to a non-blocking `f` within `timeout`, or fail: a stick that
/// stops reading must fail the write, not hang the sender past `--secs`.
fn write_deadline(f: &mut std::fs::File, mut buf: &[u8], timeout: Duration) -> Result<()> {
    use std::os::fd::AsRawFd;
    let deadline = Instant::now() + timeout;
    while !buf.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("RP2040-based device stopped reading: write timed out after {timeout:?}");
        }
        let mut pfd = libc::pollfd {
            fd: f.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        let ms = left.as_millis().clamp(1, i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd for the duration of the call.
        if unsafe { libc::poll(&mut pfd, 1, ms) } < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        match f.write(buf) {
            Ok(0) => bail!("RP2040-based device write returned 0"),
            Ok(n) => buf = &buf[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

/// Configure the RP2040-based device for FSK TX; returns the write half and a counter of the TX
/// errors the stick reports. A reader thread drains the stick's replies (B/D/ERR lines)
/// so its output buffer never fills.
fn open_rp2040(
    port: &str,
    freq_hz: u32,
    p: &moq_subghz_wire::FskProfile,
    power: i8,
) -> Result<(std::fs::File, Arc<AtomicU64>, Arc<AtomicU64>)> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = open_serial(port)?;
    // Writes go through their own non-blocking open file, so the deadline doesn't make
    // the reader thread's blocking reads fail (a cloned fd would share O_NONBLOCK).
    let mut w = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(port)
        .with_context(|| format!("open {port} for writing"))?;
    let (tx, rx) = mpsc::channel::<String>();
    let errs = Arc::new(AtomicU64::new(0));
    let errs_rd = errs.clone();
    let drained = Arc::new(AtomicU64::new(0));
    let drained_rd = drained.clone();
    std::thread::spawn(move || {
        let mut r = std::io::BufReader::new(f);
        while let Ok(Some(line)) = read_line_limited(&mut r, MAX_SERIAL_LINE) {
            let Line::Data(buf) = line else {
                continue; // oversize reply is not a config response
            };
            let l = String::from_utf8_lossy(&buf);
            if l.starts_with("ERR tx") {
                errs_rd.fetch_add(1, Ordering::Relaxed);
            }
            if let Some(n) = parse_done(&l) {
                drained_rd.fetch_max(n, Ordering::Relaxed);
            }
            if tx.send(l.trim().to_string()).is_err() {
                // keep draining after setup
            }
        }
    });
    let mut cmd = |c: &str, want: &str| -> Result<String> {
        write_deadline(
            &mut w,
            &rp2040_line(false, c.as_bytes()),
            RP2040_SETUP_TIMEOUT,
        )?;
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            if let Ok(l) = rx.recv_timeout(Duration::from_millis(100)) {
                if l.starts_with(want) {
                    return Ok(l);
                }
            }
        }
        bail!("RP2040-based device: no {want:?} reply to {c:?}")
    };
    cmd("S", "OK S")?;
    let r = cmd(
        &format!(
            "F {freq_hz} {} {} {power} {} 1 {}",
            p.bitrate, p.fdev, p.preamble, p.unchecked as u8
        ),
        "OK F",
    )?;
    if !r.ends_with("None") {
        bail!("RP2040-based device config error: {r}");
    }
    eprintln!("RP2040-based device TX ready: {r}");
    Ok((w, errs, drained))
}

/// The count in the stick's `D <t_us> <n> <burst_us>` line: frames it has put on air since its
/// last `F`. The stick prints it when its queue empties after a frame left the air.
fn parse_done(line: &str) -> Option<u64> {
    let mut it = line.split_whitespace();
    (it.next()? == "D").then_some(())?;
    it.next()?.parse::<u64>().ok()?;
    it.next()?.parse().ok()
}

/// The stick's queue holds 12 frames and one more is on air, so no more than this can be
/// waiting when `idle` starts.
const STICK_QUEUE_FRAMES: u64 = 13;

/// How long dropping a [`Rp2040Tx`] waits to tell the stick to stop. Short: a drop may run while
/// unwinding from a panic, and a stick that stopped reading will not be reached anyway.
const DROP_STOP_TIMEOUT: Duration = Duration::from_millis(500);

/// On-air time of a `len` byte frame: preamble, 2 sync bytes, a length byte, the payload and
/// 2 CRC bytes at the FSK bit rate. The same formula the sender uses for its pacing.
fn fsk_airtime_us(p: &moq_subghz_wire::FskProfile, len: usize) -> u64 {
    (u64::from(p.preamble) + 2 + 1 + len as u64 + 2) * 8_000_000 / u64::from(p.bitrate)
}

/// A RP2040-based device configured for FSK transmit.
pub struct Rp2040Tx {
    w: std::fs::File,
    /// The stick's `ERR tx` / `ERR txtimeout` lines, counted by a reader thread.
    errs: Arc<AtomicU64>,
    /// How many of `errs` were already reported by `take_tx_errors`.
    seen: u64,
    /// How long one write may wait for the stick (see [`rp2040_write_timeout`]).
    timeout: Duration,
    /// A write timed out, possibly mid-line; see [`rp2040_line`].
    resync: bool,
    /// The stick was told to stop (`idle` succeeded), so a drop has nothing left to do.
    stopped: bool,
    /// `Q` lines written to the stick so far.
    queued: u64,
    /// The stick's own count of frames it put on air (its `D` lines).
    drained: Arc<AtomicU64>,
    /// Longest `idle` waits for the queue to drain, per frame still waiting.
    frame_toa_us: u64,
    /// Frames `idle` could not confirm were sent before it stopped the stick.
    unconfirmed: u64,
    /// The profile the stick was configured with, for the airtime of each frame.
    prof: moq_subghz_wire::FskProfile,
    caps: LinkCaps,
}

impl Rp2040Tx {
    /// Open `port`, put the stick in FSK transmit at `freq_hz` and `power` dBm.
    /// `permit` is the gate's answer for this run; `freq_hz` must lie inside the span it checked.
    /// `frame_toa_us` is the airtime of a 255 byte frame, which sets the write timeout.
    pub fn open(
        permit: &TxPermit,
        port: &str,
        freq_hz: u32,
        p: &moq_subghz_wire::FskProfile,
        power: i8,
        frame_toa_us: u64,
    ) -> Result<Self> {
        crate::require_covered(|hz| permit.covers_hz(hz), freq_hz)?;
        let (w, errs, drained) = open_rp2040(port, freq_hz, p, power)?;
        Ok(Self::new(w, errs, drained, *p, frame_toa_us))
    }

    fn new(
        w: std::fs::File,
        errs: Arc<AtomicU64>,
        drained: Arc<AtomicU64>,
        prof: moq_subghz_wire::FskProfile,
        frame_toa_us: u64,
    ) -> Self {
        Rp2040Tx {
            w,
            errs,
            seen: 0,
            timeout: rp2040_write_timeout(frame_toa_us),
            resync: false,
            stopped: false,
            queued: 0,
            drained,
            frame_toa_us,
            unconfirmed: 0,
            prof,
            caps: LinkCaps {
                name: "rp2040-fsk",
                kind: LinkKind::Serial,
                max_payload: moq_subghz_wire::MAX_FRAME,
                max_frames_per_packet: 1,
                tx: true,
                rx: false,
                rx_needs_poll: false,
                reports_rssi: false,
                reports_snr: false,
                provenance: Provenance::Measured,
            },
        }
    }
}

impl Link for Rp2040Tx {
    fn caps(&self) -> &LinkCaps {
        &self.caps
    }

    /// The stick queues the frame and does not say when it goes on air, so `airtime_us` and
    /// `busy_us` are computed from the profile (one frame, no queue wait), not observed.
    fn send(&mut self, fr: &[u8]) -> Result<TxReport, LinkError> {
        check_packet(self.caps.max_payload, fr)?;
        let mut body = Vec::with_capacity(2 + 2 * fr.len());
        body.extend_from_slice(b"Q ");
        for b in fr {
            body.extend_from_slice(format!("{b:02x}").as_bytes());
        }
        let r = write_deadline(&mut self.w, &rp2040_line(self.resync, &body), self.timeout);
        self.resync = r.is_err();
        r.map_err(|e| LinkError::Io(e.to_string()))?;
        self.stopped = false;
        self.queued += 1;
        // whether the frame arrives is unknown here, so `delivered` and `acked` stay `None`
        let air = fsk_airtime_us(&self.prof, fr.len());
        Ok(TxReport {
            busy_us: air,
            airtime_us: air,
            attempts: 1,
            ..TxReport::default()
        })
    }

    fn recv(&mut self, _: &mut [u8], _: Duration) -> Result<Option<RxMeta>, LinkError> {
        Err(LinkError::Unsupported)
    }

    /// Wait for the stick to say its queue is empty (its `D` line), then stop it. The wait is
    /// bounded by the frames still waiting times their airtime; frames the stick has not
    /// confirmed by then are dropped by the stop and counted in [`Link::unconfirmed_frames`],
    /// never reported as sent. A failure to stop it is an error, and is also printed: the caller
    /// must not report a clean run while the stick may still transmit.
    fn idle(&mut self) -> Result<(), LinkError> {
        let pending = |s: &Self| {
            s.queued.saturating_sub(
                s.drained
                    .load(Ordering::Relaxed)
                    .saturating_add(s.errs.load(Ordering::Relaxed)),
            )
        };
        let waiting = pending(self).min(STICK_QUEUE_FRAMES);
        let limit = Duration::from_micros((waiting + 1) * self.frame_toa_us * 2)
            + Duration::from_millis(500);
        let t0 = Instant::now();
        while pending(self) > 0 && t0.elapsed() < limit {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.unconfirmed = pending(self);
        if self.unconfirmed > 0 {
            eprintln!(
                "RP2040-based device: {} queued frame(s) not confirmed sent before the stop; they are not counted as sent",
                self.unconfirmed
            );
        }
        match write_deadline(&mut self.w, &rp2040_line(self.resync, b"S"), self.timeout) {
            Ok(()) => {
                self.resync = false;
                self.stopped = true;
                Ok(())
            }
            Err(e) => {
                self.resync = true;
                eprintln!(
                    "stopping the RP2040-based device failed, it may still be transmitting: {e}"
                );
                Err(LinkError::Io(format!(
                    "stopping the RP2040-based device failed: {e}"
                )))
            }
        }
    }

    fn unconfirmed_frames(&mut self) -> u64 {
        std::mem::take(&mut self.unconfirmed)
    }

    /// TX errors the RP2040-based device reported since the last call. They arrive asynchronously, after
    /// the packet they belong to was queued.
    fn take_tx_errors(&mut self) -> u64 {
        let now = self.errs.load(Ordering::Relaxed);
        let n = now - self.seen;
        self.seen = now;
        n
    }
}

/// Best effort: tell the stick to stop (`S`, which only stops; nothing here can transmit) when
/// `idle` did not. Never panics and never waits long; a failure has nowhere to go.
impl Drop for Rp2040Tx {
    fn drop(&mut self) {
        if !self.stopped {
            let t = self.timeout.min(DROP_STOP_TIMEOUT);
            let _ = write_deadline(&mut self.w, &rp2040_line(self.resync, b"S"), t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_done, rp2040_line, rp2040_write_timeout, write_deadline, Rp2040Tx};
    use moq_subghz_link::{conformance, Link, LinkError};
    use moq_subghz_wire::FskProfile;
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const PROF: FskProfile = FskProfile {
        bitrate: 300_000,
        fdev: 150_000,
        preamble: 6,
        unchecked: false,
    };

    fn stick() -> (Rp2040Tx, std::fs::File) {
        let (w, r) = pty();
        (
            Rp2040Tx::new(
                w,
                Arc::new(AtomicU64::new(0)),
                Arc::new(AtomicU64::new(0)),
                PROF,
                7_093,
            ),
            r,
        )
    }

    /// A pty pair: (writer side as a non-blocking File, reader fd kept open).
    fn pty() -> (std::fs::File, std::fs::File) {
        use std::os::fd::FromRawFd;
        let (mut m, mut s) = (0, 0);
        // SAFETY: openpty writes two fds into the out parameters.
        let r = unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        assert_eq!(r, 0);
        // SAFETY: raw termios on our own fd, then O_NONBLOCK as open_rp2040 sets it.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            libc::tcgetattr(s, &mut t);
            libc::cfmakeraw(&mut t);
            libc::tcsetattr(s, libc::TCSANOW, &t);
            libc::fcntl(
                s,
                libc::F_SETFL,
                libc::fcntl(s, libc::F_GETFL) | libc::O_NONBLOCK,
            );
            (std::fs::File::from_raw_fd(s), std::fs::File::from_raw_fd(m))
        }
    }

    #[test]
    fn write_deadline_fails_on_a_reader_that_stops() {
        use std::io::Read;
        let (mut w, mut r) = pty();
        // a reader that keeps up: everything arrives
        write_deadline(&mut w, b"Q 0102\n", Duration::from_millis(200)).unwrap();
        let mut got = [0u8; 7];
        r.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"Q 0102\n");
        // a reader that never reads again: the write gives up near its deadline
        let t = Instant::now();
        let big = vec![b'a'; 1 << 20];
        assert!(write_deadline(&mut w, &big, Duration::from_millis(300)).is_err());
        let took = t.elapsed();
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_secs(2),
            "{took:?}"
        );
    }

    #[test]
    fn resync_makes_a_truncated_line_invalid() {
        assert_eq!(rp2040_line(false, b"Q 01"), b"Q 01\n");
        // what a timed-out write left ("Q 01") plus the next line must not parse as hex
        let mut stick_sees = b"Q 01".to_vec();
        stick_sees.extend_from_slice(&rp2040_line(true, b"Q 02"));
        let first = stick_sees.split(|&c| c == b'\n').next().unwrap();
        assert_eq!(first, b"Q 01x");
    }

    #[test]
    fn write_timeout_covers_slow_profiles() {
        assert_eq!(rp2040_write_timeout(7_093), Duration::from_secs(1)); // 300 kbit/s
        assert_eq!(
            rp2040_write_timeout(1_800_000),
            Duration::from_millis(5_400)
        ); // 1.2 kbit/s
    }

    #[test]
    fn passes_the_link_conformance_checks() {
        use std::io::Read;
        let (mut l, mut r) = stick();
        conformance::tx(&mut l, &[0xab, 0x01]);
        // only the valid packet and the stop went out: the refused sends wrote nothing
        let want = b"Q ab01\nS\n";
        let mut got = vec![0u8; want.len()];
        r.read_exact(&mut got).unwrap();
        assert_eq!(got, want);
    }

    #[test]
    fn parses_the_sticks_done_line() {
        assert_eq!(parse_done("D 123456 7 98765"), Some(7));
        assert_eq!(parse_done("B 123"), None);
        assert_eq!(parse_done("D x 7 1"), None);
        assert_eq!(parse_done("D 1"), None);
        assert_eq!(parse_done("ERR tx"), None);
        assert_eq!(parse_done("OK F None"), None);
    }

    fn counted_stick() -> (Rp2040Tx, std::fs::File, Arc<AtomicU64>) {
        let (w, r) = pty();
        let drained = Arc::new(AtomicU64::new(0));
        let l = Rp2040Tx::new(w, Arc::new(AtomicU64::new(0)), drained.clone(), PROF, 7_093);
        (l, r, drained)
    }

    #[test]
    fn idle_waits_for_the_stick_to_report_the_queue_empty() {
        use std::sync::atomic::Ordering;
        let (mut l, _r, drained) = counted_stick();
        for _ in 0..3 {
            l.send(&[1, 2, 3]).unwrap();
        }
        let d = drained.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            d.store(3, Ordering::Relaxed);
        });
        let t0 = Instant::now();
        l.idle().unwrap();
        // it waited for the `D 3` line, well past the old fixed 300 ms only if needed
        assert!(
            t0.elapsed() >= Duration::from_millis(140),
            "{:?}",
            t0.elapsed()
        );
        assert_eq!(l.unconfirmed_frames(), 0);
        t.join().unwrap();
    }

    #[test]
    fn idle_reports_frames_the_stick_never_confirmed() {
        let (mut l, mut r, _drained) = counted_stick();
        for _ in 0..4 {
            l.send(&[9; 10]).unwrap();
        }
        l.idle().unwrap(); // the stop still goes out
        assert_eq!(l.unconfirmed_frames(), 4);
        // reported once
        assert_eq!(l.unconfirmed_frames(), 0);
        use std::io::Read;
        let mut got = vec![0u8; "Q 09090909090909090909\n".len() * 4 + 2];
        r.read_exact(&mut got).unwrap();
        assert!(got.ends_with(b"S\n"));
    }

    #[test]
    fn a_frame_the_stick_reported_as_an_error_is_not_left_waiting() {
        use std::sync::atomic::Ordering;
        let (w, _r) = pty();
        let errs = Arc::new(AtomicU64::new(0));
        let mut l = Rp2040Tx::new(w, errs.clone(), Arc::new(AtomicU64::new(0)), PROF, 7_093);
        l.send(&[1]).unwrap();
        errs.store(1, Ordering::Relaxed);
        let t0 = Instant::now();
        l.idle().unwrap();
        assert!(t0.elapsed() < Duration::from_millis(200));
        assert_eq!(l.unconfirmed_frames(), 0);
    }

    #[test]
    fn reports_the_computed_airtime_not_zero() {
        let (mut l, _r) = stick();
        let r = l.send(&[0u8; 255]).unwrap();
        // (6 + 2 + 1 + 255 + 2) * 8 bits at 300 kbit/s
        assert_eq!((r.airtime_us, r.busy_us), (7_093, 7_093));
        assert_eq!((r.delivered, r.acked), (None, None));
        assert_eq!(l.send(&[0u8; 10]).unwrap().airtime_us, 560);
    }

    #[test]
    fn idle_reports_a_stick_that_will_not_take_the_stop() {
        let (mut l, _r) = stick();
        l.timeout = Duration::from_millis(100);
        // fill the pty so the stop cannot be written
        while write_deadline(&mut l.w, &[b'a'; 4096], Duration::from_millis(50)).is_ok() {}
        let e = l.idle().unwrap_err();
        assert!(matches!(e, LinkError::Io(_)), "{e:?}");
        assert!(!l.stopped && l.resync);
    }

    #[test]
    fn drop_stops_the_stick_once() {
        use std::io::Read;
        let (l, mut r) = stick();
        drop(l);
        let mut got = [0u8; 2];
        r.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"S\n");
        // after idle, a drop adds nothing
        let (mut l, mut r) = stick();
        l.idle().unwrap();
        drop(l);
        let mut got = [0u8; 2];
        r.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"S\n");
        let mut rest = [0u8; 1];
        // the pty is now closed on the writer side: nothing more to read
        assert!(!matches!(r.read(&mut rest), Ok(1)));
    }
}
