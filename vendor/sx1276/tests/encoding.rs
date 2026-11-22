//! Pure register-encoding tests (no hardware).
use sx1276::config::*;
use sx1276::registers::*;
use sx1276::*;

#[test]
fn frequency_math() {
    assert_eq!(frf_from_hz(915_000_000), 0xE4_C000);
    assert_eq!(frf_bytes(915_000_000), [0xE4, 0xC0, 0x00]);
    assert_eq!(frf_from_hz(868_100_000), 0xD9_0666);
    assert_eq!(frf_from_hz(433_000_000), 0x6C_4000);
    assert_eq!(frf_from_hz(434_000_000), 0x6C_8000); // datasheet reset value 0x6C8000
    assert_eq!(hz_from_frf(0xE4_C000), 915_000_000);
    assert_eq!(hz_from_frf(0x6C_8000), 434_000_000);
    // Round trip within half a step (61.035 Hz / 2).
    for hz in [
        137_000_000u32,
        433_175_000,
        868_300_000,
        902_300_000,
        927_500_000,
        1_020_000_000,
    ] {
        let back = hz_from_frf(frf_from_hz(hz));
        assert!((back as i64 - hz as i64).abs() <= 31, "{hz} -> {back}");
    }
    assert_eq!(frf_from_hz(915_000_061) - frf_from_hz(915_000_000), 1);
}

#[test]
fn modem_config_defaults() {
    let c = CssConfig::default();
    // BW125 (7) <<4 | CR4/5 (1) <<1 | explicit
    assert_eq!(c.modem_config1(), 0x72);
    // SF7 <<4 | CRC on | symbtimeout msb 0
    assert_eq!(c.modem_config2(), 0x74);
    assert_eq!(c.symbol_timeout_lsb(), 0x64);
    assert_eq!(c.modem_config3(), 0x04); // AGC auto, no LDRO
    assert_eq!(c.preamble_bytes(), [0x00, 0x08]);
    assert_eq!(c.detect_optimize(), 0x03);
    assert_eq!(c.detection_threshold(), 0x0A);
    assert!(c.validate().is_ok());
}

#[test]
fn modem_config_bitfields() {
    let c = CssConfig {
        bandwidth: Bandwidth::Bw500kHz,
        spreading_factor: SpreadingFactor::Sf12,
        coding_rate: CodingRate::Cr4_8,
        crc: false,
        header: HeaderMode::Implicit { payload_len: 32 },
        symbol_timeout: 0x3FF,
        preamble_len: 0x1234,
        ..Default::default()
    };
    assert_eq!(c.modem_config1(), 0x99);
    assert_eq!(c.modem_config2(), 0xC3);
    assert_eq!(c.symbol_timeout_lsb(), 0xFF);
    assert_eq!(c.preamble_bytes(), [0x12, 0x34]);
    // SF12/500k: Tsym = 8.192 ms -> no LDRO
    assert_eq!(c.symbol_time_us(), 8192);
    assert_eq!(c.modem_config3(), 0x04);
    let back =
        CssConfig::from_registers(c.modem_config1(), c.modem_config2(), c.modem_config3(), &c)
            .unwrap();
    assert_eq!(back.bandwidth, c.bandwidth);
    assert_eq!(back.spreading_factor, c.spreading_factor);
    assert_eq!(back.coding_rate, c.coding_rate);
    assert_eq!(back.crc, c.crc);
    assert_eq!(back.header, c.header);
    assert_eq!(back.symbol_timeout, 0x3FF);

    for (bw, bits) in [
        (Bandwidth::Bw7_8kHz, 0),
        (Bandwidth::Bw62_5kHz, 6),
        (Bandwidth::Bw250kHz, 8),
    ] {
        let c = CssConfig {
            bandwidth: bw,
            ..Default::default()
        };
        assert_eq!(c.modem_config1() >> 4, bits);
        assert_eq!(Bandwidth::from_bits(bits), Some(bw));
        assert_eq!(Bandwidth::from_hz(bw.hz()), Some(bw));
    }
    for sf in 6..=12u8 {
        let s = SpreadingFactor::from_u8(sf).unwrap();
        let c = CssConfig {
            spreading_factor: s,
            header: HeaderMode::Implicit { payload_len: 8 },
            ..Default::default()
        };
        assert_eq!(c.modem_config2() >> 4, sf);
    }
    assert_eq!(SpreadingFactor::from_u8(5), None);
    assert_eq!(CodingRate::from_denominator(7), Some(CodingRate::Cr4_7));
}

