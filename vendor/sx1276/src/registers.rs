//! Typed CSS-mode register map of the SX1276 (datasheet rev. 7, section 6.4).
//!
//! Only the CSS mode page is modelled; FSK/OOK registers that alias the same
//! addresses are not exposed.

/// CSS-mode register addresses.
#[allow(missing_docs)]
#[repr(u8)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Register {
    Fifo = 0x00,
    OpMode = 0x01,
    FrfMsb = 0x06,
    FrfMid = 0x07,
    FrfLsb = 0x08,
    PaConfig = 0x09,
    PaRamp = 0x0A,
    Ocp = 0x0B,
    Lna = 0x0C,
    FifoAddrPtr = 0x0D,
    FifoTxBaseAddr = 0x0E,
    FifoRxBaseAddr = 0x0F,
    FifoRxCurrentAddr = 0x10,
    IrqFlagsMask = 0x11,
    IrqFlags = 0x12,
    RxNbBytes = 0x13,
    RxHeaderCntValueMsb = 0x14,
    RxHeaderCntValueLsb = 0x15,
    RxPacketCntValueMsb = 0x16,
    RxPacketCntValueLsb = 0x17,
    ModemStat = 0x18,
    PktSnrValue = 0x19,
    PktRssiValue = 0x1A,
    RssiValue = 0x1B,
    HopChannel = 0x1C,
    ModemConfig1 = 0x1D,
    ModemConfig2 = 0x1E,
    SymbTimeoutLsb = 0x1F,
    PreambleMsb = 0x20,
    PreambleLsb = 0x21,
    PayloadLength = 0x22,
    MaxPayloadLength = 0x23,
    HopPeriod = 0x24,
    FifoRxByteAddr = 0x25,
    ModemConfig3 = 0x26,
    PpmCorrection = 0x27,
    FeiMsb = 0x28,
    FeiMid = 0x29,
    FeiLsb = 0x2A,
    RssiWideband = 0x2C,
    DetectOptimize = 0x31,
    InvertIq = 0x33,
    DetectionThreshold = 0x37,
    SyncWord = 0x39,
    InvertIq2 = 0x3B,
    DioMapping1 = 0x40,
    DioMapping2 = 0x41,
    Version = 0x42,
    Tcxo = 0x4B,
    PaDac = 0x4D,
    FormerTemp = 0x5B,
    AgcRef = 0x61,
    AgcThresh1 = 0x62,
    AgcThresh2 = 0x63,
    AgcThresh3 = 0x64,
    PllHop = 0x70,
}

impl Register {
    /// Register address (7 bits).
    #[inline]
    pub const fn addr(self) -> u8 {
        self as u8
    }
}

/// SPI address byte: MSB set = write access, clear = read access.
pub const WRITE_BIT: u8 = 0x80;

/// Expected `RegVersion` for SX1276/77/78/79.
pub const SILICON_VERSION: u8 = 0x12;

/// Size of the shared TX/RX FIFO in bytes.
pub const FIFO_SIZE: usize = 256;

/// `RegOpMode` bit fields.
pub mod opmode {
    /// LongRangeMode: 1 = CSS mode (only writable in sleep).
    pub const LONG_RANGE_MODE: u8 = 0x80;
    /// AccessSharedReg: 1 = access FSK registers 0x0D..0x3F while in CSS mode.
    pub const ACCESS_SHARED_REG: u8 = 0x40;
    /// LowFrequencyModeOn: 1 = LF register page (band 3, < 525 MHz).
    pub const LOW_FREQUENCY_MODE_ON: u8 = 0x08;
    /// Mask of the three Mode bits.
    pub const MODE_MASK: u8 = 0x07;
}

/// Transceiver mode (`RegOpMode` bits 2..0).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// Sleep: lowest power, FIFO not accessible.
    Sleep = 0,
    /// Standby: crystal oscillator running.
    Standby = 1,
    /// Frequency synthesis, TX.
    FsTx = 2,
    /// Transmit a single packet, then back to standby.
    Tx = 3,
    /// Frequency synthesis, RX.
    FsRx = 4,
    /// Receive continuously.
    RxContinuous = 5,
    /// Receive a single packet (or until symbol timeout), then standby.
    RxSingle = 6,
    /// Channel activity detection.
    Cad = 7,
}

