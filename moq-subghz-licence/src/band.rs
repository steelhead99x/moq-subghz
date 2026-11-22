//! Band table. Each entry says whether licence-free (unlicensed) operation is allowed.
//!
//! Only frequencies the repo already uses (902-928 MHz) or that carry a citable URL are listed.
//! Everything else is a TODO, not a guess. Sub-band privileges inside an amateur band depend on
//! licence class and mode and are NOT checked here. Not legal advice.

use crate::region::Region;

/// What kind of service a band entry describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandKind {
    /// Licence-free ISM / Part 15 style operation.
    Ism,
    /// Amateur service: licence, station ID and no encryption.
    Amateur,
}

/// One band entry. Frequencies are in Hz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Band {
    /// Region tag the entry belongs to (`"US"`).
    pub region: &'static str,
    pub name: &'static str,
    pub low_hz: u64,
    pub high_hz: u64,
    pub kind: BandKind,
    /// True only when operation here needs no licence.
    pub unlicensed_ok: bool,
    /// Where the entry comes from.
    pub source: &'static str,
}

impl Band {
    /// True when `[start_hz, end_hz]` lies fully inside the band.
    pub fn contains(&self, start_hz: u64, end_hz: u64) -> bool {
        start_hz >= self.low_hz && end_hz <= self.high_hz && start_hz <= end_hz
    }
}

/// The frequency span a tool tunes: where the carrier sits and how wide the signal is.
/// The gate checks the whole span against the band table, so a carrier just inside a band edge
/// with a wide signal is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tuning {
    pub start_hz: u64,
    pub end_hz: u64,
}

impl Tuning {
    /// A carrier only (width unknown or negligible).
    pub fn carrier(hz: u64) -> Self {
        Self::occupied(hz, 0)
    }

    /// A signal centred on `centre_hz` that is `width_hz` wide in total.
    pub fn occupied(centre_hz: u64, width_hz: u64) -> Self {
        let half = width_hz.div_ceil(2);
        Self {
            start_hz: centre_hz.saturating_sub(half),
            end_hz: centre_hz.saturating_add(half),
        }
    }
}

const PART15: &str =
    "47 CFR 15.247 https://www.ecfr.gov/current/title-47/chapter-I/subchapter-A/part-15";
const PART97: &str = "47 CFR 97.301 https://www.ecfr.gov/current/title-47/chapter-I/subchapter-D/part-97/subpart-C/section-97.301";

/// US bands.
///
/// * 902-928 MHz: the repo's existing rule (docs/REGULATORY.md, HOWTO section 9); ISM under
///   FCC 15.247, and also the US amateur 33 cm band (97.301), which is a separate entry.
/// * 2400-2483.5 and 5725-5850 MHz: ISM bands named in 47 CFR 15.247(a). No code uses them yet.
/// * 144-148, 420-450 MHz: US amateur bands listed in 47 CFR 97.301(a).
///
/// TODO(owner): EU 868 MHz (ETSI EN 300 220), 802.11ah country channel plans, amateur 2.4 GHz and
/// other amateur bands. Not listed because no frequency has been checked against a source.
pub const US_BANDS: &[Band] = &[
    Band {
        region: "US",
        name: "US 902-928 MHz ISM",
        low_hz: 902_000_000,
        high_hz: 928_000_000,
        kind: BandKind::Ism,
        unlicensed_ok: true,
        source: PART15,
    },
    Band {
        region: "US",
        name: "US 2.4 GHz ISM",
        low_hz: 2_400_000_000,
        high_hz: 2_483_500_000,
        kind: BandKind::Ism,
        unlicensed_ok: true,
        source: PART15,
    },
    Band {
        region: "US",
        name: "US 5.8 GHz ISM",
        low_hz: 5_725_000_000,
        high_hz: 5_850_000_000,
        kind: BandKind::Ism,
        unlicensed_ok: true,
        source: PART15,
    },
    Band {
        region: "US",
        name: "US amateur 2 m",
        low_hz: 144_000_000,
        high_hz: 148_000_000,
        kind: BandKind::Amateur,
        unlicensed_ok: false,
        source: PART97,
    },
    Band {
        region: "US",
        name: "US amateur 70 cm",
        low_hz: 420_000_000,
        high_hz: 450_000_000,
        kind: BandKind::Amateur,
        unlicensed_ok: false,
        source: PART97,
    },
    Band {
        region: "US",
        name: "US amateur 33 cm",
        low_hz: 902_000_000,
        high_hz: 928_000_000,
        kind: BandKind::Amateur,
        unlicensed_ok: false,
        source: PART97,
    },
];

/// Bands for a region. Add a table here when a country is reviewed with citations.
pub fn bands(region: &Region) -> &'static [Band] {
    match region {
        Region::Us => US_BANDS,
    }
}

/// Find the band of `kind` that fully contains `[start_hz, end_hz]`.
pub fn find(region: &Region, kind: BandKind, start_hz: u64, end_hz: u64) -> Option<&'static Band> {
    bands(region)
        .iter()
        .find(|b| b.kind == kind && b.contains(start_hz, end_hz))
}

/// The project's existing guard: nothing within 0.3 MHz of 925.0 MHz (HOWTO section 9).
pub fn in_project_guard(hz: u64) -> bool {
    hz.abs_diff(925_000_000) < 300_000
}

/// True when any part of the span falls inside [`in_project_guard`].
pub fn span_in_project_guard(t: Tuning) -> bool {
    t.start_hz < 925_300_000 && t.end_hz > 924_700_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_rule_is_listed() {
        let b = find(&Region::Us, BandKind::Ism, 915_000_000, 915_000_000).unwrap();
        assert!(b.unlicensed_ok);
        assert!(find(&Region::Us, BandKind::Ism, 901_999_999, 915_000_000).is_none());
    }

    #[test]
    fn amateur_entries_are_never_unlicensed() {
        for b in US_BANDS.iter().filter(|b| b.kind == BandKind::Amateur) {
            assert!(!b.unlicensed_ok, "{}", b.name);
        }
        for b in US_BANDS.iter().filter(|b| b.kind == BandKind::Ism) {
            assert!(b.unlicensed_ok, "{}", b.name);
        }
    }

    #[test]
    fn every_entry_cites_a_url() {
        for b in US_BANDS {
            assert!(b.source.contains("https://"), "{}", b.name);
            assert!(b.low_hz < b.high_hz);
        }
    }

    #[test]
    fn tuning_span() {
        assert_eq!(
            Tuning::occupied(915_000_000, 500_000),
            Tuning {
                start_hz: 914_750_000,
                end_hz: 915_250_000
            }
        );
        let t = Tuning::occupied(u64::MAX, 10);
        assert_eq!(t.end_hz, u64::MAX);
        assert_eq!(Tuning::occupied(1, 10).start_hz, 0);
        assert!(span_in_project_guard(Tuning::carrier(925_000_000)));
        assert!(span_in_project_guard(Tuning::occupied(
            924_000_000,
            1_500_000
        )));
        assert!(!span_in_project_guard(Tuning::carrier(925_300_000)));
        assert!(!span_in_project_guard(Tuning::carrier(924_700_000)));
    }

    #[test]
    fn guard() {
        assert!(in_project_guard(925_000_000));
        assert!(in_project_guard(925_299_999));
        assert!(!in_project_guard(925_300_000));
    }
}
