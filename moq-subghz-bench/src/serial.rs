//! Opening a serial device raw, 8N1, no flow control. Only ever called with a device the user
//! named on the command line.

use anyhow::{bail, Context, Result};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// The two speeds the bench tools use.
#[derive(Clone, Copy)]
pub enum Baud {
    /// The stock receiver sketch
    B9600,
    /// rp2040-sx1276-fw
    B115200,
}

/// Open `dev` non-blocking and raw at `baud`, for reading and writing.
pub fn open(dev: &Path, baud: Baud) -> Result<File> {
    open_with(dev, baud, true)
}

/// [`open`], read only when `write` is false (a logger never writes to the stick).
pub fn open_with(dev: &Path, baud: Baud, write: bool) -> Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .write(write)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(dev)
        .with_context(|| format!("open {}", dev.display()))?;
    let speed = match baud {
        Baud::B9600 => libc::B9600,
        Baud::B115200 => libc::B115200,
    };
    // SAFETY: plain termios calls on a descriptor we own, on a zeroed struct we then fill in.
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(f.as_raw_fd(), &mut t) != 0 {
            bail!("{} is not a serial device", dev.display());
        }
        t.c_iflag = 0;
        t.c_oflag = 0;
        t.c_lflag = 0;
        t.c_cflag = libc::CS8 | libc::CREAD | libc::CLOCAL;
        libc::cfsetispeed(&mut t, speed);
        libc::cfsetospeed(&mut t, speed);
        t.c_cc[libc::VMIN] = 0;
        t.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(f.as_raw_fd(), libc::TCSANOW, &t) != 0 {
            bail!("set the line speed on {}", dev.display());
        }
    }
    Ok(f)
}

/// Wait up to `ms` for the device to have bytes to read.
pub fn readable(f: &File, ms: i32) -> bool {
    let mut p = libc::pollfd {
        fd: f.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    unsafe { libc::poll(&mut p, 1, ms) > 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_file_is_not_a_serial_device() {
        let e = open(Path::new("/dev/null"), Baud::B9600)
            .unwrap_err()
            .to_string();
        assert!(e.contains("not a serial device"), "{e}");
        assert!(open(Path::new("/nonexistent/tty"), Baud::B115200).is_err());
    }
}
