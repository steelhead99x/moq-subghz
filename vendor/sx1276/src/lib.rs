//! `no_std` driver for the Semtech SX1276/77/78/79 CSS mode transceiver, written
//! against the `embedded-hal` 1.0 traits.
//!
//! Designed for throughput: every FIFO access is a single SPI transaction
//! (address byte plus up to 255 data bytes in one `transfer_in_place`),
//! receiving a packet costs four SPI transactions, starting a transmission
//! costs at most six, and register state that rarely changes (op mode, DIO
//! mapping) is cached so it is not rewritten per packet. Completion is
//! signalled through the DIO0 interrupt line via the [`IrqLine`] trait, so a
//! platform can block on a real edge event instead of polling the bus.
//!
//! The chip select (NSS) is handled by the [`SpiDevice`] you pass in. If NSS is
//! a plain GPIO (as on the SX1276 GPS HAT, BCM25) wrap your bus and pin in
//! `embedded_hal_bus::spi::ExclusiveDevice`.
//!
//! ```ignore
//! let spi = ExclusiveDevice::new(bus, nss_pin, Delay)?;
//! let mut radio = Sx1276::new(spi, reset_pin, PolledIrq::new(dio0_pin, Delay, 100), Delay);
//! radio.init(915_000_000, &CssConfig::default())?;
//! radio.start_receive(RxMode::Continuous)?;
//! let mut buf = [0u8; 255];
//! if let Some(pkt) = radio.receive(&mut buf, 1_000_000)? {
//!     handle(&buf[..pkt.len], pkt.rssi_dbm, pkt.snr_db());
//! }
//! ```
#![no_std]
#![deny(unsafe_code)]

pub mod config;
pub mod fsk;
pub mod registers;

pub use config::{
    encode_ocp, encode_pa, frf_bytes, frf_from_hz, hz_from_frf, packet_rssi_dbm, rssi_dbm,
    Bandwidth, CodingRate, ConfigError, CssConfig, HeaderMode, LowDataRateOptimize, PaOutput,
    PaSettings, SpreadingFactor,
};
pub use registers::{Dio0Mapping, Irq, Mode, Register};

use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{InputPin, OutputPin};
use embedded_hal::spi::SpiDevice;
use registers::{lna, opmode, FIFO_SIZE, SILICON_VERSION, WRITE_BIT};

/// Maximum CSS mode payload length.
pub const MAX_PAYLOAD: usize = 255;

/// Maximum SPI clock supported by the SX1276 (datasheet table 7: 10 MHz).
pub const MAX_SPI_HZ: u32 = 10_000_000;

/// Driver error.
#[derive(Debug, PartialEq, Eq)]
pub enum Error<E> {
    /// SPI bus error.
    Spi(E),
    /// Reset pin could not be driven.
    Reset,
    /// Interrupt line could not be read or waited on.
    Irq,
    /// `RegVersion` did not read 0x12.
    BadVersion(u8),
    /// Rejected configuration.
    Config(ConfigError),
    /// Rejected FSK configuration.
    Fsk(fsk::FskError),
    /// Timed out waiting for the radio.
    Timeout,
    /// Packet received with a bad payload CRC (already cleared and discarded).
    Crc,
    /// Payload empty or longer than 255 bytes, or receive buffer too small.
    PayloadSize,
}

impl<E> From<ConfigError> for Error<E> {
    fn from(e: ConfigError) -> Self {
        Error::Config(e)
    }
}

/// An interrupt input (DIO0). Implement this for your platform's edge-event
/// mechanism; [`PolledIrq`] adapts any `InputPin`.
pub trait IrqLine {
    /// Platform error.
    type Error: core::fmt::Debug;
    /// Current level of the line (true = asserted / high).
    fn is_asserted(&mut self) -> Result<bool, Self::Error>;
    /// Block until the line is asserted or `timeout_us` elapses; returns whether it is asserted.
    fn wait_asserted(&mut self, timeout_us: u32) -> Result<bool, Self::Error>;
}

/// [`IrqLine`] implementation that polls an `InputPin`, sleeping `interval_us` between reads.
pub struct PolledIrq<P, D> {
    pin: P,
    delay: D,
    interval_us: u32,
}

impl<P, D> PolledIrq<P, D> {
    /// Wrap a pin; `interval_us` is clamped to at least 1.
    pub fn new(pin: P, delay: D, interval_us: u32) -> Self {
        PolledIrq {
            pin,
            delay,
            interval_us: interval_us.max(1),
        }
    }
    /// Give back the pin and delay.
    pub fn release(self) -> (P, D) {
        (self.pin, self.delay)
    }
}

impl<P: InputPin, D: DelayNs> IrqLine for PolledIrq<P, D> {
    type Error = P::Error;
    fn is_asserted(&mut self) -> Result<bool, P::Error> {
        self.pin.is_high()
    }
    fn wait_asserted(&mut self, timeout_us: u32) -> Result<bool, P::Error> {
        let mut waited = 0u32;
        loop {
            if self.pin.is_high()? {
                return Ok(true);
            }
            if waited >= timeout_us {
                return Ok(false);
            }
            self.delay.delay_us(self.interval_us);
            waited = waited.saturating_add(self.interval_us);
        }
    }
}