#[test]
fn ldro_auto() {
    let sf12 = CssConfig {
        spreading_factor: SpreadingFactor::Sf12,
        ..Default::default()
    };
    assert_eq!(sf12.symbol_time_us(), 32_768);
    assert!(sf12.ldro_enabled());
    assert_eq!(sf12.modem_config3(), 0x0C);
    let sf11_250 = CssConfig {
        spreading_factor: SpreadingFactor::Sf11,
        bandwidth: Bandwidth::Bw250kHz,
        ..Default::default()
    };
    assert!(!sf11_250.ldro_enabled());
    let forced = CssConfig {
        low_data_rate_optimize: LowDataRateOptimize::On,
        ..Default::default()
    };
    assert_eq!(forced.modem_config3(), 0x0C);
}

#[test]
fn sf6_rules() {
    let bad = CssConfig {
        spreading_factor: SpreadingFactor::Sf6,
        ..Default::default()
    };
    assert_eq!(bad.validate(), Err(ConfigError::Sf6RequiresImplicitHeader));
    let ok = CssConfig {
        header: HeaderMode::Implicit { payload_len: 16 },
        ..bad
    };
    assert!(ok.validate().is_ok());
    assert_eq!(ok.detect_optimize(), 0x05);
    assert_eq!(ok.detection_threshold(), 0x0C);
    assert_eq!(ok.modem_config1() & 1, 1);
    let zero = CssConfig {
        header: HeaderMode::Implicit { payload_len: 0 },
        ..Default::default()
    };
    assert_eq!(zero.validate(), Err(ConfigError::InvalidPayloadLength));
}

#[test]
fn time_on_air_and_rate() {
    // Semtech CSS mode calculator: SF7/125k/CR4-5, 8 preamble, explicit header, CRC, 10 B -> 41.216 ms
    let c = CssConfig::default();
    assert_eq!(c.payload_symbols(10), 28);
    assert_eq!(c.time_on_air_us(10), 41_216);
    assert_eq!(c.bit_rate_bps(), 5_468);
    // SF12/125k/CR4-5 + LDRO, 51 B -> 2465.792 ms
    let slow = CssConfig {
        spreading_factor: SpreadingFactor::Sf12,
        ..Default::default()
    };
    assert_eq!(slow.time_on_air_us(51), 2_465_792);
    // Fastest standard setting: SF7/500k, 255 B: 390.25 symbols x 256 us
    let fast = CssConfig {
        bandwidth: Bandwidth::Bw500kHz,
        ..Default::default()
    };
    assert_eq!(fast.bit_rate_bps(), 21_875);
    assert_eq!(fast.time_on_air_us(255), 99_904);
}

#[test]
fn pa_encoding() {
    let s = encode_pa(PaOutput::PaBoost, 17).unwrap();
    assert_eq!((s.pa_config, s.pa_dac), (0xFF, 0x84));
    let s = encode_pa(PaOutput::PaBoost, 2).unwrap();
    assert_eq!(s.pa_config, 0xF0);
    let s = encode_pa(PaOutput::PaBoost, 20).unwrap();
    assert_eq!((s.pa_config, s.pa_dac, s.ocp_ma), (0xFF, 0x87, 140));
    let s = encode_pa(PaOutput::PaBoost, 18).unwrap();
    assert_eq!(s.pa_config, 0xFD);
    assert_eq!(
        encode_pa(PaOutput::PaBoost, 1),
        Err(ConfigError::PowerOutOfRange)
    );
    assert_eq!(
        encode_pa(PaOutput::PaBoost, 21),
        Err(ConfigError::PowerOutOfRange)
    );
    assert_eq!(encode_pa(PaOutput::Rfo, 14).unwrap().pa_config, 0x7E);
    assert_eq!(encode_pa(PaOutput::Rfo, -4).unwrap().pa_config, 0x00);
    assert_eq!(
        encode_pa(PaOutput::Rfo, 16),
        Err(ConfigError::PowerOutOfRange)
    );
}

