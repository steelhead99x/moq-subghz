//! Radio settings for the test and the limits the host tools hold themselves to.

use sx1276::{Bandwidth, CodingRate, CssConfig, SpreadingFactor};

/// Highest power the test uses, as the other host tools in this repo do.
pub const MAX_DBM: i8 = 5;
/// Lowest power the PA_BOOST output and the stick's `L` command take.
pub const MIN_DBM: i8 = 2;

/// Spreading factor, bandwidth, coding rate and carrier of one test setting.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// Carrier in Hz.
    pub freq_hz: u32,
    /// Spreading factor, 6..=12.
    pub sf: u8,
    /// Bandwidth in Hz: 125000, 250000 or 500000.
    pub bw_hz: u32,
    /// Coding rate denominator, 5..=8 for 4/5..4/8.
    pub cr: u8,
}

/// Named settings. `sf7-125` is US915 DR3 on channel 0. `sf7-500` is the 500 kHz channel 64
/// at SF7, which the US915 plan uses only for downlinks (DR13); `sf8-500` is the uplink
/// DR4 on that channel. `sf7-250` is not a US915 width; it is the middle step of
/// Dave Akerman's fast SSDV mode.
pub fn named(name: &str) -> Result<Profile, String> {
    let (freq_hz, sf, bw_hz) = match name {
        "sf7-125" => (902_300_000, 7, 125_000),
        "sf7-500" => (903_000_000, 7, 500_000),
        "sf8-500" => (903_000_000, 8, 500_000),
        "sf7-250" => (903_000_000, 7, 250_000),
        _ => {
            return Err(format!(
                "unknown profile {name}; use sf7-125, sf7-500, sf8-500 or sf7-250"
            ))
        }
    };
    Ok(Profile {
        freq_hz,
        sf,
        bw_hz,
        cr: 5,
    })
}

impl Profile {
    /// Refuse what this test does not send or the stick would silently change.
    ///
    /// The stick firmware maps any bandwidth other than 125 and 250 kHz to 500 kHz, so the
    /// host refuses them here instead.
    pub fn validate(&self) -> Result<(), String> {
        let mhz = self.freq_hz as f64 / 1e6;
        if !(902.0..=928.0).contains(&mhz) || (mhz - 925.0).abs() < 0.3 {
            return Err(format!("{mhz} MHz: use 902-928 MHz and not 925.0"));
        }
        if !matches!(self.bw_hz, 125_000 | 250_000 | 500_000) {
            return Err(format!(
                "bandwidth {} Hz: use 125000, 250000 or 500000",
                self.bw_hz
            ));
        }
        if !(7..=12).contains(&self.sf) {
            return Err(format!("SF{}: use 7..=12", self.sf));
        }
        if !(5..=8).contains(&self.cr) {
            return Err(format!("coding rate 4/{}: use 5..=8", self.cr));
        }
        Ok(())
    }

    /// The modem settings. Everything not listed here is the driver default, which is what
    /// the stick firmware uses too: explicit header, CRC on, 8 preamble symbols, sync 0x12.
    pub fn css(&self) -> Result<CssConfig, String> {
        self.validate()?;
        Ok(CssConfig {
            bandwidth: Bandwidth::from_hz(self.bw_hz).ok_or("bandwidth")?,
            spreading_factor: SpreadingFactor::from_u8(self.sf).ok_or("spreading factor")?,
            coding_rate: CodingRate::from_denominator(self.cr).ok_or("coding rate")?,
            ..CssConfig::default()
        })
    }

    /// Time on air of a `len` byte frame, in microseconds.
    pub fn toa_us(&self, len: usize) -> Result<u32, String> {
        if !(1..=255).contains(&len) {
            return Err(format!("frame length {len} is outside 1..=255"));
        }
        Ok(self.css()?.time_on_air_us(len as u8))
    }

    /// The `L` line that puts the stick in receive on this setting (2 dBm, explicit header).
    pub fn stick_command(&self) -> String {
        format!(
            "L {} {} {} {} {MIN_DBM} 0",
            self.freq_hz, self.sf, self.bw_hz, self.cr
        )
    }
}

/// Transmit power must be inside the range the host tools allow.
pub fn check_power(dbm: i8) -> Result<(), String> {
    if (MIN_DBM..=MAX_DBM).contains(&dbm) {
        Ok(())
    } else {
        Err(format!("{dbm} dBm: use {MIN_DBM}..={MAX_DBM}"))
    }
}

/// Microseconds to wait after a frame: the larger of a fixed gap and the gap that keeps the
/// transmitter under `duty_pct` percent airtime.
pub fn gap_us(toa_us: u32, gap_ms: u32, duty_pct: f64) -> u64 {
    let fixed = gap_ms as u64 * 1000;
    if duty_pct > 0.0 && duty_pct < 100.0 {
        let off = (toa_us as f64 * (100.0 / duty_pct - 1.0)).ceil() as u64;
        fixed.max(off)
    } else {
        fixed
    }
}

