//! The CatWAN stick (`catwan-fw` v5) over USB serial: the line protocol and the port.
//!
//! Lines from the stick:
//!   `I catwan-fw <version> sx_version=0x<hex> t_us=<n>`   reply to `I`
//!   `OK L <freq> <error or None>`, `OK R <error or None>`   replies to `L` and `R`
//!   `R <t_us> <rssi_dbm> <snr_quarter_db> <hex>`           one received frame
//!   `X <t_us> <error>`                                     a receive error

use crate::profile::Profile;
use std::io;
use std::time::Duration;

/// Longest line kept. A 255 byte frame is about 550 bytes in hex.
pub const MAX_LINE: usize = 1024;
/// The firmware release this test was written against.
pub const FW_VERSION_PREFIX: &str = "0.5.";

/// The reply to `I`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Banner {
    /// `catwan-fw` version, from its Cargo.toml.
    pub version: String,
    /// SX1276 silicon version register, 0x12 on a working chip.
    pub sx_version: u8,
    /// The stick's microsecond clock.
    pub t_us: u64,
}

/// Parse the reply to `I`.
pub fn parse_banner(line: &str) -> Result<Banner, String> {
    let mut it = line.split(' ');
    let bad = || format!("not a catwan-fw banner: {line:?}");
    if it.next() != Some("I") || it.next() != Some("catwan-fw") {
        return Err(bad());
    }
    let version = it.next().ok_or_else(bad)?.to_string();
    let sx = it
        .next()
        .and_then(|s| s.strip_prefix("sx_version=0x"))
        .and_then(|s| u8::from_str_radix(s, 16).ok())
        .ok_or_else(bad)?;
    let t_us = it
        .next()
        .and_then(|s| s.strip_prefix("t_us="))
        .and_then(|s| s.parse().ok())
        .ok_or_else(bad)?;
    Ok(Banner {
        version,
        sx_version: sx,
        t_us,
    })
}

/// Accept only the firmware this test was written for, on a chip that answers.
pub fn check_banner(b: &Banner) -> Result<(), String> {
    if !b.version.starts_with(FW_VERSION_PREFIX) {
        return Err(format!(
            "catwan-fw {} (this test is for {FW_VERSION_PREFIX}x, the v5 image)",
            b.version
        ));
    }
    if b.sx_version != 0x12 {
        return Err(format!(
            "SX1276 version register 0x{:02x}, expected 0x12",
            b.sx_version
        ));
    }
    Ok(())
}

/// One received frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Rx {
    /// The stick's clock when the frame ended, microseconds.
    pub t_us: u64,
    /// Packet RSSI in dBm.
    pub rssi: i32,
    /// Packet SNR in dB.
    pub snr_db: f32,
    /// The radio frame.
    pub frame: Vec<u8>,
}

/// Parse an `R` line. `None` for any other line or a malformed one. This is
/// [`moq_subghz_wire::parse_r_line`], so the receiver and this tool accept the same lines.
pub fn parse_rx(line: &str) -> Option<Rx> {
    let r = moq_subghz_wire::parse_r_line(line)?;
    Some(Rx {
        t_us: r.t_us,
        rssi: r.rssi,
        snr_db: r.snr as f32 / 4.0,
        frame: r.frame,
    })
}

/// An `R` line as the stick prints it, for loopback files and tests.
pub fn r_line(t_us: u64, rssi: i32, snr_quarter_db: i32, frame: &[u8]) -> String {
    let hex: String = frame.iter().map(|b| format!("{b:02x}")).collect();
    format!("R {t_us} {rssi} {snr_quarter_db} {hex}")
}

/// A reply `OK <cmd> ...`: `None` for another line, an error when the stick reports one.
pub fn parse_ok(line: &str, cmd: char) -> Option<Result<(), String>> {
    let rest = line.strip_prefix("OK ")?.strip_prefix(cmd)?;
    let rest = rest.strip_prefix(' ')?;
    Some(if rest.ends_with(" None") || rest == "None" {
        Ok(())
    } else {
        Err(format!("stick refused {cmd}: {line}"))
    })
}

