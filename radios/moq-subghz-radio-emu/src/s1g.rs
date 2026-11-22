//! IEEE 802.11ah S1G PHY numbers.
//!
//! SOURCE of the rate table: IEEE 802.11ah-2016, S1G MCS tables, as reproduced in the
//! "S1G MCS data rates" table of https://en.wikipedia.org/wiki/IEEE_802.11ah (1 spatial
//! stream, 8 us long guard interval). The table below is not copied: rates are DERIVED
//! from the OFDM parameters (data subcarriers, bits per subcarrier, code rate, 40 us
//! symbol) and the unit tests check every cell against the published table.

/// Data subcarriers N_SD per bandwidth (MHz): 1 MHz 24, 2 MHz 52, 4 MHz 108, 8 MHz 234,
/// 16 MHz 468 (802.11ah-2016, Clause 23).
pub fn n_sd(bw_mhz: u8) -> Option<u32> {
    Some(match bw_mhz {
        1 => 24,
        2 => 52,
        4 => 108,
        8 => 234,
        16 => 468,
        _ => return None,
    })
}

/// (bits per subcarrier, code rate numerator, denominator, repetition) of MCS0..=10.
const MCS: [(u32, u32, u32, u32); 11] = [
    (1, 1, 2, 1), // BPSK 1/2
    (2, 1, 2, 1), // QPSK 1/2
    (2, 3, 4, 1), // QPSK 3/4
    (4, 1, 2, 1), // 16-QAM 1/2
    (4, 3, 4, 1), // 16-QAM 3/4
    (6, 2, 3, 1), // 64-QAM 2/3
    (6, 3, 4, 1), // 64-QAM 3/4
    (6, 5, 6, 1), // 64-QAM 5/6
    (8, 3, 4, 1), // 256-QAM 3/4
    (8, 5, 6, 1), // 256-QAM 5/6
    (1, 1, 2, 2), // BPSK 1/2 with 2x repetition, 1 MHz only
];

/// OFDM symbol duration with the long (8 us) guard interval, microseconds.
pub const SYMBOL_US: u64 = 40;

/// Data bits per OFDM symbol, or `None` for a combination the standard does not define
/// (MCS10 only at 1 MHz; MCS9 not at 2 MHz for one stream, per the published table).
pub fn n_dbps(bw_mhz: u8, mcs: u8) -> Option<u32> {
    let sd = n_sd(bw_mhz)?;
    let m = *MCS.get(mcs as usize)?;
    if (mcs == 10 && bw_mhz != 1) || (mcs == 9 && bw_mhz == 2) {
        return None;
    }
    let num = sd * m.0 * m.1;
    let den = m.2 * m.3;
    (num % den == 0).then_some(num / den)
}

/// Peak PHY rate in kbit/s (long GI, one stream).
pub fn rate_kbps(bw_mhz: u8, mcs: u8) -> Option<f64> {
    n_dbps(bw_mhz, mcs).map(|b| f64::from(b) * 1000.0 / SYMBOL_US as f64)
}

/// ASSUMPTION: PLCP preamble plus SIG fields, microseconds, one stream. 1 MHz: STF 160 +
/// LTF1 160 + SIG 240 = 560 us; 2 MHz and wider ("long" preamble): STF 80 + LTF1 80 +
/// SIG-A 80 + D-STF 40 + D-LTF 40 + SIG-B 40 = 360 us. These come from the frame structure of
/// 802.11ah-2016 as recalled when the model was written; they were not checked against the
/// standard text or a datasheet, and no measurement backs them.
pub fn preamble_us(bw_mhz: u8) -> u64 {
    if bw_mhz == 1 {
        560
    } else {
        360
    }
}

/// Time on air of a PPDU carrying `psdu_bytes`: preamble plus whole OFDM symbols for
/// SERVICE (16 bits) + PSDU + BCC tail (6 bits). Symbol quantisation is included.
///
/// ASSUMPTION: one BCC encoder, so the tail is a flat 6 bits. A PHY rate high enough to need a
/// second encoder (6 more tail bits) is not modelled. That moves a PPDU by at most one 40 us
/// symbol, and only when the bits land within 6 of a symbol boundary: up to about 8 percent of
/// a full-size packet at 16 MHz MCS9 (about 520 us), far less at the narrow, slow settings.
pub fn ppdu_us(bw_mhz: u8, mcs: u8, psdu_bytes: usize) -> Option<u64> {
    let dbps = u64::from(n_dbps(bw_mhz, mcs)?);
    let bits = 16u64
        .saturating_add(8u64.saturating_mul(psdu_bytes as u64))
        .saturating_add(6);
    Some(preamble_us(bw_mhz).saturating_add(bits.div_ceil(dbps).saturating_mul(SYMBOL_US)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published table (Mbit/s), rows MCS0..=10, columns 1, 2, 4, 8, 16 MHz; `0.0` = N/A.
    /// Wikipedia rounds some cells (2.93, 8.78, 17.6, ...); the check allows 0.5 % + 0.01.
    const PUB: [[f64; 5]; 11] = [
        [0.3, 0.65, 1.35, 2.93, 5.85],
        [0.6, 1.3, 2.7, 5.85, 11.7],
        [0.9, 1.95, 4.05, 8.78, 17.6],
        [1.2, 2.6, 5.4, 11.7, 23.4],
        [1.8, 3.9, 8.1, 17.6, 35.1],
        [2.4, 5.2, 10.8, 23.4, 46.8],
        [2.7, 5.85, 12.2, 26.3, 52.7],
        [3.0, 6.5, 13.5, 29.3, 58.5],
        [3.6, 7.8, 16.2, 35.1, 70.2],
        [4.0, 0.0, 18.0, 39.0, 78.0],
        [0.15, 0.0, 0.0, 0.0, 0.0],
    ];

    #[test]
    fn derived_rates_match_the_published_table() {
        for (mcs, row) in PUB.iter().enumerate() {
            for (i, bw) in [1u8, 2, 4, 8, 16].iter().enumerate() {
                let got = rate_kbps(*bw, mcs as u8);
                if row[i] == 0.0 {
                    assert_eq!(got, None, "MCS{mcs} at {bw} MHz is N/A");
                } else {
                    let g = got.unwrap_or_else(|| panic!("MCS{mcs} {bw} MHz")) / 1000.0;
                    assert!(
                        (g - row[i]).abs() <= row[i] * 0.005 + 0.01,
                        "MCS{mcs} {bw} MHz: {g} vs {}",
                        row[i]
                    );
                }
            }
        }
    }

    #[test]
    fn ppdu_time_quantises_to_symbols() {
        // 1 MHz MCS0: 12 data bits per us*... N_DBPS = 12; 100 B = 822 bits -> 69 symbols.
        assert_eq!(ppdu_us(1, 0, 100), Some(560 + 69 * 40));
        assert_eq!(ppdu_us(3, 0, 1), None);
    }
}
