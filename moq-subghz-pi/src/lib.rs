//! Pi glue for the SX1276 backends (`moq-subghz-radio-sx1276`): the Dragino HAT's SX1276
//! over spidev and GPIO cdev, in FSK mode. Linux only; empty elsewhere.
#![cfg(target_os = "linux")]

use anyhow::{bail, Context, Result};
use embedded_hal_bus::spi::ExclusiveDevice;
use linux_embedded_hal::gpio_cdev::{Chip, EventRequestFlags, LineEventHandle, LineRequestFlags};
use linux_embedded_hal::spidev::{SpiModeFlags, SpidevOptions};
use linux_embedded_hal::{CdevPin, Delay, SpidevBus};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};
use sx1276::fsk::FskConfig;
use sx1276::{IrqLine, PaOutput, Sx1276};

/// DIO0 as a GPIO cdev rising-edge line, waited on with poll(2).
pub struct CdevIrq {
    ev: LineEventHandle,
}
impl IrqLine for CdevIrq {
    type Error = std::io::Error;
    fn is_asserted(&mut self) -> std::io::Result<bool> {
        Ok(self.ev.get_value().map_err(std::io::Error::other)? != 0)
    }
    fn wait_asserted(&mut self, timeout_us: u32) -> std::io::Result<bool> {
        let deadline = Instant::now() + Duration::from_micros(timeout_us as u64);
        loop {
            if self.is_asserted()? {
                return Ok(true);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(false);
            }
            let ms = left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32;
            let mut pfd = libc::pollfd {
                fd: self.ev.as_raw_fd(),
                events: libc::POLLIN | libc::POLLPRI,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the duration of the call.
            let n = unsafe { libc::poll(&mut pfd, 1, ms) };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
            if n > 0 {
                self.ev.get_event().map_err(std::io::Error::other)?;
            }
        }
    }
}

type Spi = ExclusiveDevice<SpidevBus, CdevPin, Delay>;

/// The HAT's SX1276: spidev0.0, NSS on GPIO 25, reset on GPIO 17, DIO0 on GPIO 4.
pub type Hat = Sx1276<Spi, CdevPin, CdevIrq, Delay>;

/// An SX1276 driver error as an `anyhow` error.
pub fn sx_err<E: std::fmt::Debug>(err: sx1276::Error<E>) -> anyhow::Error {
    anyhow::anyhow!("sx1276: {err:?}")
}

/// Open spidev and the HAT's GPIO lines. The radio is not touched: no reset, no register
/// access, so the caller picks FSK or LoRa.
pub fn open_hat_raw(spi_hz: u32) -> Result<Hat> {
    let mut bus = SpidevBus::open("/dev/spidev0.0").context("open spidev")?;
    let mut o = SpidevOptions::new();
    o.bits_per_word(8).max_speed_hz(spi_hz);
    if bus
        .configure(
            &o.clone()
                .mode(SpiModeFlags::SPI_MODE_0 | SpiModeFlags::SPI_NO_CS)
                .build(),
        )
        .is_err()
    {
        bus.configure(&o.mode(SpiModeFlags::SPI_MODE_0).build())
            .context("configure spidev")?;
    }
    let mut chip = Chip::new("/dev/gpiochip0")?;
    let nss = chip
        .get_line(25)?
        .request(LineRequestFlags::OUTPUT, 1, "moq-subghz")?;
    let rst = chip
        .get_line(17)?
        .request(LineRequestFlags::OUTPUT, 1, "moq-subghz")?;
    let ev = chip.get_line(4)?.events(
        LineRequestFlags::INPUT,
        EventRequestFlags::RISING_EDGE,
        "moq-subghz",
    )?;
    let spi = ExclusiveDevice::new(bus, CdevPin::new(nss)?, Delay)
        .map_err(|x| anyhow::anyhow!("NSS: {x}"))?;
    Ok(Sx1276::new(spi, CdevPin::new(rst)?, CdevIrq { ev }, Delay))
}

/// Open the HAT and configure it for FSK at `freq_hz` with `cfg`, in standby, PA_BOOST
/// at `power` dBm.
pub fn open_hat(spi_hz: u32, freq_hz: u32, cfg: &FskConfig, power: i8) -> Result<Hat> {
    let mut r = open_hat_raw(spi_hz)?;
    r.init(freq_hz, &sx1276::LoRaConfig::default())
        .map_err(sx_err)?;
    r.enter_fsk_mode().map_err(sx_err)?;
    r.set_frequency(freq_hz).map_err(sx_err)?;
    r.write_raw(0x0C, 0x23).map_err(sx_err)?;
    r.configure_fsk(cfg).map_err(sx_err)?;
    r.standby().map_err(sx_err)?;
    r.set_tx_power(PaOutput::PaBoost, power).map_err(sx_err)?;
    if r.frequency().map_err(sx_err)?.abs_diff(freq_hz) > 1000 {
        bail!("frequency readback mismatch");
    }
    Ok(r)
}