/// Splits serial bytes into lines without holding more than [`MAX_LINE`] bytes.
#[derive(Default)]
pub struct LineBuf {
    buf: Vec<u8>,
    skipping: bool,
    /// Lines dropped for being too long.
    pub dropped: u64,
}

impl LineBuf {
    /// Feed bytes; complete lines come back trimmed, in order.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for &b in bytes {
            if b == b'\n' {
                if self.skipping {
                    self.skipping = false;
                    self.dropped += 1;
                } else {
                    let s = String::from_utf8_lossy(&self.buf).trim_end().to_string();
                    if !s.is_empty() {
                        out.push(s);
                    }
                }
                self.buf.clear();
            } else if !self.skipping {
                if self.buf.len() >= MAX_LINE {
                    self.skipping = true;
                    self.buf.clear();
                } else {
                    self.buf.push(b);
                }
            }
        }
        out
    }
}

/// Lines to and from the stick.
pub trait LineIo {
    /// Send one command line.
    fn send(&mut self, line: &str) -> io::Result<()>;
    /// The next line, or `None` after `timeout` with nothing.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<String>>;
}

/// Read until `pick` accepts a line. Lines it skips (frames left over from an earlier run)
/// are dropped. Gives up after `polls` empty reads.
fn wait_for<T>(
    io: &mut impl LineIo,
    polls: u32,
    what: &str,
    mut pick: impl FnMut(&str) -> Option<Result<T, String>>,
) -> Result<T, String> {
    let mut idle = 0;
    while idle < polls {
        match io
            .recv(Duration::from_millis(250))
            .map_err(|e| e.to_string())?
        {
            None => idle += 1,
            Some(l) => {
                if let Some(r) = pick(&l) {
                    return r;
                }
            }
        }
    }
    Err(format!("no reply to {what} from the stick"))
}

/// Check the banner, set the radio and start continuous receive. Fails loud on the wrong
/// firmware, a silent stick or any error the stick reports.
pub fn start(io: &mut impl LineIo, p: &Profile) -> Result<Banner, String> {
    p.validate()?;
    let e = |x: io::Error| x.to_string();
    io.send("I").map_err(e)?;
    let b = wait_for(io, 12, "I", |l| {
        l.starts_with("I ").then(|| parse_banner(l))
    })?;
    check_banner(&b)?;
    io.send(&p.stick_command()).map_err(e)?;
    wait_for(io, 12, "L", |l| parse_ok(l, 'L'))?;
    io.send("R").map_err(e)?;
    wait_for(io, 12, "R", |l| parse_ok(l, 'R'))?;
    Ok(b)
}

/// A serial port as a [`LineIo`].
#[cfg(unix)]
pub struct Serial {
    file: std::fs::File,
    lines: LineBuf,
    pending: std::collections::VecDeque<String>,
}

#[cfg(unix)]
impl Serial {
    /// Open `path` raw at 115200 8N1 (the stick ignores the rate, it is USB CDC). `permit` is the
    /// licence gate's receive permit: the port is written to (the start command), so it is not
    /// opened without one.
    pub fn open(_permit: &moq_subghz_licence::RxPermit, path: &str) -> anyhow::Result<Serial> {
        use anyhow::{bail, Context};
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOCTTY)
            .open(path)
            .with_context(|| format!("open {path}"))?;
        // SAFETY: plain termios calls on a valid fd.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(file.as_raw_fd(), &mut t) != 0 {
                bail!("{path} is not a serial port (tcgetattr failed)");
            }
            libc::cfmakeraw(&mut t);
            t.c_cflag |= libc::CLOCAL | libc::CREAD;
            libc::cfsetspeed(&mut t, libc::B115200);
            if libc::tcsetattr(file.as_raw_fd(), libc::TCSANOW, &t) != 0 {
                bail!("tcsetattr failed on {path}");
            }
            libc::tcflush(file.as_raw_fd(), libc::TCIOFLUSH);
        }
        Ok(Serial {
            file,
            lines: LineBuf::default(),
            pending: Default::default(),
        })
    }

    /// Lines dropped for being too long.
    pub fn dropped(&self) -> u64 {
        self.lines.dropped
    }
}

