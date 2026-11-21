//! FSK packet-mode configuration (the SX1276's fast mode, up to 300 kbit/s).
//!
//! FSK registers alias the LoRa page addresses, so they are written by raw
//! address with LongRangeMode = 0. Everything here is pure encoding plus the
//! airtime model; [`crate::Sx1276::configure_fsk`] applies it.

use crate::config::FXOSC_HZ;

/// FSK-page register addresses used here.
#[allow(missing_docs)]
pub mod reg {
    pub const BITRATE_MSB: u8 = 0x02;
    pub const BITRATE_LSB: u8 = 0x03;
    pub const FDEV_MSB: u8 = 0x04;
    pub const FDEV_LSB: u8 = 0x05;
    pub const RX_BW: u8 = 0x12;
    pub const AFC_BW: u8 = 0x13;
    pub const PREAMBLE_MSB: u8 = 0x25;
    pub const PREAMBLE_LSB: u8 = 0x26;
    pub const SYNC_CONFIG: u8 = 0x27;
    pub const SYNC_VALUE1: u8 = 0x28;
    pub const PACKET_CONFIG1: u8 = 0x30;
    pub const PACKET_CONFIG2: u8 = 0x31;
    pub const PAYLOAD_LENGTH: u8 = 0x32;
    pub const FIFO_THRESH: u8 = 0x35;
    pub const BITRATE_FRAC: u8 = 0x5D;
    pub const RSSI_VALUE: u8 = 0x11;
    pub const IRQ_FLAGS1: u8 = 0x3E;
    pub const IRQ_FLAGS2: u8 = 0x3F;
}

/// `RegIrqFlags2` bits.
#[allow(missing_docs)]
pub mod irq2 {
    pub const FIFO_FULL: u8 = 0x80;
    pub const FIFO_EMPTY: u8 = 0x40;
    pub const FIFO_LEVEL: u8 = 0x20;
    pub const FIFO_OVERRUN: u8 = 0x10;
    pub const PACKET_SENT: u8 = 0x08;
    pub const PAYLOAD_READY: u8 = 0x04;
    pub const CRC_OK: u8 = 0x02;
}

/// FSK FIFO size; packets longer than this need FIFO refills during TX/RX.
pub const FSK_FIFO_SIZE: usize = 64;
/// FifoThreshold used for long-packet refill/drain (FifoLevel = FIFO holds more than this).
pub const FIFO_THRESHOLD: u8 = 32;
/// Frequency synthesizer step, Hz (Fxosc / 2^19 = 61.035 Hz), scaled by 1e3.
const FSTEP_MILLIHZ: u64 = FXOSC_HZ * 1000 / (1 << 19);

/// Errors from FSK parameter validation.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FskError {
    BitrateOutOfRange,
    FdevOutOfRange,
    /// Fdev + bitrate/2 must be <= 250 kHz (datasheet).
    TooWide,
    /// Modulation index 2*Fdev/bitrate must be >= 0.5.
    IndexTooLow,
    BadSyncWord,
}

/// FSK packet-mode settings.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FskConfig {
    pub bitrate_bps: u32,
    pub fdev_hz: u32,
    /// Preamble length in bytes.
    pub preamble_bytes: u16,
    /// Sync word, 1..=8 bytes (first `sync_len` bytes used).
    pub sync_word: [u8; 8],
    pub sync_len: u8,
    pub crc: bool,
    /// Data whitening (DC-free).
    pub whitening: bool,
    /// On a CRC failure the chip clears the FIFO and raises no PayloadReady (the datasheet
    /// default). `false` keeps the packet and raises PayloadReady with CrcOk clear, so a
    /// reader that drains the FIFO while the packet arrives
    /// ([`crate::Sx1276::fsk_poll_long`]) can't splice the bytes it already took onto the
    /// next packet; the driver then discards the failed packet whole.
    pub crc_auto_clear: bool,
    /// Stress testing only: skip the datasheet bitrate (<= 300 kbit/s), bandwidth and
    /// modulation-index limits (still requires 1.2..=600 kbit/s and Fdev <= 300 kHz).
    pub unchecked: bool,
}

impl Default for FskConfig {
    /// 300 kbit/s, Fdev 100 kHz (index 0.67), 4-byte preamble, 2-byte sync, CRC, whitening.
    fn default() -> Self {
        FskConfig {
            bitrate_bps: 300_000,
            fdev_hz: 100_000,
            preamble_bytes: 4,
            sync_word: [0x2D, 0xD4, 0, 0, 0, 0, 0, 0],
            sync_len: 2,
            crc: true,
            whitening: true,
            crc_auto_clear: true,
            unchecked: false,
        }
    }
}

