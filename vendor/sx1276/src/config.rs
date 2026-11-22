//! Modem configuration types and their register encodings.
//!
//! Everything in here is pure (no bus access) so it can be unit tested on the host.

use crate::registers::{modem3, pa};

/// Crystal oscillator frequency of the SX1276 (32 MHz).
pub const FXOSC_HZ: u64 = 32_000_000;

/// Lowest carrier frequency supported by the SX1276 (band 3).
pub const MIN_FREQUENCY_HZ: u32 = 137_000_000;
/// Highest carrier frequency supported by the SX1276 (band 1).
pub const MAX_FREQUENCY_HZ: u32 = 1_020_000_000;
/// Frequencies below this use the low-frequency register page (`LowFrequencyModeOn`).
pub const LF_HF_SPLIT_HZ: u32 = 525_000_000;

/// Convert a carrier frequency in Hz to the 24-bit `RegFrf` value,
/// `Frf = f * 2^19 / Fxosc`, rounded to the nearest step (61.035 Hz).
pub const fn frf_from_hz(hz: u32) -> u32 {
    ((((hz as u64) << 19) + FXOSC_HZ / 2) / FXOSC_HZ) as u32
}

/// Convert a 24-bit `RegFrf` value back to Hz (rounded to the nearest Hz).
pub const fn hz_from_frf(frf: u32) -> u32 {
    (((frf as u64 & 0xFF_FFFF) * FXOSC_HZ + (1 << 18)) >> 19) as u32
}

/// `RegFrfMsb`, `RegFrfMid`, `RegFrfLsb` bytes for a frequency.
pub const fn frf_bytes(hz: u32) -> [u8; 3] {
    let frf = frf_from_hz(hz);
    [(frf >> 16) as u8, (frf >> 8) as u8, frf as u8]
}

/// Signal bandwidth (`RegModemConfig1` bits 7..4).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Bandwidth {
    Bw7_8kHz = 0,
    Bw10_4kHz = 1,
    Bw15_6kHz = 2,
    Bw20_8kHz = 3,
    Bw31_25kHz = 4,
    Bw41_7kHz = 5,
    Bw62_5kHz = 6,
    Bw125kHz = 7,
    Bw250kHz = 8,
    Bw500kHz = 9,
}

impl Bandwidth {
    /// Nominal bandwidth in Hz.
    pub const fn hz(self) -> u32 {
        match self {
            Bandwidth::Bw7_8kHz => 7_800,
            Bandwidth::Bw10_4kHz => 10_400,
            Bandwidth::Bw15_6kHz => 15_600,
            Bandwidth::Bw20_8kHz => 20_800,
            Bandwidth::Bw31_25kHz => 31_250,
            Bandwidth::Bw41_7kHz => 41_700,
            Bandwidth::Bw62_5kHz => 62_500,
            Bandwidth::Bw125kHz => 125_000,
            Bandwidth::Bw250kHz => 250_000,
            Bandwidth::Bw500kHz => 500_000,
        }
    }

    /// Look up a bandwidth by its nominal value in Hz.
    pub const fn from_hz(hz: u32) -> Option<Bandwidth> {
        Some(match hz {
            7_800 => Bandwidth::Bw7_8kHz,
            10_400 => Bandwidth::Bw10_4kHz,
            15_600 => Bandwidth::Bw15_6kHz,
            20_800 => Bandwidth::Bw20_8kHz,
            31_250 => Bandwidth::Bw31_25kHz,
            41_700 => Bandwidth::Bw41_7kHz,
            62_500 => Bandwidth::Bw62_5kHz,
            125_000 => Bandwidth::Bw125kHz,
            250_000 => Bandwidth::Bw250kHz,
            500_000 => Bandwidth::Bw500kHz,
            _ => return None,
        })
    }

    /// Decode `RegModemConfig1` bits 7..4.
    pub const fn from_bits(bits: u8) -> Option<Bandwidth> {
        Some(match bits {
            0 => Bandwidth::Bw7_8kHz,
            1 => Bandwidth::Bw10_4kHz,
            2 => Bandwidth::Bw15_6kHz,
            3 => Bandwidth::Bw20_8kHz,
            4 => Bandwidth::Bw31_25kHz,
            5 => Bandwidth::Bw41_7kHz,
            6 => Bandwidth::Bw62_5kHz,
            7 => Bandwidth::Bw125kHz,
            8 => Bandwidth::Bw250kHz,
            9 => Bandwidth::Bw500kHz,
            _ => return None,
        })
    }
}

