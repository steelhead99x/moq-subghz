//! Region rule tables: licence ID plausibility and identification interval.

use crate::config::ConfigError;

/// Region whose rules apply to the licence.
///
/// Only the United States is enabled. A country is added here only after its rules have been
/// reviewed with citations: an ID shape check, the identification interval, and a band table
/// in `band.rs`. Until then a licence file naming any other region is rejected
/// ([`ConfigError::RegionNotReviewed`]) and licensed mode stays blocked for it. There is
/// deliberately no way to supply your own mask or interval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Region {
    /// United States (FCC amateur service, 47 CFR Part 97).
    Us,
}

impl Region {
    /// Region from the `region` key of the licence file. `"other"` is recognised but blocked.
    pub fn from_name(name: &str) -> Result<Self, ConfigError> {
        match name {
            "us" => Ok(Region::Us),
            "other" => Err(ConfigError::RegionNotReviewed),
            _ => Err(ConfigError::BadRegion),
        }
    }

    /// Maximum seconds between identifications while transmitting.
    ///
    /// US: 47 CFR 97.119(a), identify "at the end of each communication, and at least every
    /// 10 minutes during a communication".
    /// <https://www.ecfr.gov/current/title-47/chapter-I/subchapter-D/part-97/subpart-B/section-97.119>
    pub fn id_interval_secs(&self) -> u32 {
        match self {
            Region::Us => 600,
        }
    }

    /// Normalise (trim, upper-case) and check the ID shape. Returns the normalised ID.
    /// The error never contains the ID.
    pub fn check_id(&self, raw: &str) -> Result<String, ConfigError> {
        let id = raw.trim().to_ascii_uppercase();
        let ok = match self {
            Region::Us => us_callsign_plausible(&id),
        };
        if ok {
            Ok(id)
        } else {
            Err(ConfigError::BadIdShape)
        }
    }
}

/// Plausible FCC callsign shapes (1x2, 1x3, 2x1, 2x2, 2x3): prefix letters, one digit,
/// suffix letters. FCC prefixes are K, N, W (one letter, suffix 2 or 3 letters) or
/// AA-AL, KA-KZ, NA-NZ, WA-WZ (two letters, suffix 1 to 3 letters).
/// A plausible shape is NOT a valid or issued callsign. Portable suffixes (`/P`) are not accepted.
/// Shapes per the FCC callsign system: <https://www.fcc.gov/wireless/bureau-divisions/mobility-division/amateur-radio-service/amateur-radio-call-signs>
/// (TODO owner: confirm against current rules; special event 1x1 calls are rejected on purpose).
pub fn us_callsign_plausible(id: &str) -> bool {
    let b = id.as_bytes();
    if !b
        .iter()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return false;
    }
    let pre = b.iter().take_while(|c| c.is_ascii_uppercase()).count();
    if !(1..=2).contains(&pre) || pre >= b.len() || !b[pre].is_ascii_digit() {
        return false;
    }
    let suf = &b[pre + 1..];
    if !(1..=3).contains(&suf.len()) || !suf.iter().all(|c| c.is_ascii_uppercase()) {
        return false;
    }
    match pre {
        1 => matches!(b[0], b'K' | b'N' | b'W') && suf.len() >= 2,
        _ => match b[0] {
            b'A' => (b'A'..=b'L').contains(&b[1]),
            b'K' | b'N' | b'W' => true,
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_shapes_accept() {
        for id in [
            "K1AB", "N2ABC", "W9XYZ", "AA1A", "AL9ZZ", "KB1AB", "WZ0ABC", "NA5A",
        ] {
            assert!(us_callsign_plausible(id), "{id}");
        }
    }

    #[test]
    fn us_shapes_reject() {
        for id in [
            "", "N0CALL", "K1A", "W3C", "AM1AB", "BA1AB", "K1ABCD", "K11AB", "1K1AB", "KAB", "K1",
            "K1AB/P", "k1ab ", "AAA1AB", "A1AB", "K1A1",
        ] {
            assert!(!us_callsign_plausible(id), "{id}");
        }
    }

    #[test]
    fn check_id_normalises_and_errors_without_value() {
        assert_eq!(Region::Us.check_id(" k1ab ").unwrap(), "K1AB");
        let e = Region::Us.check_id("SECRETX").unwrap_err();
        assert!(!format!("{e} {e:?}").contains("SECRETX"));
    }

    #[test]
    fn other_region_is_blocked() {
        assert_eq!(Region::from_name("us"), Ok(Region::Us));
        assert_eq!(
            Region::from_name("other"),
            Err(ConfigError::RegionNotReviewed)
        );
        assert_eq!(Region::from_name("eu"), Err(ConfigError::BadRegion));
    }

    #[test]
    fn us_interval_is_fixed() {
        assert_eq!(Region::Us.id_interval_secs(), 600);
    }
}