/// Receive mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RxMode {
    /// Stay in RX; packets keep arriving until you change mode.
    Continuous,
    /// One packet or a symbol timeout (`CssConfig::symbol_timeout`), then standby.
    Single,
}

/// Receive state for [`Sx1276::fsk_poll_long`] (raw FIFO stream: length byte + payload).
#[derive(Clone, Debug)]
pub struct FskLongRx {
    raw: [u8; 256],
    got: usize,
    len: usize,
    rssi: u8,
    last_us: u32,
}

impl Default for FskLongRx {
    fn default() -> Self {
        FskLongRx {
            raw: [0; 256],
            got: 0,
            len: 0,
            rssi: 0,
            last_us: 0,
        }
    }
}

impl FskLongRx {
    /// Payload of the packet last returned by `fsk_poll_long`.
    ///
    /// Empty when no packet has been stored. The default value used to panic:
    /// `1..=0` is an empty inclusive range, which Rust rejects.
    pub fn payload(&self) -> &[u8] {
        let end = match self.len.checked_add(1) {
            Some(end) if self.len > 0 && end <= self.raw.len() => end,
            _ => return &[],
        };
        &self.raw[1..end]
    }
    /// True while a packet is partly read.
    pub fn in_progress(&self) -> bool {
        self.got > 0
    }
}

/// Metadata of a received packet; the payload is in the caller's buffer.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PacketInfo {
    /// Payload length in bytes.
    pub len: usize,
    /// Packet RSSI in dBm.
    pub rssi_dbm: i16,
    /// Packet SNR in quarter dB (raw `RegPktSnrValue`).
    pub snr_quarter_db: i8,
}

impl PacketInfo {
    /// Packet SNR in dB.
    pub fn snr_db(&self) -> f32 {
        self.snr_quarter_db as f32 / 4.0
    }
}

/// SX1276 driver.
pub struct Sx1276<SPI, RST, IRQ, D> {
    spi: SPI,
    reset: RST,
    irq: IRQ,
    delay: D,
    /// Cached `RegOpMode` without the mode bits (LongRangeMode, LF page).
    opmode_base: u8,
    /// Cached mode as last written (or implied by a completed TX/CAD/RX single).
    mode: Option<Mode>,
    /// Cached `RegDioMapping1`.
    dio_mapping1: Option<u8>,
    high_frequency: bool,
    config: CssConfig,
    /// The FSK config keeps CRC-failed packets ([`fsk::FskConfig::crc_auto_clear`] off),
    /// so reads must check CrcOk.
    fsk_crc_check: bool,
    buf: [u8; FIFO_SIZE + 1],
}

type R<T, E> = Result<T, Error<E>>;

