//! `stick-cap`: the raw logger for the stock the board maker CSS mode receiver sketch. Writes every byte read from the stick with a timestamp to `<out>.raw.jsonl`
//! (base64) and the parsed packets to `<out>.packets.csv`. Stop with SIGINT or SIGTERM, or
//! `--seconds`. It opens only the serial device it is given.

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::Args;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Args, Debug)]
pub struct Opts {
    /// Serial device of the stick; it is opened at 9600 baud
    #[arg(long)]
    pub dev: PathBuf,
    /// Output stem: writes <out>.raw.jsonl and <out>.packets.csv
    #[arg(long)]
    pub out: PathBuf,
    /// Stop after this many seconds (0 runs until a signal)
    #[arg(long, default_value_t = 0.0)]
    pub seconds: f64,
    /// Add to the capture files if they exist (by default an existing file is an error)
    #[arg(long)]
    pub append: bool,
}

/// Longest `--seconds` accepted: 30 days.
const MAX_SECONDS: f64 = 30.0 * 86400.0;

/// `--seconds` is a finite number from 0 (until a signal) to 30 days.
pub fn check_seconds(s: f64) -> Result<()> {
    if !s.is_finite() || !(0.0..=MAX_SECONDS).contains(&s) {
        bail!("--seconds {s} is not a number from 0 (until a signal) to {MAX_SECONDS}");
    }
    Ok(())
}

/// A packet found in the stick's text: the payload and the RSSI it reported.
#[derive(Debug, PartialEq)]
pub struct Found {
    pub payload: Vec<u8>,
    pub rssi: i64,
}