/// True when the modem registers read back from the radio are the ones `cfg` wrote.
/// The symbol timeout bits and the AGC bit are not compared.
pub fn modem_readback_ok(mc1: u8, mc2: u8, mc3: u8, cfg: &CssConfig) -> bool {
    (mc1, mc2 & 0xFC, mc3 & 0x08)
        == (
            cfg.modem_config1(),
            cfg.modem_config2() & 0xFC,
            cfg.modem_config3() & 0x08,
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_profiles_are_the_requested_ones() {
        let a = named("sf7-125").unwrap();
        assert_eq!(
            (a.freq_hz, a.sf, a.bw_hz, a.cr),
            (902_300_000, 7, 125_000, 5)
        );
        let b = named("sf7-500").unwrap();
        assert_eq!((b.freq_hz, b.bw_hz), (903_000_000, 500_000));
        assert_eq!(named("sf7-250").unwrap().bw_hz, 250_000);
        let d4 = named("sf8-500").unwrap();
        assert_eq!((d4.freq_hz, d4.sf, d4.bw_hz), (903_000_000, 8, 500_000));
        assert!(named("sf9-125").is_err());
        for n in ["sf7-125", "sf7-500", "sf7-250", "sf8-500"] {
            named(n).unwrap().css().unwrap();
        }
    }

    #[test]
    fn config_is_explicit_header_crc_on_default_sync() {
        let c = named("sf7-125").unwrap().css().unwrap();
        assert_eq!(c, CssConfig::default());
        assert_eq!(c.sync_word, 0x12);
        assert!(c.crc);
    }

    #[test]
    fn airtime_matches_the_semtech_calculator() {
        // SF7, CR 4/5, CRC on, explicit header, 8 preamble symbols. Expected values come
        // from the datasheet formula in Python, not from this code. 399.6 ms is just under
        // the 400 ms dwell limit that sets the 242 byte framed US915 DR3 payload.
        let p = named("sf7-125").unwrap();
        assert_eq!(p.toa_us(255).unwrap(), 399_616);
        assert_eq!(p.toa_us(13).unwrap(), 46_336);
        assert_eq!(named("sf7-250").unwrap().toa_us(255).unwrap(), 199_808);
        let w = named("sf7-500").unwrap();
        assert_eq!(w.toa_us(255).unwrap(), 99_904);
        assert_eq!(w.toa_us(13).unwrap(), 11_584);
        assert_eq!(named("sf8-500").unwrap().toa_us(255).unwrap(), 176_768);
        assert!(p.toa_us(0).is_err());
        assert!(p.toa_us(256).is_err());
    }

    #[test]
    fn validation_catches_out_of_band_and_odd_settings() {
        let ok = named("sf7-125").unwrap();
        assert!(Profile {
            freq_hz: 868_100_000,
            ..ok
        }
        .validate()
        .is_err());
        assert!(Profile {
            freq_hz: 925_000_000,
            ..ok
        }
        .validate()
        .is_err());
        assert!(Profile {
            freq_hz: 928_000_000,
            ..ok
        }
        .validate()
        .is_ok());
        assert!(Profile {
            bw_hz: 62_500,
            ..ok
        }
        .validate()
        .is_err());
        assert!(Profile { sf: 6, ..ok }.validate().is_err());
        assert!(Profile { cr: 4, ..ok }.validate().is_err());
        assert!(Profile { cr: 9, ..ok }.validate().is_err());
    }

    #[test]
    fn power_limits() {
        assert!(check_power(2).is_ok() && check_power(5).is_ok());
        assert!(check_power(1).is_err() && check_power(6).is_err() && check_power(17).is_err());
    }

    #[test]
    fn stick_command_is_the_documented_line() {
        assert_eq!(
            named("sf7-500").unwrap().stick_command(),
            "L 903000000 7 500000 5 2 0"
        );
    }

    #[test]
    fn pacing_takes_the_larger_gap() {
        assert_eq!(gap_us(400_000, 0, 0.0), 0);
        assert_eq!(gap_us(400_000, 250, 0.0), 250_000);
        // 1 % duty: off for 99 times the airtime
        assert_eq!(gap_us(400_000, 0, 1.0), 39_600_000);
        assert_eq!(gap_us(400_000, 60_000, 1.0), 60_000_000);
        assert_eq!(gap_us(400_000, 5, 100.0), 5_000);
    }

    #[test]
    fn readback_compares_the_register_bits_that_matter() {
        let c = named("sf7-500").unwrap().css().unwrap();
        let (m1, m2, m3) = (c.modem_config1(), c.modem_config2(), c.modem_config3());
        assert!(modem_readback_ok(m1, m2 | 0x03, m3 | 0x04, &c));
        assert!(!modem_readback_ok(m1 ^ 0x10, m2, m3, &c));
        assert!(!modem_readback_ok(m1, m2 ^ 0x10, m3, &c));
        assert!(!modem_readback_ok(m1, m2 ^ 0x04, m3, &c));
    }
}
