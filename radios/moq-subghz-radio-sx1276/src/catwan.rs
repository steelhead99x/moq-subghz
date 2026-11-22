//! The CatWAN USB stick as a transmit link: one `Q <hex>` line per packet. The stick stops
//! reading USB while its 12-frame TX queue is full, so a write blocks, which is the backpressure.
//! Moved from `moq-subghz-send` as it was; only the `Link` wrapper at the end is new.

use anyhow::{bail, Context, Result};
use moq_subghz_link::{Link, LinkCaps, LinkError, LinkKind, Provenance, RxMeta, TxReport};
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

/// How long one write to the CatWAN may wait for the stick to take it. The stick stops
/// reading USB only while its 12-frame queue is full, and a slot frees once a frame is
/// on air, so allow three frame times, and at least a second for fast profiles.
fn catwan_write_timeout(frame_toa_us: u64) -> Duration {
    Duration::from_micros(3 * frame_toa_us).max(Duration::from_secs(1))
}

/// Timeout for the stick's setup commands, before the profile's frame time matters.
const CATWAN_SETUP_TIMEOUT: Duration = Duration::from_secs(1);

/// One command line for the stick. After a timed-out write the stick may hold the
/// front of a line; the stick runs every line ending in a newline, and would send a
/// truncated `Q` with even-length hex as a short frame. `resync` prefixes `x\n`, so that
/// leftover fails the stick's hex check and is dropped (a bare `x` line gets an `ERR`).
fn catwan_line(resync: bool, body: &[u8]) -> Vec<u8> {
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
            bail!("CatWAN stopped reading: write timed out after {timeout:?}");
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
            Ok(0) => bail!("CatWAN write returned 0"),
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

/// Configure the CatWAN for FSK TX; returns the write half and a counter of the TX
/// errors the stick reports. A reader thread drains the stick's replies (B/D/ERR lines)
/// so its output buffer never fills.
fn open_catwan(
    port: &str,
    freq_hz: u32,
    p: &moq_subghz_wire::FskProfile,
    power: i8,
) -> Result<(std::fs::File, Arc<AtomicU64>)> {
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
            if tx.send(l.trim().to_string()).is_err() {
                // keep draining after setup
            }
        }
    });
    let mut cmd = |c: &str, want: &str| -> Result<String> {
        write_deadline(
            &mut w,
            &catwan_line(false, c.as_bytes()),
            CATWAN_SETUP_TIMEOUT,
        )?;
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(3) {
            if let Ok(l) = rx.recv_timeout(Duration::from_millis(100)) {
                if l.starts_with(want) {
                    return Ok(l);
                }
            }
        }
        bail!("CatWAN: no {want:?} reply to {c:?}")
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
        bail!("CatWAN config error: {r}");
    }
    eprintln!("CatWAN TX ready: {r}");
    Ok((w, errs))
}

/// A CatWAN stick configured for FSK transmit.
pub struct CatwanTx {
    w: std::fs::File,
    /// The stick's `ERR tx` / `ERR txtimeout` lines, counted by a reader thread.
    errs: Arc<AtomicU64>,
    /// How many of `errs` were already reported by `take_tx_errors`.
    seen: u64,
    /// How long one write may wait for the stick (see [`catwan_write_timeout`]).
    timeout: Duration,
    /// A write timed out, possibly mid-line; see [`catwan_line`].
    resync: bool,
    caps: LinkCaps,
}

impl CatwanTx {
    /// Open `port`, put the stick in FSK transmit at `freq_hz` and `power` dBm.
    /// `frame_toa_us` is the airtime of a 255 byte frame, which sets the write timeout.
    pub fn open(
        port: &str,
        freq_hz: u32,
        p: &moq_subghz_wire::FskProfile,
        power: i8,
        frame_toa_us: u64,
    ) -> Result<Self> {
        let (w, errs) = open_catwan(port, freq_hz, p, power)?;
        Ok(CatwanTx {
            w,
            errs,
            seen: 0,
            timeout: catwan_write_timeout(frame_toa_us),
            resync: false,
            caps: LinkCaps {
                name: "catwan-fsk",
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
        })
    }
}

impl Link for CatwanTx {
    fn caps(&self) -> &LinkCaps {
        &self.caps
    }

    fn send(&mut self, fr: &[u8]) -> Result<TxReport, LinkError> {
        let mut body = Vec::with_capacity(2 + 2 * fr.len());
        body.extend_from_slice(b"Q ");
        for b in fr {
            body.extend_from_slice(format!("{b:02x}").as_bytes());
        }
        let r = write_deadline(&mut self.w, &catwan_line(self.resync, &body), self.timeout);
        self.resync = r.is_err();
        r.map_err(|e| LinkError::Io(e.to_string()))?;
        // the stick queues the frame: when it goes on air and whether it arrives are unknown here
        Ok(TxReport {
            attempts: 1,
            ..TxReport::default()
        })
    }

    fn recv(&mut self, _: &mut [u8], _: Duration) -> Result<Option<RxMeta>, LinkError> {
        Err(LinkError::Unsupported)
    }

    /// Let the stick drain its queue, then stop it. A failure to stop it is printed, not returned:
    /// the run is over either way.
    fn idle(&mut self) -> Result<(), LinkError> {
        std::thread::sleep(Duration::from_millis(300)); // let the stick drain its queue
        if let Err(e) = write_deadline(&mut self.w, &catwan_line(self.resync, b"S"), self.timeout) {
            eprintln!("stopping the CatWAN failed, it may still be transmitting: {e}");
        }
        Ok(())
    }

    /// TX errors the CatWAN reported since the last call. They arrive asynchronously, after
    /// the packet they belong to was queued.
    fn take_tx_errors(&mut self) -> u64 {
        let now = self.errs.load(Ordering::Relaxed);
        let n = now - self.seen;
        self.seen = now;
        n
    }
}

#[cfg(test)]
mod tests {
    use super::{catwan_line, catwan_write_timeout, write_deadline};
    use std::time::{Duration, Instant};

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
        // SAFETY: raw termios on our own fd, then O_NONBLOCK as open_catwan sets it.
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
        assert_eq!(catwan_line(false, b"Q 01"), b"Q 01\n");
        // what a timed-out write left ("Q 01") plus the next line must not parse as hex
        let mut stick_sees = b"Q 01".to_vec();
        stick_sees.extend_from_slice(&catwan_line(true, b"Q 02"));
        let first = stick_sees.split(|&c| c == b'\n').next().unwrap();
        assert_eq!(first, b"Q 01x");
    }

    #[test]
    fn write_timeout_covers_slow_profiles() {
        assert_eq!(catwan_write_timeout(7_093), Duration::from_secs(1)); // 300 kbit/s
        assert_eq!(
            catwan_write_timeout(1_800_000),
            Duration::from_millis(5_400)
        ); // 1.2 kbit/s
    }
}