#[test]
fn ocp_encoding() {
    assert_eq!(encode_ocp(100), Ok(0x2B)); // datasheet default
    assert_eq!(encode_ocp(45), Ok(0x20));
    assert_eq!(encode_ocp(120), Ok(0x2F));
    assert_eq!(encode_ocp(140), Ok(0x31));
    assert_eq!(encode_ocp(240), Ok(0x3B));
    assert!(encode_ocp(44).is_err());
    assert!(encode_ocp(241).is_err());
}

#[test]
fn rssi_snr() {
    assert_eq!(packet_rssi_dbm(60, 40, true), -157 + 64);
    assert_eq!(packet_rssi_dbm(60, -20, true), -157 + 60 - 5);
    assert_eq!(packet_rssi_dbm(60, 0, false), -164 + 64);
    assert_eq!(rssi_dbm(50, true), -107);
    let p = PacketInfo {
        len: 1,
        rssi_dbm: 0,
        snr_quarter_db: -10,
    };
    assert_eq!(p.snr_db(), -2.5);
}

#[test]
fn opmode_and_irq_bits() {
    assert_eq!(Mode::from_bits(0x81), Mode::Standby);
    assert_eq!(Mode::from_bits(0x87), Mode::Cad);
    assert_eq!(Mode::from_bits(0x8D), Mode::RxContinuous);
    assert_eq!(Dio0Mapping::CadDone.apply(0x00), 0x80);
    assert_eq!(Dio0Mapping::TxDone.apply(0xFF), 0x7F);
    assert_eq!(Dio0Mapping::RxDone.apply(0xC5), 0x05);
    let f = Irq::RX_DONE | Irq::PAYLOAD_CRC_ERROR;
    assert_eq!(f.bits(), 0x60);
    assert!(f.contains(Irq::RX_DONE));
    assert!(!f.contains(Irq::RX_DONE | Irq::TX_DONE));
    assert!(f.intersects(Irq::RX_DONE | Irq::TX_DONE));
    assert_eq!(Register::Version.addr(), 0x42);
    assert_eq!(Register::SyncWord.addr(), 0x39);
}

#[test]
fn fsk_encoding() {
    use sx1276::fsk::*;
    // 32 MHz / 4.8 kbps = 6666.67 -> reg 0x1A0A + frac 11/16 (datasheet table rounds to 0x1A0B, no frac)
    assert_eq!(bitrate_regs(4_800), (0x1A0A, 0x0B));
    assert_eq!(bitrate_regs(300_000), (0x006A, 0x0B));
    let (r, f) = bitrate_regs(300_000);
    assert!((bitrate_from_regs(r, f) as i64 - 300_000).abs() < 100);
    assert_eq!(fdev_reg(5_000), 0x0052); // datasheet default 5 kHz
    assert_eq!(fdev_reg(100_000), 1638);
    // RxBw: default 0x05 = 10.4 kHz (mant 24, exp 5); widest 250 kHz = mant 16 exp 1 = 0x01
    assert_eq!(rx_bw_reg(10_000), (0x15, 10_416));
    assert_eq!(rx_bw_reg(250_000), (0x01, 250_000));
    assert_eq!(rx_bw_reg(240_000), (0x01, 250_000));
    assert_eq!(rx_bw_reg(200_000), (0x09, 200_000));
    let c = FskConfig::default();
    assert!(c.validate().is_ok());
    assert_eq!(c.rx_bw(), (0x01, 250_000));
    assert_eq!(c.sync_config(), 0x51);
    assert_eq!(c.packet_config1(), 0xD0);
    assert_eq!(c.frame_bytes(255), 4 + 2 + 1 + 255 + 2);
    assert_eq!(c.time_on_air_us(255), 264 * 8 * 1_000_000 / 300_000);
    assert_eq!(
        FskConfig {
            fdev_hz: 120_000,
            ..c
        }
        .validate(),
        Err(FskError::TooWide)
    );
    assert_eq!(
        FskConfig {
            fdev_hz: 50_000,
            ..c
        }
        .validate(),
        Err(FskError::IndexTooLow)
    );
    assert_eq!(
        FskConfig {
            bitrate_bps: 400_000,
            ..c
        }
        .validate(),
        Err(FskError::BitrateOutOfRange)
    );
    assert_eq!(
        FskConfig { sync_len: 0, ..c }.validate(),
        Err(FskError::BadSyncWord)
    );
}