/// Pull every complete `Received packet '<payload>' with RSSI <n>` line out of `buf`, leaving
/// the unfinished tail. Bounded: more than 8 KiB of unmatched text keeps only its last 4 KiB.
pub fn scan(buf: &mut Vec<u8>) -> Vec<Found> {
    const HEAD: &[u8] = b"Received packet '";
    const MID: &[u8] = b"' with RSSI ";
    let find = |h: &[u8], n: &[u8], from: usize| {
        h[from..]
            .windows(n.len())
            .position(|w| w == n)
            .map(|i| i + from)
    };
    let mut out = Vec::new();
    while let Some(start) = find(buf, HEAD, 0) {
        let body = start + HEAD.len();
        // The payload ends at the first "' with RSSI <digits>" that is followed by a line end.
        let mut at = body;
        let mut hit = None;
        while let Some(m) = find(buf, MID, at) {
            let mut i = m + MID.len();
            let digits_from = i;
            if buf.get(i) == Some(&b'-') {
                i += 1;
            }
            let d = i;
            while buf.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            let end_ok = i > d
                && (buf.get(i) == Some(&b'\n')
                    || (buf.get(i) == Some(&b'\r') && buf.get(i + 1) == Some(&b'\n')));
            if end_ok {
                let rssi = std::str::from_utf8(&buf[digits_from..i])
                    .ok()
                    .and_then(|s| s.parse().ok());
                let next = if buf[i] == b'\r' { i + 2 } else { i + 1 };
                hit = rssi.map(|r| (m, r, next));
                break;
            }
            at = m + 1;
        }
        let Some((m, rssi, next)) = hit else { break };
        out.push(Found {
            payload: buf[body..m].to_vec(),
            rssi,
        });
        buf.drain(..next);
    }
    if buf.len() > 8192 {
        buf.drain(..buf.len() - 4096);
    }
    out
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

/// Open a capture file: new (never an existing file or a symlink), or appended to with `--append`.
fn open_raw(path: &PathBuf, append: bool) -> Result<std::fs::File> {
    let mut o = OpenOptions::new();
    o.append(true).custom_flags(libc::O_NOFOLLOW);
    if append {
        o.create(true);
    } else {
        o.create_new(true);
    }
    o.open(path).with_context(|| {
        if !append && path.symlink_metadata().is_ok() {
            format!(
                "{} exists: pass --append to add to it, or choose another --out",
                path.display()
            )
        } else {
            format!("open {}", path.display())
        }
    })
}

pub fn run(o: &Opts) -> Result<()> {
    check_seconds(o.seconds)?;
    let stem = o.out.display().to_string();
    let (rawp, pk) = (
        PathBuf::from(format!("{stem}.raw.jsonl")),
        PathBuf::from(format!("{stem}.packets.csv")),
    );
    if !o.append {
        // Before the device is touched: an old capture is never mixed with a new one.
        for p in [&rawp, &pk] {
            anyhow::ensure!(
                p.symlink_metadata().is_err(),
                "{} exists: pass --append to add to it, or choose another --out",
                p.display()
            );
        }
    }
    // Read only: the stick is only listened to.
    let dev = crate::serial::open_with(&o.dev, crate::serial::Baud::B9600, false)?;
    let fd = dev.as_raw_fd();
    // SAFETY: installs handlers that only set a flag.
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }
    let mut raw = open_raw(&rawp, o.append)?;
    let mut csv = open_raw(&pk, o.append)?;
    if csv.metadata()?.len() == 0 {
        writeln!(csv, "t_mono_s,t_epoch_s,rssi_dbm,len,payload_b64")?;
    }
    let b64 = base64::engine::general_purpose::STANDARD;
    let (t0, mut buf) = (Instant::now(), Vec::new());
    while !STOP.load(Ordering::SeqCst)
        && (o.seconds == 0.0 || t0.elapsed().as_secs_f64() < o.seconds)
    {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut p, 1, 200) };
        if r < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            bail!(
                "poll {}: {}",
                o.dev.display(),
                std::io::Error::last_os_error()
            );
        }
        if r <= 0 {
            continue;
        }
        let mut chunk = [0u8; 4096];
        // SAFETY: the buffer is 4096 bytes.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                continue;
            }
            bail!("read {}: {e}", o.dev.display());
        }
        if n == 0 {
            // Readable with nothing to read: the device went away (hang up).
            bail!("{} was closed (end of file)", o.dev.display());
        }
        let d = &chunk[..n as usize];
        let (mono, epoch) = (
            t0.elapsed().as_secs_f64(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_secs_f64(),
        );
        writeln!(
            raw,
            "{{\"t\": {mono:.4}, \"e\": {epoch:.4}, \"b\": \"{}\"}}",
            b64.encode(d)
        )?;
        raw.flush()?;
        buf.extend_from_slice(d);
        for f in scan(&mut buf) {
            writeln!(
                csv,
                "{mono:.4},{epoch:.4},{},{},{}",
                f.rssi,
                f.payload.len(),
                b64.encode(&f.payload)
            )?;
            csv.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_must_be_sane() {
        for ok in [0.0, 0.5, 3600.0, MAX_SECONDS] {
            assert!(check_seconds(ok).is_ok(), "{ok}");
        }
        for bad in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -1.0,
            -0.001,
            1e12,
            MAX_SECONDS + 1.0,
        ] {
            assert!(check_seconds(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn nan_seconds_are_refused_before_the_device_is_opened() {
        let o = Opts {
            dev: "/nonexistent/tty".into(),
            out: "/nonexistent/x".into(),
            seconds: f64::NAN,
            append: false,
        };
        let e = run(&o).unwrap_err().to_string();
        assert!(e.contains("--seconds"), "{e}");
    }

    #[test]
    fn an_existing_capture_needs_append() {
        let d = crate::testutil::Scratch::new("cap");
        let stem = d.0.join("s");
        std::fs::write(format!("{}.raw.jsonl", stem.display()), "old\n").unwrap();
        let o = Opts {
            dev: "/nonexistent/tty".into(),
            out: stem.clone(),
            seconds: 1.0,
            append: false,
        };
        let e = run(&o).unwrap_err().to_string();
        assert!(e.contains("--append"), "{e}");
        assert_eq!(
            std::fs::read_to_string(format!("{}.raw.jsonl", stem.display())).unwrap(),
            "old\n"
        );
        // A symlink is not followed.
        let link = d.0.join("l.raw.jsonl");
        std::os::unix::fs::symlink("/nonexistent/target", &link).unwrap();
        assert!(open_raw(&link, true).is_err());
        assert!(open_raw(&link, false).is_err());
    }

    #[test]
    fn finds_packets_across_reads() {
        let mut b =
            b"noise Received packet 'A:000/002:0102' with RSSI -39\r\nReceived packet 'A:001"
                .to_vec();
        let f = scan(&mut b);
        assert_eq!(
            f,
            [Found {
                payload: b"A:000/002:0102".to_vec(),
                rssi: -39
            }]
        );
        b.extend_from_slice(b"/002:ff' with RSSI -41\n");
        let f = scan(&mut b);
        assert_eq!(
            f,
            [Found {
                payload: b"A:001/002:ff".to_vec(),
                rssi: -41
            }]
        );
        assert!(b.is_empty());
    }

    #[test]
    fn payload_may_hold_quotes_and_the_tail_is_bounded() {
        let mut b = b"Received packet 'it's' with RSSI x' with RSSI 7\n".to_vec();
        assert_eq!(scan(&mut b)[0].payload, b"it's' with RSSI x");
        let mut big = vec![b'z'; 20000];
        scan(&mut big);
        assert_eq!(big.len(), 4096);
    }

    #[test]
    fn scan_never_panics_on_random_bytes() {
        let mut rng = crate::testutil::Rng(11);
        let seed = b"Received packet 'A:000/002:0102' with RSSI -39\r\nxx";
        for i in 0..5000 {
            let mut v: Vec<u8> = if i % 2 == 0 {
                rng.mutate(seed)
            } else {
                (0..rng.below(300)).map(|_| rng.next() as u8).collect()
            };
            let _ = scan(&mut v);
        }
    }
}