/// `RegBitrate` (16 bits) and `RegBitRateFrac` (4 bits): BR = Fxosc / (reg + frac/16).
pub const fn bitrate_regs(bps: u32) -> (u16, u8) {
    let x16 = (FXOSC_HZ * 16 + bps as u64 / 2) / bps as u64; // rounded 16 * Fxosc / BR
    ((x16 >> 4) as u16, (x16 & 0x0F) as u8)
}

/// Bit rate actually produced by the register pair.
pub const fn bitrate_from_regs(reg: u16, frac: u8) -> u32 {
    ((FXOSC_HZ * 16) / (reg as u64 * 16 + frac as u64)) as u32
}

/// `RegFdev` (14 bits): Fdev = Fstep * reg, rounded.
pub const fn fdev_reg(hz: u32) -> u16 {
    (((hz as u64 * 1000 * 1000) + FSTEP_MILLIHZ * 500) / (FSTEP_MILLIHZ * 1000)) as u16 & 0x3FFF
}

/// Smallest single-sideband receiver bandwidth >= `hz`, as (`RegRxBw` value, bandwidth Hz).
/// RxBw = Fxosc / (RxBwMant * 2^(RxBwExp + 2)), Mant in {16, 20, 24}, Exp 1..=7.
pub fn rx_bw_reg(hz: u32) -> (u8, u32) {
    let mut best: Option<(u8, u32)> = None;
    for exp in 1u8..=7 {
        for (mbits, mant) in [(0u8, 16u64), (1, 20), (2, 24)] {
            let bw = (FXOSC_HZ / (mant << (exp + 2))) as u32;
            if bw >= hz && best.is_none_or(|(_, b)| bw < b) {
                best = Some(((mbits << 3) | exp, bw));
            }
        }
    }
    best.unwrap_or((0x01, 250_000)) // widest available: 250 kHz
}

impl FskConfig {
    /// Validate against the datasheet limits.
    pub fn validate(&self) -> Result<(), FskError> {
        if self.unchecked {
            if !(1_200..=600_000).contains(&self.bitrate_bps) {
                return Err(FskError::BitrateOutOfRange);
            }
            if !(600..=300_000).contains(&self.fdev_hz) {
                return Err(FskError::FdevOutOfRange);
            }
            if self.sync_len == 0
                || self.sync_len > 8
                || self.sync_word[..self.sync_len as usize].contains(&0)
            {
                return Err(FskError::BadSyncWord);
            }
            return Ok(());
        }
        if !(1_200..=300_000).contains(&self.bitrate_bps) {
            return Err(FskError::BitrateOutOfRange);
        }
        if !(600..=200_000).contains(&self.fdev_hz) {
            return Err(FskError::FdevOutOfRange);
        }
        if self.fdev_hz + self.bitrate_bps / 2 > 250_000 {
            return Err(FskError::TooWide);
        }
        if 4 * self.fdev_hz < self.bitrate_bps {
            return Err(FskError::IndexTooLow);
        }
        if self.sync_len == 0
            || self.sync_len > 8
            || self.sync_word[..self.sync_len as usize].contains(&0)
        {
            return Err(FskError::BadSyncWord);
        }
        Ok(())
    }

    /// Receiver bandwidth register for this config (Fdev + BR/2, single sideband).
    pub fn rx_bw(&self) -> (u8, u32) {
        rx_bw_reg(self.fdev_hz + self.bitrate_bps / 2)
    }

    /// `RegSyncConfig`: AutoRestartRx on without PLL re-lock wait (fixed channel, fastest
    /// restart between back-to-back packets) | SyncOn | SyncSize.
    pub const fn sync_config(&self) -> u8 {
        0x40 | 0x10 | (self.sync_len - 1)
    }

    /// `RegPacketConfig1`: variable length | DC-free whitening | CrcOn | CrcAutoClearOff.
    pub const fn packet_config1(&self) -> u8 {
        0x80 | if self.whitening { 0x40 } else { 0 }
            | if self.crc { 0x10 } else { 0 }
            | if self.crc && !self.crc_auto_clear {
                0x08
            } else {
                0
            }
    }

    /// True when the driver must check CrcOk itself on PayloadReady.
    pub const fn checks_crc_on_read(&self) -> bool {
        self.crc && !self.crc_auto_clear
    }

    /// `RegPacketConfig2`: packet mode.
    pub const fn packet_config2(&self) -> u8 {
        0x40
    }

    /// Bytes on air for a `len`-byte payload (variable length format).
    pub const fn frame_bytes(&self, len: usize) -> usize {
        self.preamble_bytes as usize
            + self.sync_len as usize
            + 1
            + len
            + if self.crc { 2 } else { 0 }
    }

    /// Time on air in microseconds.
    pub const fn time_on_air_us(&self, len: usize) -> u32 {
        ((self.frame_bytes(len) as u64 * 8 * 1_000_000) / self.bitrate_bps as u64) as u32
    }
}
