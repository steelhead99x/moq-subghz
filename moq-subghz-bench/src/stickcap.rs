//! `stick-cap`: the raw logger for the stock Electronic Cats LoRa receiver sketch. A port of
//! `stickcap.py`. Writes every byte read from the stick with a timestamp to `<out>.raw.jsonl`
//! (base64) and the parsed packets to `<out>.packets.csv`. Stop with SIGINT or SIGTERM, or
//! `--seconds`. It opens only the serial device it is given.

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::Args;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsRawFd;
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
    loop {
        let Some(start) = find(buf, HEAD, 0) else {
            break;
        };
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

fn open_raw(path: &PathBuf) -> Result<std::fs::File> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

pub fn run(o: &Opts) -> Result<()> {
    let dev = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(&o.dev)
        .with_context(|| format!("open {}", o.dev.display()))?;
    let fd = dev.as_raw_fd();
    // SAFETY: plain termios calls on a descriptor we own, on a zeroed struct we then fill in.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(fd, &mut t) != 0 {
            bail!("{} is not a serial device", o.dev.display());
        }
        t.c_iflag = 0;
        t.c_oflag = 0;
        t.c_lflag = 0;
        t.c_cflag = libc::CS8 | libc::CREAD | libc::CLOCAL;
        libc::cfsetispeed(&mut t, libc::B9600);
        libc::cfsetospeed(&mut t, libc::B9600);
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(fd, libc::TCSANOW, &t) != 0 {
            bail!("set 9600 baud on {}", o.dev.display());
        }
        libc::signal(libc::SIGINT, on_signal as usize);
        libc::signal(libc::SIGTERM, on_signal as usize);
    }
    let stem = o.out.display().to_string();
    let mut raw = open_raw(&PathBuf::from(format!("{stem}.raw.jsonl")))?;
    let pk = PathBuf::from(format!("{stem}.packets.csv"));
    let mut csv = open_raw(&pk)?;
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

use std::os::unix::fs::OpenOptionsExt;

#[cfg(test)]
mod tests {
    use super::*;

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
}