#[cfg(unix)]
impl LineIo for Serial {
    fn send(&mut self, line: &str) -> io::Result<()> {
        use std::io::Write;
        self.file.write_all(line.as_bytes())?;
        self.file.write_all(b"\n")
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<String>> {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        if let Some(l) = self.pending.pop_front() {
            return Ok(Some(l));
        }
        let mut pfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let n = unsafe {
            libc::poll(
                &mut pfd,
                1,
                timeout.as_millis().min(i32::MAX as u128) as i32,
            )
        };
        if n < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(e)
            };
        }
        if n == 0 {
            return Ok(None);
        }
        let mut b = [0u8; 4096];
        let got = self.file.read(&mut b)?;
        if got == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "serial port closed",
            ));
        }
        self.pending.extend(self.lines.push(&b[..got]));
        Ok(self.pending.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::named;
    use std::collections::VecDeque;

    /// A scripted stick: each sent line makes it queue its replies. Empty queue = timeout.
    struct Fake {
        sent: Vec<String>,
        out: VecDeque<String>,
        script: Vec<(&'static str, Vec<&'static str>)>,
    }
    impl LineIo for Fake {
        fn send(&mut self, line: &str) -> io::Result<()> {
            self.sent.push(line.to_string());
            for (pre, replies) in &self.script {
                if line.starts_with(pre) {
                    self.out.extend(replies.iter().map(|s| s.to_string()));
                }
            }
            Ok(())
        }
        fn recv(&mut self, _: Duration) -> io::Result<Option<String>> {
            Ok(self.out.pop_front())
        }
    }

    const BANNER: &str = "I catwan-fw 0.5.0 sx_version=0x12 t_us=1234567";

    fn fake(script: Vec<(&'static str, Vec<&'static str>)>) -> Fake {
        Fake {
            sent: vec![],
            out: ["R 1 -50 8 aabb".to_string()].into(), // a stale frame from before
            script,
        }
    }

    #[test]
    fn banner_parses_and_is_checked() {
        let b = parse_banner(BANNER).unwrap();
        assert_eq!(
            (b.version.as_str(), b.sx_version, b.t_us),
            ("0.5.0", 0x12, 1234567)
        );
        assert!(check_banner(&b).is_ok());
        let old = parse_banner("I catwan-fw 0.4.1 sx_version=0x12 t_us=1").unwrap();
        assert!(check_banner(&old).unwrap_err().contains("v5"));
        let dead = parse_banner("I catwan-fw 0.5.0 sx_version=0x00 t_us=1").unwrap();
        assert!(check_banner(&dead).is_err());
        assert!(parse_banner("I something 1 2 3").is_err());
        assert!(parse_banner("I catwan-fw 0.5.0 sx_version=0xZZ t_us=1").is_err());
        assert!(parse_banner("OK R None").is_err());
    }

    #[test]
    fn rx_lines_parse() {
        let r = parse_rx("R 5000123 -97 -22 00ff10").unwrap();
        assert_eq!((r.t_us, r.rssi, r.snr_db), (5_000_123, -97, -5.5));
        assert_eq!(r.frame, vec![0x00, 0xFF, 0x10]);
        assert_eq!(parse_rx("R 1 -50 4 ").unwrap().frame, Vec::<u8>::new());
        assert!(parse_rx("R 1 -50 4 abc").is_none());
        assert!(parse_rx("R 1 -50 4 zz").is_none());
        assert!(parse_rx("R 1 -50 4 aa extra").is_none());
        assert!(parse_rx("R x -50 4 aa").is_none());
        assert!(parse_rx("X 1 oops").is_none());
        assert!(parse_rx(&format!("R 1 -50 4 {}", "aa".repeat(256))).is_none());
        assert_eq!(
            parse_rx(&format!("R 1 -50 4 {}", "aa".repeat(255)))
                .unwrap()
                .frame
                .len(),
            255
        );
        // multi-byte characters from a lossy decode must not panic
        assert!(parse_rx("R 1 -50 4 \u{FFFD}\u{FFFD}").is_none());
    }

    #[test]
    fn r_line_round_trips() {
        let l = r_line(77, -101, -9, &[0, 1, 254, 255]);
        assert_eq!(l, "R 77 -101 -9 0001feff");
        let r = parse_rx(&l).unwrap();
        assert_eq!((r.t_us, r.rssi, r.snr_db), (77, -101, -2.25));
        assert_eq!(r.frame, vec![0, 1, 254, 255]);
    }

    #[test]
    fn ok_replies() {
        assert_eq!(parse_ok("OK L 902300000 None", 'L'), Some(Ok(())));
        assert_eq!(parse_ok("OK R None", 'R'), Some(Ok(())));
        assert!(parse_ok("OK L 902300000 Some(BadVersion(0))", 'L')
            .unwrap()
            .is_err());
        assert!(parse_ok("OK R Some(Timeout)", 'R').unwrap().is_err());
        assert_eq!(parse_ok("OK R None", 'L'), None);
        assert_eq!(parse_ok("R 1 2 3 aa", 'R'), None);
    }

    #[test]
    fn line_buffer_splits_caps_and_survives_chunking() {
        let mut b = LineBuf::default();
        assert_eq!(b.push(b"R 1 2"), Vec::<String>::new());
        assert_eq!(
            b.push(b" 3 aa\r\nOK R None\n\n"),
            vec!["R 1 2 3 aa", "OK R None"]
        );
        let mut big = vec![b'x'; MAX_LINE + 50];
        big.push(b'\n');
        big.extend_from_slice(b"ok\n");
        assert_eq!(b.push(&big), vec!["ok"]);
        assert_eq!(b.dropped, 1);
        // exactly at the cap is kept
        let mut at = vec![b'y'; MAX_LINE];
        at.push(b'\n');
        assert_eq!(b.push(&at)[0].len(), MAX_LINE);
        assert_eq!(b.push(b"\xff\xfe\n")[0].chars().count(), 2);
    }

    #[test]
    fn start_sends_i_then_l_then_r_and_skips_stale_frames() {
        let mut f = fake(vec![
            ("I", vec![BANNER]),
            ("L ", vec!["OK L 902300000 None"]),
            ("R", vec!["OK R None"]),
        ]);
        let p = named("sf7-125").unwrap();
        assert_eq!(start(&mut f, &p).unwrap().version, "0.5.0");
        assert_eq!(f.sent, vec!["I", "L 902300000 7 125000 5 2 0", "R"]);
    }

    #[test]
    fn start_fails_loud() {
        let p = named("sf7-125").unwrap();
        let silent = start(&mut fake(vec![]), &p);
        assert!(silent.unwrap_err().contains("no reply to I"));
        let old = start(
            &mut fake(vec![(
                "I",
                vec!["I catwan-fw 0.3.0 sx_version=0x12 t_us=1"],
            )]),
            &p,
        );
        assert!(old.unwrap_err().contains("v5"));
        let refused = start(
            &mut fake(vec![
                ("I", vec![BANNER]),
                ("L ", vec!["OK L 902300000 Some(BadVersion(0))"]),
            ]),
            &p,
        );
        assert!(refused.unwrap_err().contains("refused L"));
        let no_rx = fake(vec![
            ("I", vec![BANNER]),
            ("L ", vec!["OK L 902300000 None"]),
        ]);
        let mut no_rx = no_rx;
        assert!(start(&mut no_rx, &p).unwrap_err().contains("no reply to R"));
        let mut bad = p;
        bad.bw_hz = 62_500;
        let mut untouched = fake(vec![]);
        assert!(start(&mut untouched, &bad).is_err());
        assert!(untouched.sent.is_empty());
    }
}