impl<SPI, RST, IRQ, D> Sx1276<SPI, RST, IRQ, D>
where
    SPI: SpiDevice,
    RST: OutputPin,
    IRQ: IrqLine,
    D: DelayNs,
{
    /// Create the driver. Nothing is sent until [`reset`](Self::reset) or [`init`](Self::init).
    pub fn new(spi: SPI, reset: RST, irq: IRQ, delay: D) -> Self {
        Sx1276 {
            spi,
            reset,
            irq,
            delay,
            opmode_base: opmode::LONG_RANGE_MODE,
            mode: None,
            dio_mapping1: None,
            high_frequency: true,
            config: CssConfig::default(),
            fsk_crc_check: false,
            buf: [0; FIFO_SIZE + 1],
        }
    }

    /// Give back the peripherals.
    pub fn release(self) -> (SPI, RST, IRQ, D) {
        (self.spi, self.reset, self.irq, self.delay)
    }

    /// Mutable access to the interrupt line (e.g. to read DIO0 directly).
    pub fn irq_line(&mut self) -> &mut IRQ {
        &mut self.irq
    }

    /// The modem configuration last applied with [`configure`](Self::configure).
    pub fn config(&self) -> &CssConfig {
        &self.config
    }

    // ---------------------------------------------------------------- raw access

    /// Read one register by raw address.
    pub fn read_raw(&mut self, addr: u8) -> R<u8, SPI::Error> {
        let mut b = [addr & !WRITE_BIT, 0];
        self.spi.transfer_in_place(&mut b).map_err(Error::Spi)?;
        Ok(b[1])
    }

    /// Write one register by raw address.
    pub fn write_raw(&mut self, addr: u8, value: u8) -> R<(), SPI::Error> {
        let mut b = [addr | WRITE_BIT, value];
        self.spi.transfer_in_place(&mut b).map_err(Error::Spi)
    }

    /// Read a register.
    pub fn read_register(&mut self, reg: Register) -> R<u8, SPI::Error> {
        self.read_raw(reg.addr())
    }

    /// Write a register.
    pub fn write_register(&mut self, reg: Register, value: u8) -> R<(), SPI::Error> {
        self.write_raw(reg.addr(), value)
    }

    /// Read-modify-write: bits in `mask` are replaced with those from `value`.
    pub fn modify_register(&mut self, reg: Register, mask: u8, value: u8) -> R<(), SPI::Error> {
        let old = self.read_register(reg)?;
        self.write_register(reg, (old & !mask) | (value & mask))
    }

    /// Burst read `buf.len()` (<= 256) consecutive registers starting at `start`
    /// in one SPI transaction. Reading [`Register::Fifo`] streams the FIFO.
    pub fn read_burst(&mut self, start: Register, buf: &mut [u8]) -> R<(), SPI::Error> {
        let n = buf.len();
        if n > FIFO_SIZE {
            return Err(Error::PayloadSize);
        }
        let frame = &mut self.buf[..=n];
        frame[0] = start.addr() & !WRITE_BIT;
        frame[1..].fill(0);
        self.spi.transfer_in_place(frame).map_err(Error::Spi)?;
        buf.copy_from_slice(&frame[1..]);
        Ok(())
    }

    /// Burst write `data` (<= 256 bytes) to consecutive registers starting at
    /// `start` in one SPI transaction. Writing [`Register::Fifo`] streams the FIFO.
    pub fn write_burst(&mut self, start: Register, data: &[u8]) -> R<(), SPI::Error> {
        let n = data.len();
        if n > FIFO_SIZE {
            return Err(Error::PayloadSize);
        }
        let frame = &mut self.buf[..=n];
        frame[0] = start.addr() | WRITE_BIT;
        frame[1..].copy_from_slice(data);
        self.spi.transfer_in_place(frame).map_err(Error::Spi)
    }

    // ---------------------------------------------------------------- lifecycle

    /// Pulse RESET low (1 ms) and wait 10 ms for the chip to come up.
    /// Clears all cached state; the chip is back in FSK standby.
    pub fn reset(&mut self) -> R<(), SPI::Error> {
        self.reset.set_low().map_err(|_| Error::Reset)?;
        self.delay.delay_ms(1);
        self.reset.set_high().map_err(|_| Error::Reset)?;
        self.delay.delay_ms(10);
        self.mode = None;
        self.dio_mapping1 = None;
        self.opmode_base = opmode::LONG_RANGE_MODE;
        Ok(())
    }

    /// `RegVersion` (0x12 for SX1276).
    pub fn version(&mut self) -> R<u8, SPI::Error> {
        self.read_register(Register::Version)
    }

    /// Reset, check the silicon version, enter CSS mode, set the frequency and
    /// modem configuration, and leave the radio in standby.
    pub fn init(&mut self, frequency_hz: u32, config: &CssConfig) -> R<(), SPI::Error> {
        config.validate()?;
        self.reset()?;
        let v = self.version()?;
        if v != SILICON_VERSION {
            return Err(Error::BadVersion(v));
        }
        self.enter_css_mode()?;
        self.set_frequency(frequency_hz)?;
        // TX and RX both use the full FIFO starting at 0 (half duplex).
        self.write_burst(Register::FifoTxBaseAddr, &[0x00, 0x00])?;
        self.write_register(Register::Lna, lna::MAX_GAIN_BOOST_HF)?;
        self.write_register(Register::IrqFlagsMask, 0x00)?;
        self.configure(config)?;
        self.clear_irq(Irq::ALL)?;
        self.set_mode(Mode::Standby)
    }

    /// Switch from FSK to CSS mode: FSK sleep, then LongRangeMode + sleep (the bit can
    /// only change in sleep). Leaves the radio in CSS mode sleep.
    pub fn enter_css_mode(&mut self) -> R<(), SPI::Error> {
        let lf = self.read_register(Register::OpMode)? & opmode::LOW_FREQUENCY_MODE_ON;
        self.write_register(Register::OpMode, lf | Mode::Sleep as u8)?;
        self.delay.delay_ms(10);
        self.opmode_base = opmode::LONG_RANGE_MODE | lf;
        self.dio_mapping1 = None;
        self.write_register(Register::OpMode, self.opmode_base | Mode::Sleep as u8)?;
        self.delay.delay_ms(10);
        self.mode = Some(Mode::Sleep);
        Ok(())
    }

    /// Switch to FSK/OOK mode (LongRangeMode = 0, which can only change in sleep).
    /// Leaves the radio in FSK sleep. Call [`enter_css_mode`](Self::enter_css_mode) to go back.
    pub fn enter_fsk_mode(&mut self) -> R<(), SPI::Error> {
        let lf = self.opmode_base & opmode::LOW_FREQUENCY_MODE_ON;
        self.write_register(Register::OpMode, self.opmode_base | Mode::Sleep as u8)?;
        self.delay.delay_ms(10);
        self.opmode_base = lf;
        self.dio_mapping1 = None;
        self.write_register(Register::OpMode, self.opmode_base | Mode::Sleep as u8)?;
        self.delay.delay_ms(10);
        self.mode = Some(Mode::Sleep);
        Ok(())
    }

    /// Apply an FSK packet-mode configuration (FSK sleep or standby). Bursts:
    /// bitrate+Fdev (0x02..=0x05), RxBw+AfcBw (0x12..=0x13), preamble+sync
    /// (0x25..=0x2F), packet config (0x30..=0x32), plus FifoThresh and BitRateFrac.
    pub fn configure_fsk(&mut self, cfg: &fsk::FskConfig) -> R<(), SPI::Error> {
        cfg.validate().map_err(Error::Fsk)?;
        let (br, frac) = fsk::bitrate_regs(cfg.bitrate_bps);
        let fd = fsk::fdev_reg(cfg.fdev_hz);
        let (rxbw, _) = cfg.rx_bw();
        self.write_raw_burst(
            fsk::reg::BITRATE_MSB,
            &[(br >> 8) as u8, br as u8, (fd >> 8) as u8, fd as u8],
        )?;
        self.write_raw_burst(fsk::reg::RX_BW, &[rxbw, rxbw])?;
        let mut ps = [0u8; 11];
        ps[0] = (cfg.preamble_bytes >> 8) as u8;
        ps[1] = cfg.preamble_bytes as u8;
        ps[2] = cfg.sync_config();
        ps[3..].copy_from_slice(&cfg.sync_word);
        self.write_raw_burst(fsk::reg::PREAMBLE_MSB, &ps)?;
        self.write_raw_burst(
            fsk::reg::PACKET_CONFIG1,
            &[cfg.packet_config1(), cfg.packet_config2(), 0xFF],
        )?;
        // TxStartCondition = FifoNotEmpty, FifoThreshold = 32 (refill point for >64-byte packets).
        self.write_raw(fsk::reg::FIFO_THRESH, 0x80 | fsk::FIFO_THRESHOLD)?;
        self.write_raw(fsk::reg::BITRATE_FRAC, frac)?;
        self.fsk_crc_check = cfg.checks_crc_on_read();
        Ok(())
    }

    /// Read and drop whatever the FIFO still holds (at most 300 bytes); true if any.
    fn fsk_drain(&mut self) -> R<bool, SPI::Error> {
        let mut any = false;
        let mut guard = 0;
        while self.read_raw(fsk::reg::IRQ_FLAGS2)? & fsk::irq2::FIFO_EMPTY == 0 && guard < 300 {
            self.read_raw(0x00)?;
            any = true;
            guard += 1;
        }
        Ok(any)
    }

    /// Burst write starting at a raw address (for FSK-page registers).
    pub fn write_raw_burst(&mut self, addr: u8, data: &[u8]) -> R<(), SPI::Error> {
        let n = data.len();
        if n > FIFO_SIZE {
            return Err(Error::PayloadSize);
        }
        let frame = &mut self.buf[..=n];
        frame[0] = addr | WRITE_BIT;
        frame[1..].copy_from_slice(data);
        self.spi.transfer_in_place(frame).map_err(Error::Spi)
    }

    /// Burst read starting at a raw address.
    pub fn read_raw_burst(&mut self, addr: u8, buf: &mut [u8]) -> R<(), SPI::Error> {
        let n = buf.len();
        if n > FIFO_SIZE {
            return Err(Error::PayloadSize);
        }
        let frame = &mut self.buf[..=n];
        frame[0] = addr & !WRITE_BIT;
        frame[1..].fill(0);
        self.spi.transfer_in_place(frame).map_err(Error::Spi)?;
        buf.copy_from_slice(&frame[1..]);
        Ok(())
    }

    // ---------------------------------------------------------------- mode

    // Raw `RegOpMode` is `op_mode` below.
    // ---------------------------------------------------------------- FSK packets

    /// FSK: load a variable-length packet (<= 63 bytes, so it fits the 64-byte FIFO
    /// with its length byte) and start TX. DIO0 rises on PacketSent.
    /// SPI cost: standby (if needed), one FIFO burst, DIO mapping (first time), mode TX.
    pub fn fsk_start_transmit(&mut self, payload: &[u8]) -> R<(), SPI::Error> {
        let n = payload.len();
        if n == 0 || n >= fsk::FSK_FIFO_SIZE {
            return Err(Error::PayloadSize);
        }
        self.ensure_standby()?;
        let frame = &mut self.buf[..n + 2];
        frame[0] = WRITE_BIT; // FIFO
        frame[1] = n as u8;
        frame[2..].copy_from_slice(payload);
        self.spi.transfer_in_place(frame).map_err(Error::Spi)?;
        self.set_dio_mapping1_raw(0x00)?; // DIO0 = PacketSent (TX) / PayloadReady (RX)
        self.set_mode(Mode::Tx)
    }

    /// FSK: after DIO0 rose, check PacketSent. The chip returns to standby by itself.
    pub fn fsk_finish_transmit(&mut self) -> R<bool, SPI::Error> {
        let f2 = self.read_raw(fsk::reg::IRQ_FLAGS2)?;
        if f2 & fsk::irq2::PACKET_SENT != 0 {
            self.mode = Some(Mode::Standby);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// FSK: enter continuous RX (AutoRestartRx restarts after each packet).
    pub fn fsk_start_receive(&mut self) -> R<(), SPI::Error> {
        self.ensure_standby()?;
        self.set_dio_mapping1_raw(0x00)?;
        self.set_mode(Mode::RxContinuous)
    }

    /// FSK: if PayloadReady, read the packet (length byte + payload) and the RSSI.
    /// Packets failing CRC are dropped by the chip and never reported.
    pub fn fsk_read_packet(&mut self, buf: &mut [u8]) -> R<Option<PacketInfo>, SPI::Error> {
        let f2 = self.read_raw(fsk::reg::IRQ_FLAGS2)?;
        if f2 & fsk::irq2::PAYLOAD_READY == 0 {
            return Ok(None);
        }
        if self.fsk_crc_check && f2 & fsk::irq2::CRC_OK == 0 {
            self.fsk_drain()?;
            return Err(Error::Crc);
        }
        let rssi = self.read_raw(fsk::reg::RSSI_VALUE)?;
        let len = self.read_raw(0x00)? as usize;
        if len == 0 || len >= fsk::FSK_FIFO_SIZE || len > buf.len() {
            self.fsk_drain()?;
            return Err(Error::PayloadSize);
        }
        self.read_raw_burst(0x00, &mut buf[..len])?;
        Ok(Some(PacketInfo {
            len,
            rssi_dbm: -((rssi as i16) / 2),
            snr_quarter_db: 0,
        }))
    }

    /// FSK: transmit a variable-length packet of up to 255 bytes. Loads the length
    /// byte plus the first 63 bytes, enters TX, then refills the FIFO by polling
    /// FifoLevel (set while the FIFO holds more than [`fsk::FIFO_THRESHOLD`] bytes),
    /// writing up to 31 bytes each time it clears (~0.85 ms of margin at 300 kbit/s).
    /// Returns once the last byte is in the FIFO; DIO0 then rises on PacketSent
    /// ([`Self::fsk_finish_transmit`]). A FIFO underrun corrupts the packet (the
    /// receiver's CRC drops it); `spin_limit` bounds the polling loop.
    pub fn fsk_transmit_long(&mut self, payload: &[u8], spin_limit: u32) -> R<(), SPI::Error> {
        let n = payload.len();
        if n == 0 || n > 255 {
            return Err(Error::PayloadSize);
        }
        self.ensure_standby()?;
        let first = n.min(fsk::FSK_FIFO_SIZE - 1);
        {
            let frame = &mut self.buf[..first + 2];
            frame[0] = WRITE_BIT; // FIFO
            frame[1] = n as u8;
            frame[2..].copy_from_slice(&payload[..first]);
            self.spi.transfer_in_place(frame).map_err(Error::Spi)?;
        }
        self.set_dio_mapping1_raw(0x00)?;
        self.set_mode(Mode::Tx)?;
        let mut off = first;
        let mut spins = 0u32;
        let room = fsk::FSK_FIFO_SIZE - 1 - fsk::FIFO_THRESHOLD as usize;
        while off < n {
            let f2 = self.read_raw(fsk::reg::IRQ_FLAGS2)?;
            if f2 & fsk::irq2::FIFO_LEVEL == 0 {
                let k = (n - off).min(room);
                let frame = &mut self.buf[..k + 1];
                frame[0] = WRITE_BIT;
                frame[1..].copy_from_slice(&payload[off..off + k]);
                self.spi.transfer_in_place(frame).map_err(Error::Spi)?;
                off += k;
                spins = 0;
            } else {
                spins += 1;
                if spins > spin_limit {
                    let _ = self.standby();
                    return Err(Error::Timeout);
                }
            }
        }
        Ok(())
    }

    /// FSK: poll for a variable-length packet of up to 255 bytes (receiver already in
    /// [`Self::fsk_start_receive`]). Call as often as possible (every < ~0.8 ms at 300
    /// kbit/s): drains 32 bytes whenever FifoLevel is set and the rest on PayloadReady.
    /// `now_us` is any free-running microsecond clock; a partial packet with no FIFO
    /// progress for `stall_us` (the chip silently clears the FIFO on a CRC failure) is
    /// abandoned and reported as [`Error::Crc`]. With
    /// [`fsk::FskConfig::crc_auto_clear`] off, a failed packet is reported as
    /// [`Error::Crc`] at PayloadReady instead, so it can't splice onto the next one. Any
    /// error leaves `st` ready for a fresh packet. On success the payload is
    /// `st.payload()`.
    pub fn fsk_poll_long(
        &mut self,
        st: &mut FskLongRx,
        now_us: u32,
        stall_us: u32,
    ) -> R<Option<PacketInfo>, SPI::Error> {
        let r = self.fsk_poll_long_step(st, now_us, stall_us);
        if r.is_err() {
            // whatever failed, a half-read packet must not carry into the next one
            st.got = 0;
        }
        r
    }

    fn fsk_poll_long_step(
        &mut self,
        st: &mut FskLongRx,
        now_us: u32,
        stall_us: u32,
    ) -> R<Option<PacketInfo>, SPI::Error> {
        let f2 = self.read_raw(fsk::reg::IRQ_FLAGS2)?;
        if f2 & fsk::irq2::PAYLOAD_READY != 0 {
            if self.fsk_crc_check && f2 & fsk::irq2::CRC_OK == 0 {
                self.fsk_drain()?;
                return Err(Error::Crc);
            }
            if st.got == 0 {
                st.rssi = self.read_raw(fsk::reg::RSSI_VALUE)?;
                st.raw[0] = self.read_raw(0x00)?;
                st.got = 1;
            }
            let total = st.raw[0] as usize + 1;
            // The FIFO holds 64 bytes. More than that still to read means the packet was
            // first seen at PayloadReady and its head is gone: an error, not a burst read.
            let need = total.saturating_sub(st.got);
            let mut err = total == 1 || st.got > total || need > fsk::FSK_FIFO_SIZE;
            if !err && need > 0 {
                let (g, t) = (st.got, total);
                self.read_raw_burst(0x00, &mut st.raw[g..t])?;
                st.got = t;
            }
            // anything still in the FIFO means we lost sync with the byte stream
            err |= self.fsk_drain()?;
            let len = total - 1;
            st.got = 0;
            if err {
                return Err(Error::PayloadSize);
            }
            st.len = len;
            return Ok(Some(PacketInfo {
                len,
                rssi_dbm: -((st.rssi as i16) / 2),
                snr_quarter_db: 0,
            }));
        }
        if f2 & fsk::irq2::FIFO_LEVEL != 0 {
            if st.got == 0 {
                st.rssi = self.read_raw(fsk::reg::RSSI_VALUE)?;
            }
            let k = fsk::FIFO_THRESHOLD as usize;
            if st.got + k > st.raw.len() || (st.got > 0 && st.got + k > st.raw[0] as usize + 1) {
                // more data than the length byte announced: stream out of sync
                let mut junk = [0u8; 32];
                self.read_raw_burst(0x00, &mut junk[..k])?;
                st.got = 0;
                st.last_us = now_us;
                return Err(Error::PayloadSize);
            }
            let g = st.got;
            self.read_raw_burst(0x00, &mut st.raw[g..g + k])?;
            st.got += k;
            st.last_us = now_us;
            return Ok(None);
        }
        if st.got > 0 && now_us.wrapping_sub(st.last_us) > stall_us {
            st.got = 0;
            return Err(Error::Crc);
        }
        Ok(None)
    }

    /// Write RegDioMapping1 directly (cache-aware).
    pub fn set_dio_mapping1_raw(&mut self, v: u8) -> R<(), SPI::Error> {
        if self.dio_mapping1 != Some(v) {
            self.write_register(Register::DioMapping1, v)?;
            self.dio_mapping1 = Some(v);
        }
        Ok(())
    }

    pub fn op_mode(&mut self) -> R<u8, SPI::Error> {
        self.read_register(Register::OpMode)
    }

    /// Current mode as read from the chip.
    pub fn mode(&mut self) -> R<Mode, SPI::Error> {
        let m = Mode::from_bits(self.op_mode()?);
        self.mode = Some(m);
        Ok(m)
    }

    /// Set the transceiver mode (one register write, keeps CSS mode and LF bits).
    pub fn set_mode(&mut self, mode: Mode) -> R<(), SPI::Error> {
        self.write_register(Register::OpMode, self.opmode_base | mode as u8)?;
        self.mode = Some(mode);
        Ok(())
    }

    /// CSS mode sleep (lowest power; FIFO contents are lost).
    pub fn sleep(&mut self) -> R<(), SPI::Error> {
        self.set_mode(Mode::Sleep)
    }

    /// Standby.
    pub fn standby(&mut self) -> R<(), SPI::Error> {
        self.set_mode(Mode::Standby)
    }

    fn ensure_standby(&mut self) -> R<(), SPI::Error> {
        if self.mode != Some(Mode::Standby) {
            self.set_mode(Mode::Standby)?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- RF setup

    /// Set the carrier frequency (sleep or standby only). One 3-byte burst write;
    /// also switches the LF/HF register page if the band changes.
    pub fn set_frequency(&mut self, hz: u32) -> R<(), SPI::Error> {
        if !(config::MIN_FREQUENCY_HZ..=config::MAX_FREQUENCY_HZ).contains(&hz) {
            return Err(Error::Config(ConfigError::FrequencyOutOfRange));
        }
        self.write_burst(Register::FrfMsb, &frf_bytes(hz))?;
        let hf = hz >= config::LF_HF_SPLIT_HZ;
        let base = if hf {
            self.opmode_base & !opmode::LOW_FREQUENCY_MODE_ON
        } else {
            self.opmode_base | opmode::LOW_FREQUENCY_MODE_ON
        };
        self.high_frequency = hf;
        if base != self.opmode_base {
            self.opmode_base = base;
            let mode = self.mode.unwrap_or(Mode::Sleep);
            self.set_mode(mode)?;
        }
        Ok(())
    }

    /// Carrier frequency in Hz as read back from `RegFrf`.
    pub fn frequency(&mut self) -> R<u32, SPI::Error> {
        let mut b = [0u8; 3];
        self.read_burst(Register::FrfMsb, &mut b)?;
        Ok(hz_from_frf(
            ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32,
        ))
    }

    /// Apply a Cssm configuration (sleep or standby).
    pub fn configure(&mut self, cfg: &CssConfig) -> R<(), SPI::Error> {
        cfg.validate()?;
        let payload_len = match cfg.header {
            HeaderMode::Implicit { payload_len } => payload_len,
            HeaderMode::Explicit => 0xFF,
        };
        let [pre_msb, pre_lsb] = cfg.preamble_bytes();
        // 0x1D..=0x24 in one burst: MC1, MC2, SymbTimeoutLsb, PreambleMsb/Lsb,
        // PayloadLength, MaxPayloadLength, HopPeriod (0 = no FHSS).
        self.write_burst(
            Register::ModemConfig1,
            &[
                cfg.modem_config1(),
                cfg.modem_config2(),
                cfg.symbol_timeout_lsb(),
                pre_msb,
                pre_lsb,
                payload_len,
                0xFF,
                0x00,
            ],
        )?;
        self.write_register(Register::ModemConfig3, cfg.modem_config3())?;
        self.modify_register(Register::DetectOptimize, 0x07, cfg.detect_optimize())?;
        self.write_register(Register::DetectionThreshold, cfg.detection_threshold())?;
        self.write_register(Register::SyncWord, cfg.sync_word)?;
        self.config = *cfg;
        Ok(())
    }

    /// Set output power. Only affects transmissions; nothing is sent.
    pub fn set_tx_power(&mut self, output: PaOutput, dbm: i8) -> R<(), SPI::Error> {
        let s = encode_pa(output, dbm)?;
        self.write_register(Register::PaConfig, s.pa_config)?;
        self.write_register(Register::PaDac, s.pa_dac)?;
        self.set_ocp(s.ocp_ma)
    }

    /// Set the PA over-current protection limit in mA (45..=240).
    pub fn set_ocp(&mut self, ma: u16) -> R<(), SPI::Error> {
        let v = encode_ocp(ma)?;
        self.write_register(Register::Ocp, v)
    }

    /// Route a function to DIO0 (skipped if already mapped).
    pub fn set_dio0(&mut self, map: Dio0Mapping) -> R<(), SPI::Error> {
        let current = match self.dio_mapping1 {
            Some(v) => v,
            None => self.read_register(Register::DioMapping1)?,
        };
        let new = map.apply(current);
        if Some(new) != self.dio_mapping1 {
            self.write_register(Register::DioMapping1, new)?;
            self.dio_mapping1 = Some(new);
        }
        Ok(())
    }

    // ---------------------------------------------------------------- IRQ

    /// Read `RegIrqFlags`.
    pub fn irq_flags(&mut self) -> R<Irq, SPI::Error> {
        Ok(Irq(self.read_register(Register::IrqFlags)?))
    }

    /// Clear IRQ flags (write-1-to-clear).
    pub fn clear_irq(&mut self, flags: Irq) -> R<(), SPI::Error> {
        self.write_register(Register::IrqFlags, flags.bits())
    }

    /// Wait for DIO0 to assert, up to `timeout_us`. Returns false on timeout.
    pub fn wait_dio0(&mut self, timeout_us: u32) -> R<bool, SPI::Error> {
        self.irq.wait_asserted(timeout_us).map_err(|_| Error::Irq)
    }

    // ---------------------------------------------------------------- TX

    /// Load `payload` and start a transmission. Returns immediately; DIO0 rises on TxDone.
    ///
    /// SPI cost: standby (if needed), FifoAddrPtr, FIFO burst, PayloadLength,
    /// DIO mapping (first time only), mode TX.
    pub fn start_transmit(&mut self, payload: &[u8]) -> R<(), SPI::Error> {
        if payload.is_empty() || payload.len() > MAX_PAYLOAD {
            return Err(Error::PayloadSize);
        }
        self.ensure_standby()?;
        self.write_register(Register::FifoAddrPtr, 0x00)?;
        self.write_burst(Register::Fifo, payload)?;
        self.write_register(Register::PayloadLength, payload.len() as u8)?;
        self.set_dio0(Dio0Mapping::TxDone)?;
        self.set_mode(Mode::Tx)
    }

    /// Call after DIO0 rose for TxDone: clears the flag. Returns true if TX completed.
    pub fn finish_transmit(&mut self) -> R<bool, SPI::Error> {
        let flags = self.irq_flags()?;
        if flags.contains(Irq::TX_DONE) {
            self.clear_irq(Irq::TX_DONE)?;
            self.mode = Some(Mode::Standby); // chip returns to standby by itself
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Transmit and block on DIO0 until TxDone or `timeout_us`.
    pub fn transmit(&mut self, payload: &[u8], timeout_us: u32) -> R<(), SPI::Error> {
        self.start_transmit(payload)?;
        if self.wait_dio0(timeout_us)? && self.finish_transmit()? {
            return Ok(());
        }
        self.standby()?;
        Err(Error::Timeout)
    }

    // ---------------------------------------------------------------- RX

    /// Enter receive mode with DIO0 mapped to RxDone.
    pub fn start_receive(&mut self, mode: RxMode) -> R<(), SPI::Error> {
        self.ensure_standby()?;
        self.write_register(Register::FifoAddrPtr, 0x00)?;
        self.set_dio0(Dio0Mapping::RxDone)?;
        self.clear_irq(Irq::ALL)?;
        self.set_mode(match mode {
            RxMode::Continuous => Mode::RxContinuous,
            RxMode::Single => Mode::RxSingle,
        })
    }

    /// Non-blocking: if a packet is waiting, copy it into `buf`.
    ///
    /// SPI cost per packet: one 11-byte burst of 0x10..=0x1A (FIFO address,
    /// IRQ flags, length, SNR, RSSI), FifoAddrPtr, FIFO burst, IRQ clear.
    /// With no packet pending only the first burst is done.
    pub fn read_packet(&mut self, buf: &mut [u8]) -> R<Option<PacketInfo>, SPI::Error> {
        let mut st = [0u8; 11];
        self.read_burst(Register::FifoRxCurrentAddr, &mut st)?;
        let (rx_addr, flags, len, snr, rssi) =
            (st[0], Irq(st[2]), st[3] as usize, st[9] as i8, st[10]);
        if flags.contains(Irq::RX_TIMEOUT) {
            self.clear_irq(Irq::RX_TIMEOUT)?;
            self.mode = Some(Mode::Standby);
            return Err(Error::Timeout);
        }
        if !flags.contains(Irq::RX_DONE) {
            return Ok(None);
        }
        if self.mode == Some(Mode::RxSingle) {
            self.mode = Some(Mode::Standby);
        }
        if flags.contains(Irq::PAYLOAD_CRC_ERROR) {
            self.clear_irq(Irq::RX_DONE | Irq::PAYLOAD_CRC_ERROR | Irq::VALID_HEADER)?;
            return Err(Error::Crc);
        }
        if len > buf.len() {
            self.clear_irq(Irq::RX_DONE | Irq::VALID_HEADER)?;
            return Err(Error::PayloadSize);
        }
        self.write_register(Register::FifoAddrPtr, rx_addr)?;
        self.read_burst(Register::Fifo, &mut buf[..len])?;
        self.clear_irq(Irq::RX_DONE | Irq::VALID_HEADER)?;
        Ok(Some(PacketInfo {
            len,
            rssi_dbm: packet_rssi_dbm(rssi, snr, self.high_frequency),
            snr_quarter_db: snr,
        }))
    }

    /// Block on DIO0 (RxDone) up to `timeout_us`, then read the packet.
    /// Call [`start_receive`](Self::start_receive) first. `Ok(None)` on timeout.
    pub fn receive(
        &mut self,
        buf: &mut [u8],
        timeout_us: u32,
    ) -> R<Option<PacketInfo>, SPI::Error> {
        if !self.wait_dio0(timeout_us)? {
            // RX single symbol timeouts do not raise DIO0 (mapped to RxDone): check flags.
            return match self.read_packet(buf) {
                Err(Error::Timeout) => Ok(None),
                other => other,
            };
        }
        self.read_packet(buf)
    }

    /// Instantaneous RSSI in dBm (valid in RX).
    pub fn rssi(&mut self) -> R<i16, SPI::Error> {
        Ok(rssi_dbm(
            self.read_register(Register::RssiValue)?,
            self.high_frequency,
        ))
    }

    /// Number of valid packets received since entering RX (`RegRxPacketCntValue`).
    pub fn rx_packet_count(&mut self) -> R<u16, SPI::Error> {
        let mut b = [0u8; 2];
        self.read_burst(Register::RxPacketCntValueMsb, &mut b)?;
        Ok(u16::from_be_bytes(b))
    }

    // ---------------------------------------------------------------- CAD

    /// Start channel activity detection (receive-side; never transmits).
    pub fn start_cad(&mut self) -> R<(), SPI::Error> {
        self.ensure_standby()?;
        self.set_dio0(Dio0Mapping::CadDone)?;
        self.clear_irq(Irq::ALL)?;
        self.set_mode(Mode::Cad)
    }

    /// Call after CadDone: returns whether CSS mode activity was detected and clears the flags.
    pub fn finish_cad(&mut self) -> R<Option<bool>, SPI::Error> {
        let flags = self.irq_flags()?;
        if !flags.contains(Irq::CAD_DONE) {
            return Ok(None);
        }
        self.clear_irq(Irq::CAD_DONE | Irq::CAD_DETECTED)?;
        self.mode = Some(Mode::Standby);
        Ok(Some(flags.contains(Irq::CAD_DETECTED)))
    }

    /// Run one CAD and block on DIO0 for the result.
    pub fn cad(&mut self, timeout_us: u32) -> R<bool, SPI::Error> {
        self.start_cad()?;
        self.wait_dio0(timeout_us)?;
        match self.finish_cad()? {
            Some(detected) => Ok(detected),
            None => {
                self.standby()?;
                Err(Error::Timeout)
            }
        }
    }
}

#[cfg(test)]
mod payload_tests {
    use super::FskLongRx;

    #[test]
    fn default_payload_is_empty() {
        assert!(FskLongRx::default().payload().is_empty());
    }
}