/// Spreading factor (`RegModemConfig2` bits 7..4), chips per symbol = 2^SF.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SpreadingFactor {
    Sf6 = 6,
    Sf7 = 7,
    Sf8 = 8,
    Sf9 = 9,
    Sf10 = 10,
    Sf11 = 11,
    Sf12 = 12,
}

impl SpreadingFactor {
    /// Build from the numeric spreading factor (6..=12).
    pub const fn from_u8(sf: u8) -> Option<SpreadingFactor> {
        Some(match sf {
            6 => SpreadingFactor::Sf6,
            7 => SpreadingFactor::Sf7,
            8 => SpreadingFactor::Sf8,
            9 => SpreadingFactor::Sf9,
            10 => SpreadingFactor::Sf10,
            11 => SpreadingFactor::Sf11,
            12 => SpreadingFactor::Sf12,
            _ => return None,
        })
    }
    /// Numeric spreading factor.
    pub const fn value(self) -> u8 {
        self as u8
    }
}

/// Forward error correction coding rate (`RegModemConfig1` bits 3..1).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CodingRate {
    Cr4_5 = 1,
    Cr4_6 = 2,
    Cr4_7 = 3,
    Cr4_8 = 4,
}

impl CodingRate {
    /// Build from the denominator of 4/x (5..=8).
    pub const fn from_denominator(d: u8) -> Option<CodingRate> {
        Some(match d {
            5 => CodingRate::Cr4_5,
            6 => CodingRate::Cr4_6,
            7 => CodingRate::Cr4_7,
            8 => CodingRate::Cr4_8,
            _ => return None,
        })
    }
}

/// Packet header mode.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HeaderMode {
    /// Explicit header: length, coding rate and CRC presence are sent in the header.
    Explicit,
    /// Implicit header: fixed payload length known to both sides (required for SF6).
    Implicit {
        /// Fixed payload length in bytes (1..=255).
        payload_len: u8,
    },
}

/// Low data rate optimisation setting.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum LowDataRateOptimize {
    /// Enable when the symbol time exceeds 16 ms (datasheet recommendation).
    Auto,
    On,
    Off,
}

/// Cssm configuration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct CssConfig {
    pub bandwidth: Bandwidth,
    pub spreading_factor: SpreadingFactor,
    pub coding_rate: CodingRate,
    /// Payload CRC generation (TX) / checking requested (RX, explicit header carries it).
    pub crc: bool,
    /// Preamble length in symbols (the modem adds 4.25 symbols).
    pub preamble_len: u16,
    /// Sync word: 0x12 private networks (default), 0x34 framed public.
    pub sync_word: u8,
    pub header: HeaderMode,
    pub low_data_rate_optimize: LowDataRateOptimize,
    /// RX single symbol timeout (10 bits, symbols).
    pub symbol_timeout: u16,
}

impl Default for CssConfig {
    fn default() -> Self {
        CssConfig {
            bandwidth: Bandwidth::Bw125kHz,
            spreading_factor: SpreadingFactor::Sf7,
            coding_rate: CodingRate::Cr4_5,
            crc: true,
            preamble_len: 8,
            sync_word: 0x12,
            header: HeaderMode::Explicit,
            low_data_rate_optimize: LowDataRateOptimize::Auto,
            symbol_timeout: 0x64,
        }
    }
}

impl CssConfig {
    /// Symbol duration in microseconds: 2^SF / BW.
    pub const fn symbol_time_us(&self) -> u32 {
        (((1u64 << self.spreading_factor.value()) * 1_000_000) / self.bandwidth.hz() as u64) as u32
    }

    /// Whether LowDataRateOptimize will be enabled.
    pub const fn ldro_enabled(&self) -> bool {
        match self.low_data_rate_optimize {
            LowDataRateOptimize::On => true,
            LowDataRateOptimize::Off => false,
            LowDataRateOptimize::Auto => self.symbol_time_us() > 16_000,
        }
    }

    /// Check for combinations the radio does not support.
    pub const fn validate(&self) -> Result<(), ConfigError> {
        if matches!(self.spreading_factor, SpreadingFactor::Sf6)
            && matches!(self.header, HeaderMode::Explicit)
        {
            return Err(ConfigError::Sf6RequiresImplicitHeader);
        }
        if let HeaderMode::Implicit { payload_len: 0 } = self.header {
            return Err(ConfigError::InvalidPayloadLength);
        }
        if self.symbol_timeout > 0x3FF || self.symbol_timeout < 4 {
            return Err(ConfigError::InvalidSymbolTimeout);
        }
        if self.preamble_len < 6 {
            return Err(ConfigError::PreambleTooShort);
        }
        Ok(())
    }

