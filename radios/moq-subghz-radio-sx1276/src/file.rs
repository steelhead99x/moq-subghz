//! No radio: frames written as CatWAN `R` lines, paced at the FSK airtime. Moved from
//! `moq-subghz-send` (`--loopback`); the file is what `moq-subghz-recv --replay` reads.

use moq_subghz_link::{
    check_packet, Link, LinkCaps, LinkError, LinkKind, Provenance, RxMeta, TxReport,
};
use std::io::Write;
use std::time::{Duration, Instant};

pub struct FileSink {
    f: std::io::BufWriter<std::fs::File>,
    toa: Duration,
    t0: Instant,
    caps: LinkCaps,
}

impl FileSink {
    /// Create `path`. Each send sleeps `toa` (the airtime of a full frame) before writing its line.
    pub fn create(path: &str, toa: Duration) -> std::io::Result<Self> {
        Ok(FileSink {
            f: std::io::BufWriter::new(std::fs::File::create(path)?),
            toa,
            t0: Instant::now(),
            caps: LinkCaps {
                name: "loopback-file",
                kind: LinkKind::Replay,
                max_payload: moq_subghz_wire::MAX_FRAME,
                max_frames_per_packet: 1,
                tx: true,
                rx: false,
                rx_needs_poll: false,
                reports_rssi: false,
                reports_snr: false,
                provenance: Provenance::Replay,
            },
        })
    }
}

fn io(e: std::io::Error) -> LinkError {
    LinkError::Io(e.to_string())
}

impl Link for FileSink {
    fn caps(&self) -> &LinkCaps {
        &self.caps
    }

    fn send(&mut self, fr: &[u8]) -> Result<TxReport, LinkError> {
        check_packet(self.caps.max_payload, fr)?;
        std::thread::sleep(self.toa);
        let hex: String = fr.iter().map(|b| format!("{b:02x}")).collect();
        writeln!(self.f, "R {} 0 0 {hex}", self.t0.elapsed().as_micros()).map_err(io)?;
        Ok(TxReport {
            busy_us: self.toa.as_micros() as u64,
            airtime_us: self.toa.as_micros() as u64,
            attempts: 1,
            delivered: Some(true),
            acked: None,
            deliver_after_us: None,
        })
    }

    fn recv(&mut self, _: &mut [u8], _: Duration) -> Result<Option<RxMeta>, LinkError> {
        Err(LinkError::Unsupported)
    }

    fn idle(&mut self) -> Result<(), LinkError> {
        self.f.flush().map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moq_subghz_wire::parse_r_line;

    #[test]
    fn writes_r_lines_the_receiver_replays() {
        let path = std::env::temp_dir().join(format!("moq-subghz-filesink-{}", std::process::id()));
        let path_s = path.to_str().unwrap();
        let mut s = FileSink::create(path_s, Duration::ZERO).unwrap();
        assert_eq!(s.send(&[0x01, 0xab, 0xff]).unwrap().attempts, 1);
        s.send(&[0x02]).unwrap();
        s.idle().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let frames: Vec<_> = text
            .lines()
            .map(|l| {
                assert!(l.starts_with("R "), "{l}");
                assert!(l.contains(" 0 0 "), "rssi and snr columns are 0: {l}");
                parse_r_line(l).unwrap()
            })
            .collect();
        assert_eq!(frames[0].frame, [0x01, 0xab, 0xff]);
        assert_eq!(frames[1].frame, [0x02]);
        assert!(frames[0].t_us <= frames[1].t_us);
    }

    #[test]
    fn passes_the_link_conformance_checks() {
        let path =
            std::env::temp_dir().join(format!("moq-subghz-filesink-conf-{}", std::process::id()));
        let mut s = FileSink::create(path.to_str().unwrap(), Duration::ZERO).unwrap();
        moq_subghz_link::conformance::tx(&mut s, &[1, 2, 3]);
        // the refused sends wrote no line
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(text.lines().count(), 1, "{text}");
    }

    #[test]
    fn is_transmit_only() {
        let path =
            std::env::temp_dir().join(format!("moq-subghz-filesink-rx-{}", std::process::id()));
        let mut s = FileSink::create(path.to_str().unwrap(), Duration::ZERO).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            s.recv(&mut [0u8; 8], Duration::ZERO),
            Err(LinkError::Unsupported)
        );
        assert!(s.caps().tx && !s.caps().rx);
    }
}
