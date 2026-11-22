//! Region rule tables: licence ID plausibility and identification interval.

use crate::config::ConfigError;

/// Region whose rules apply to the licence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Region {
    /// United States (FCC amateur service, 47 CFR Part 97).
    Us,
    /// Any other region. Has no built-in rules: the user must supply an ID mask and an
    /// identification interval in the licence file, otherwise licensed mode stays blocked.
    Other(IdRule),
}

/// How an ID is checked and how often it must be sent on air.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdRule {
    /// Built-in US callsign shapes.
    UsCallsign,
    /// User-supplied mask. `L` is a letter A-Z, `D` is a digit, anything else must match
    /// literally. Example: `LLDLL`.
    Mask(String),
    /// `region = "other"` without a mask: licensed mode is blocked.
    Missing,
}

impl Region {
    /// Maximum seconds between identifications while transmitting. `None` means unknown, and
    /// transmitting in licensed mode is refused.
    ///
    /// US: 47 CFR 97.119(a), identify "at the end of each communication, and at least every
    /// 10 minutes during a communication".
    /// <https://www.ecfr.gov/current/title-47/chapter-I/subchapter-D/part-97/subpart-B/section-97.119>
    pub fn id_interval_secs(&self, user_interval: Option<u32>) -> Option<u32> {
        match self {
            Region::Us => Some(600),
            Region::Other(_) => user_interval.filter(|s| *s > 0),
        }
    }

    /// Normalise (trim, upper-case) and check the ID shape. Returns the normalised ID.
    /// The error never contains the ID.
    pub fn check_id(&self, raw: &str) -> Result<String, ConfigError> {
        let id = raw.trim().to_ascii_uppercase();
        let ok = match self {
            Region::Us => us_callsign_plausible(&id),
            Region::Other(IdRule::Mask(m)) => mask_matches(m, &id),
            Region::Other(IdRule::UsCallsign) => us_callsign_plausible(&id),
            Region::Other(IdRule::Missing) => return Err(ConfigError::OtherNeedsRule),
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

/// `L` letter, `D` digit, any other mask character matches itself. Lengths must be equal.
pub fn mask_matches(mask: &str, id: &str) -> bool {
    let (m, i) = (mask.as_bytes(), id.as_bytes());
    m.len() == i.len()
        && m.iter().zip(i).all(|(m, c)| match m {
            b'L' => c.is_ascii_uppercase(),
            b'D' => c.is_ascii_digit(),
            lit => lit == c,
        })
}

/// A mask is usable when it is 3..=16 chars of `L`, `D`, digits, uppercase letters, `-`.
pub fn mask_is_sane(mask: &str) -> bool {
    (3..=16).contains(&mask.len())
        && mask
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'-')
        && mask.bytes().any(|c| c == b'L' || c == b'D')
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
    fn other_needs_mask() {
        let r = Region::Other(IdRule::Missing);
        assert_eq!(r.check_id("AB1CD"), Err(ConfigError::OtherNeedsRule));
        let r = Region::Other(IdRule::Mask("LLDLL".into()));
        assert!(r.check_id("ab1cd").is_ok());
        assert_eq!(r.check_id("AB1C"), Err(ConfigError::BadIdShape));
        assert_eq!(r.check_id("ABDCD"), Err(ConfigError::BadIdShape));
    }

    #[test]
    fn intervals() {
        assert_eq!(Region::Us.id_interval_secs(None), Some(600));
        assert_eq!(Region::Us.id_interval_secs(Some(5)), Some(600));
        let o = Region::Other(IdRule::Missing);
        assert_eq!(o.id_interval_secs(None), None);
        assert_eq!(o.id_interval_secs(Some(0)), None);
        assert_eq!(o.id_interval_secs(Some(300)), Some(300));
    }

    #[test]
    fn mask_sanity() {
        assert!(mask_is_sane("LLDLL"));
        assert!(!mask_is_sane("LL"));
        assert!(!mask_is_sane("lldll"));
        assert!(!mask_is_sane("..."));
    }
}