    /// `RegModemConfig1`: Bw(7..4) | CodingRate(3..1) | ImplicitHeaderModeOn(0).
    pub const fn modem_config1(&self) -> u8 {
        let implicit = matches!(self.header, HeaderMode::Implicit { .. }) as u8;
        ((self.bandwidth as u8) << 4) | ((self.coding_rate as u8) << 1) | implicit
    }

    /// `RegModemConfig2`: SpreadingFactor(7..4) | TxContinuousMode(3)=0 |
    /// RxPayloadCrcOn(2) | SymbTimeout(9..8)(1..0).
    pub const fn modem_config2(&self) -> u8 {
        ((self.spreading_factor.value()) << 4)
            | ((self.crc as u8) << 2)
            | ((self.symbol_timeout >> 8) as u8 & 0x03)
    }

    /// `RegSymbTimeoutLsb`.
    pub const fn symbol_timeout_lsb(&self) -> u8 {
        self.symbol_timeout as u8
    }

    /// `RegModemConfig3`: LowDataRateOptimize(3) | AgcAutoOn(2) (always on).
    pub const fn modem_config3(&self) -> u8 {
        let ldro = if self.ldro_enabled() {
            modem3::LOW_DATA_RATE_OPTIMIZE
        } else {
            0
        };
        ldro | modem3::AGC_AUTO_ON
    }

    /// Low 3 bits of `RegDetectOptimize` (0x05 for SF6, 0x03 otherwise).
    pub const fn detect_optimize(&self) -> u8 {
        if matches!(self.spreading_factor, SpreadingFactor::Sf6) {
            0x05
        } else {
            0x03
        }
    }

    /// `RegDetectionThreshold` (0x0C for SF6, 0x0A otherwise).
    pub const fn detection_threshold(&self) -> u8 {
        if matches!(self.spreading_factor, SpreadingFactor::Sf6) {
            0x0C
        } else {
            0x0A
        }
    }

    /// `RegPreambleMsb`, `RegPreambleLsb`.
    pub const fn preamble_bytes(&self) -> [u8; 2] {
        [(self.preamble_len >> 8) as u8, self.preamble_len as u8]
    }

    /// Decode `RegModemConfig1/2/3` back into the fields they carry.
    /// Preamble, sync word and the implicit payload length live in other registers
    /// and are taken from `rest`.
    pub fn from_registers(mc1: u8, mc2: u8, mc3: u8, rest: &CssConfig) -> Option<CssConfig> {
        let implicit = mc1 & 0x01 != 0;
        Some(CssConfig {
            bandwidth: Bandwidth::from_bits(mc1 >> 4)?,
            coding_rate: match (mc1 >> 1) & 0x07 {
                1 => CodingRate::Cr4_5,
                2 => CodingRate::Cr4_6,
                3 => CodingRate::Cr4_7,
                4 => CodingRate::Cr4_8,
                _ => return None,
            },
            header: if implicit {
                match rest.header {
                    HeaderMode::Implicit { payload_len } => HeaderMode::Implicit { payload_len },
                    HeaderMode::Explicit => HeaderMode::Implicit { payload_len: 1 },
                }
            } else {
                HeaderMode::Explicit
            },
            spreading_factor: SpreadingFactor::from_u8(mc2 >> 4)?,
            crc: mc2 & 0x04 != 0,
            symbol_timeout: (((mc2 & 0x03) as u16) << 8) | rest.symbol_timeout & 0xFF,
            low_data_rate_optimize: if mc3 & modem3::LOW_DATA_RATE_OPTIMIZE != 0 {
                LowDataRateOptimize::On
            } else {
                LowDataRateOptimize::Off
            },
            ..*rest
        })
    }
}

/// Invalid modem or power configuration.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Sf6RequiresImplicitHeader,
    InvalidPayloadLength,
    InvalidSymbolTimeout,
    PreambleTooShort,
    FrequencyOutOfRange,
    PowerOutOfRange,
    OcpOutOfRange,
}

/// Which PA output pin the antenna is connected to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PaOutput {
    /// PA_BOOST pin: +2..=+20 dBm (sub-GHz CSS transceiver modules / the SX1276 GPS HAT use this).
    PaBoost,
    /// RFO pin: -4..=+15 dBm.
    Rfo,
}

/// Encoded power settings: `RegPaConfig`, `RegPaDac` and a suggested `RegOcp` trim (mA).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PaSettings {
    pub pa_config: u8,
    pub pa_dac: u8,
    pub ocp_ma: u16,
}

