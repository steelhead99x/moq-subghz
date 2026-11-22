//! Station identification frames and their schedule.
//!
//! Only the licence ID goes on air (the operator name stays off air). Frames are built only
//! from a [`Permit`] that says `station_id`, which [`crate::check_transmit`] grants only in
//! licensed mode on an amateur profile with transmit explicitly allowed (never on ISM profiles). Plain text, never sealed.

use crate::config::LicenceConfig;
use crate::gate::Permit;
use std::time::Duration;

/// Frame magic: "MQID".
pub const MAGIC: [u8; 4] = *b"MQID";
/// Frame layout version.
pub const VERSION: u8 = 1;

/// Build an identification payload: `MQID`, version, length, ID bytes (ASCII, clear text).
/// `None` unless the permit allows transmitting with a station ID and the config has an ID.
pub fn id_frame(permit: &Permit, licence: &LicenceConfig) -> Option<Vec<u8>> {
    if !(permit.may_transmit && permit.station_id) {
        return None;
    }
    let id = licence.id(false)?;
    let mut f = Vec::with_capacity(6 + id.len());
    f.extend_from_slice(&MAGIC);
    f.push(VERSION);
    f.push(id.len() as u8); // IDs are at most 16 bytes (mask limit) or 6 (US).
    f.extend_from_slice(id.as_bytes());
    Some(f)
}

/// Parse an identification frame; returns the ID text.
pub fn parse_id_frame(b: &[u8]) -> Option<&str> {
    if b.len() < 6 || b[..4] != MAGIC || b[4] != VERSION || b.len() != 6 + b[5] as usize {
        return None;
    }
    std::str::from_utf8(&b[6..]).ok().filter(|s| s.is_ascii())
}

/// How long before the region's limit the next ID goes out, in seconds. For a 600 s rule that
/// is every 540 s.
pub const ID_LEAD_SECS: u32 = 60;

/// Why no scheduler could be built from a permit that calls for station IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    /// The interval is not longer than [`ID_LEAD_SECS`], so a fixed lead would mean sending IDs
    /// back to back (or never in time). The caller must not transmit.
    IntervalTooShort,
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the identification interval is too short to schedule with a {ID_LEAD_SECS} s lead"
        )
    }
}

impl std::error::Error for ScheduleError {}

/// Decides when to send the next identification. Time is a monotonic offset the caller
/// supplies, so tests do not sleep. An ID goes out at the start, then a fixed
/// [`ID_LEAD_SECS`] (60 s) before the region's limit is reached (every 540 s for a 600 s rule),
/// and once more at the end of a communication.
#[derive(Clone, Debug)]
pub struct IdScheduler {
    period: Duration,
    last: Option<Duration>,
    sent_since_id: bool,
}

impl IdScheduler {
    /// `Ok(None)` if the permit does not call for station IDs (not licensed, not an amateur
    /// profile, or not transmitting). `Err` if it does but the interval is not longer than
    /// 60 s: treat that as "do not transmit".
    pub fn new(permit: &Permit) -> Result<Option<Self>, ScheduleError> {
        if !(permit.may_transmit && permit.station_id) {
            return Ok(None);
        }
        let secs = permit
            .id_interval_secs
            .ok_or(ScheduleError::IntervalTooShort)?;
        if secs <= ID_LEAD_SECS {
            return Err(ScheduleError::IntervalTooShort);
        }
        Ok(Some(Self {
            period: Duration::from_secs(u64::from(secs - ID_LEAD_SECS)),
            last: None,
            sent_since_id: false,
        }))
    }

    /// True when an ID must be sent before (or instead of) the next data frame.
    pub fn due(&self, now: Duration) -> bool {
        match self.last {
            None => true,
            Some(t) => now.saturating_sub(t) >= self.period,
        }
    }

    /// Call after every non-ID transmission.
    pub fn note_data_sent(&mut self) {
        self.sent_since_id = true;
    }

    /// Call after an ID frame went out.
    pub fn mark_sent(&mut self, now: Duration) {
        self.last = Some(now);
        self.sent_since_id = false;
    }

    /// True when data went out since the last ID: send a final ID when the run ends.
    pub fn final_id_needed(&self) -> bool {
        self.sent_since_id || self.last.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::band::US_BANDS;
    use crate::gate::{check_start, check_transmit, Flags, Profile};
    use crate::region::Region;
    use crate::Mode;

    fn setup() -> (LicenceConfig, Permit) {
        let c = LicenceConfig::from_parts(Region::Us, Some("K1AB"), Some("Test Person")).unwrap();
        let band = US_BANDS.iter().find(|b| b.name.contains("70 cm")).unwrap();
        let f = Flags {
            transmit: true,
            licence: Some(&c),
            ..Flags::default()
        };
        let p = check_transmit(Mode::Licensed, band, Profile::AMATEUR, &f).unwrap();
        (c, p)
    }

    #[test]
    fn frame_round_trip_has_id_but_not_name() {
        let (c, p) = setup();
        let f = id_frame(&p, &c).unwrap();
        assert_eq!(parse_id_frame(&f), Some("K1AB"));
        assert!(!String::from_utf8_lossy(&f).contains("Test Person"));
        assert!(parse_id_frame(&f[..f.len() - 1]).is_none());
    }

    #[test]
    fn no_frame_without_transmit_or_in_unlicensed() {
        let (c, _) = setup();
        let band = US_BANDS.iter().find(|b| b.name.contains("70 cm")).unwrap();
        let rx = Flags {
            licence: Some(&c),
            ..Flags::default()
        };
        let p = check_start(Mode::Licensed, band, Profile::AMATEUR, &rx).unwrap();
        assert!(id_frame(&p, &c).is_none() && IdScheduler::new(&p).unwrap().is_none());
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        let p = check_transmit(Mode::Unlicensed, &US_BANDS[0], Profile::ISM, &f).unwrap();
        assert!(id_frame(&p, &c).is_none() && IdScheduler::new(&p).unwrap().is_none());
    }

    #[test]
    fn no_frame_on_ism_profile_even_when_licensed() {
        let (c, _) = setup();
        let f = Flags {
            transmit: true,
            licence: Some(&c),
            ..Flags::default()
        };
        let p = check_transmit(Mode::Licensed, &US_BANDS[0], Profile::ISM, &f).unwrap();
        assert!(id_frame(&p, &c).is_none() && IdScheduler::new(&p).unwrap().is_none());
    }

    #[test]
    fn short_intervals_are_refused() {
        let (_, mut p) = setup();
        for secs in [None, Some(0), Some(30), Some(60)] {
            p.id_interval_secs = secs;
            assert_eq!(
                IdScheduler::new(&p).unwrap_err(),
                ScheduleError::IntervalTooShort
            );
        }
        p.id_interval_secs = Some(61);
        let mut s = IdScheduler::new(&p).unwrap().unwrap();
        s.mark_sent(Duration::from_secs(0));
        assert!(!s.due(Duration::ZERO) && s.due(Duration::from_secs(1)));
    }

    #[test]
    fn schedule_us_ten_minutes() {
        let (_, p) = setup();
        let mut s = IdScheduler::new(&p).unwrap().unwrap();
        let t = Duration::from_secs;
        assert!(s.due(t(0)));
        s.mark_sent(t(0));
        assert!(!s.final_id_needed());
        assert!(!s.due(t(539)));
        assert!(s.due(t(540)));
        assert_eq!(ID_LEAD_SECS, 60);
        assert!(s.due(t(600)));
        s.note_data_sent();
        assert!(s.final_id_needed());
        s.mark_sent(t(540));
        assert!(!s.final_id_needed());
    }
}