impl Mode {
    /// Decode the mode bits of a `RegOpMode` value.
    pub const fn from_bits(bits: u8) -> Mode {
        match bits & opmode::MODE_MASK {
            0 => Mode::Sleep,
            1 => Mode::Standby,
            2 => Mode::FsTx,
            3 => Mode::Tx,
            4 => Mode::FsRx,
            5 => Mode::RxContinuous,
            6 => Mode::RxSingle,
            _ => Mode::Cad,
        }
    }
}

/// `RegIrqFlags` / `RegIrqFlagsMask` (write 1 to clear a flag).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Irq(pub u8);

impl Irq {
    pub const RX_TIMEOUT: Irq = Irq(0x80);
    pub const RX_DONE: Irq = Irq(0x40);
    pub const PAYLOAD_CRC_ERROR: Irq = Irq(0x20);
    pub const VALID_HEADER: Irq = Irq(0x10);
    pub const TX_DONE: Irq = Irq(0x08);
    pub const CAD_DONE: Irq = Irq(0x04);
    pub const FHSS_CHANGE_CHANNEL: Irq = Irq(0x02);
    pub const CAD_DETECTED: Irq = Irq(0x01);
    /// All eight flags.
    pub const ALL: Irq = Irq(0xFF);
    /// No flags.
    pub const NONE: Irq = Irq(0x00);

    /// Raw register value.
    #[inline]
    pub const fn bits(self) -> u8 {
        self.0
    }
    /// True if every flag in `other` is set.
    #[inline]
    pub const fn contains(self, other: Irq) -> bool {
        self.0 & other.0 == other.0
    }
    /// True if any flag in `other` is set.
    #[inline]
    pub const fn intersects(self, other: Irq) -> bool {
        self.0 & other.0 != 0
    }
    /// True if no flag is set.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl core::ops::BitOr for Irq {
    type Output = Irq;
    fn bitor(self, rhs: Irq) -> Irq {
        Irq(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for Irq {
    type Output = Irq;
    fn bitand(self, rhs: Irq) -> Irq {
        Irq(self.0 & rhs.0)
    }
}

/// Function routed to the DIO0 pin in CSS mode (`RegDioMapping1` bits 7..6).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Dio0Mapping {
    RxDone = 0b00,
    TxDone = 0b01,
    CadDone = 0b10,
}

impl Dio0Mapping {
    /// Merge this mapping into an existing `RegDioMapping1` value.
    pub const fn apply(self, reg: u8) -> u8 {
        (reg & 0x3F) | ((self as u8) << 6)
    }
}

/// `RegPaConfig` bit fields.
pub mod pa {
    /// PaSelect: 1 = PA_BOOST pin (up to +20 dBm), 0 = RFO pin (up to +14/15 dBm).
    pub const PA_SELECT_BOOST: u8 = 0x80;
    /// `RegPaDac` default (+17 dBm max on PA_BOOST).
    pub const PA_DAC_DEFAULT: u8 = 0x84;
    /// `RegPaDac` high power (+20 dBm on PA_BOOST, max 1% duty cycle).
    pub const PA_DAC_HIGH_POWER: u8 = 0x87;
}

/// `RegOcp` bit fields.
pub mod ocp {
    /// OcpOn.
    pub const OCP_ON: u8 = 0x20;
}

/// `RegLna` values.
pub mod lna {
    /// G1 (max gain) with LnaBoostHf = 150% current (recommended for HF band).
    pub const MAX_GAIN_BOOST_HF: u8 = 0x23;
}

/// `RegModemConfig3` bit fields.
pub mod modem3 {
    pub const LOW_DATA_RATE_OPTIMIZE: u8 = 0x08;
    pub const AGC_AUTO_ON: u8 = 0x04;
}

/// `RegHopChannel` bit fields.
pub mod hop_channel {
    /// CrcOnPayload: CRC information extracted from the received header.
    pub const CRC_ON_PAYLOAD: u8 = 0x40;
}

/// `RegModemStat` bit fields.
pub mod modem_stat {
    pub const MODEM_CLEAR: u8 = 0x10;
    pub const HEADER_INFO_VALID: u8 = 0x08;
    pub const RX_ONGOING: u8 = 0x04;
    pub const SIGNAL_SYNCHRONIZED: u8 = 0x02;
    pub const SIGNAL_DETECTED: u8 = 0x01;
}