/// Encode an output power request.
///
/// * PA_BOOST: `Pout = 17 - (15 - OutputPower)` (2..=17 dBm); 18..=20 dBm use the
///   high-power PA DAC where `Pout = 20 - (15 - OutputPower)`.
/// * RFO: `Pmax = 10.8 + 0.6 * MaxPower`, `Pout = Pmax - (15 - OutputPower)`;
///   MaxPower = 7 (Pmax 15 dBm) for 0..=15 dBm, MaxPower = 0 (~11 dBm) for -4..=-1 dBm.
pub const fn encode_pa(output: PaOutput, dbm: i8) -> Result<PaSettings, ConfigError> {
    match output {
        PaOutput::PaBoost => {
            if dbm < 2 || dbm > 20 {
                return Err(ConfigError::PowerOutOfRange);
            }
            if dbm > 17 {
                Ok(PaSettings {
                    pa_config: pa::PA_SELECT_BOOST | 0x70 | (dbm - 5) as u8,
                    pa_dac: pa::PA_DAC_HIGH_POWER,
                    ocp_ma: 140,
                })
            } else {
                Ok(PaSettings {
                    pa_config: pa::PA_SELECT_BOOST | 0x70 | (dbm - 2) as u8,
                    pa_dac: pa::PA_DAC_DEFAULT,
                    ocp_ma: 100,
                })
            }
        }
        PaOutput::Rfo => {
            if dbm < -4 || dbm > 15 {
                return Err(ConfigError::PowerOutOfRange);
            }
            let pa_config = if dbm >= 0 {
                0x70 | dbm as u8
            } else {
                (dbm + 4) as u8
            };
            Ok(PaSettings {
                pa_config,
                pa_dac: pa::PA_DAC_DEFAULT,
                ocp_ma: 100,
            })
        }
    }
}

/// Encode `RegOcp` for a current limit in mA (45..=240), OCP enabled.
///
/// `Imax = 45 + 5 * OcpTrim` for trim 0..=15, `Imax = -30 + 10 * OcpTrim` for 16..=27.
/// Values between steps round down to the next lower supported limit.
pub const fn encode_ocp(ma: u16) -> Result<u8, ConfigError> {
    let trim = if ma < 45 || ma > 240 {
        return Err(ConfigError::OcpOutOfRange);
    } else if ma <= 120 {
        (ma - 45) / 5
    } else {
        (ma + 30) / 10
    };
    Ok(crate::registers::ocp::OCP_ON | trim as u8)
}

/// Packet RSSI in dBm from `RegPktRssiValue` and `RegPktSnrValue` (datasheet 5.5.5).
pub const fn packet_rssi_dbm(pkt_rssi: u8, pkt_snr: i8, high_frequency: bool) -> i16 {
    let offset: i16 = if high_frequency { -157 } else { -164 };
    let raw = pkt_rssi as i16;
    if pkt_snr >= 0 {
        offset + (raw * 16) / 15
    } else {
        offset + raw + (pkt_snr as i16) / 4
    }
}

/// Current (instantaneous) RSSI in dBm from `RegRssiValue`.
pub const fn rssi_dbm(rssi: u8, high_frequency: bool) -> i16 {
    let offset: i16 = if high_frequency { -157 } else { -164 };
    offset + rssi as i16
}

impl CssConfig {
    /// Number of payload symbols for `payload_len` bytes (datasheet 4.1.1.7).
    pub const fn payload_symbols(&self, payload_len: u8) -> u32 {
        let sf = self.spreading_factor.value() as i32;
        let crc = self.crc as i32;
        let ih = matches!(self.header, HeaderMode::Implicit { .. }) as i32;
        let de = self.ldro_enabled() as i32;
        let num = 8 * payload_len as i32 - 4 * sf + 28 + 16 * crc - 20 * ih;
        let den = 4 * (sf - 2 * de);
        let blocks = if num > 0 { (num + den - 1) / den } else { 0 };
        8 + (blocks * (self.coding_rate as i32 + 4)) as u32
    }

    /// Time on air of one packet in microseconds (preamble + header + payload).
    pub const fn time_on_air_us(&self, payload_len: u8) -> u32 {
        // Work in quarter symbols so the 4.25-symbol preamble tail stays exact.
        let quarter_symbols =
            (self.preamble_len as u64 * 4 + 17) + 4 * self.payload_symbols(payload_len) as u64;
        ((quarter_symbols << self.spreading_factor.value()) * 1_000_000
            / (4 * self.bandwidth.hz() as u64)) as u32
    }

    /// Raw PHY bit rate in bits/s: SF * BW / 2^SF * 4/(4+CR).
    pub const fn bit_rate_bps(&self) -> u32 {
        let sf = self.spreading_factor.value() as u64;
        ((sf * self.bandwidth.hz() as u64 * 4) / ((1u64 << sf) * (self.coding_rate as u64 + 4)))
            as u32
    }
}
