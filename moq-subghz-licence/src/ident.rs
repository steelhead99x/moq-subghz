//! Station identification frames and their schedule.
//!
//! Only the licence ID goes on air (the operator name stays off air). Frames are built only
//! from a [`TxPermit`] that says `station_id`, which [`crate::check_transmit`] grants only in
//! licensed mode on an amateur profile with transmit explicitly allowed (never on ISM profiles). Plain text, never sealed.

use crate::config::LicenceConfig;
use crate::gate::TxPermit;
use std::time::{Duration, Instant};

/// Frame magic: "MQID".
pub const MAGIC: [u8; 4] = *b"MQID";
/// Frame layout version.
pub const VERSION: u8 = 1;

/// Build an identification payload: `MQID`, version, length, ID bytes (ASCII, clear text).
/// `None` unless the permit allows transmitting with a station ID and the config has an ID.
pub fn id_frame(permit: &TxPermit, licence: &LicenceConfig) -> Option<Vec<u8>> {
    if !permit.station_id() {
        return None;
    }
    let id = licence.on_air_id()?;
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

/// What a permit asks of the caller about station identification. Match every arm: there is no
/// `Option` or `Result` to flatten into "transmit anyway".
#[derive(Clone, Debug)]
#[must_use = "MustNotTransmit means the tool must not transmit"]
pub enum IdPlan {
    /// The permit needs no station ID (not licensed, not an amateur profile, or not transmitting).
    NoIdRequired,
    /// Send IDs on this schedule.
    Schedule(IdScheduler),
    /// An ID is required and cannot be scheduled. The caller must not transmit.
    MustNotTransmit(ScheduleError),
}

impl TxPermit {
    /// What this permit requires for station identification.
    pub fn id_plan(&self) -> IdPlan {
        if !self.station_id() {
            return IdPlan::NoIdRequired;
        }
        let secs = match self.id_interval_secs() {
            Some(s) if s > ID_LEAD_SECS => s,
            _ => return IdPlan::MustNotTransmit(ScheduleError::IntervalTooShort),
        };
        IdPlan::Schedule(IdScheduler {
            period: Duration::from_secs(u64::from(secs - ID_LEAD_SECS)),
            last: None,
            sent_since_id: false,
        })
    }
}

/// Decides when to send the next identification, on the monotonic clock ([`Instant`]) so a
/// wall-clock step cannot delay one. An ID goes out at the start, then a fixed
/// [`ID_LEAD_SECS`] (60 s) before the region's limit is reached (every 540 s for a 600 s rule),
/// and once more at the end of a communication.
///
/// The monotonic clock does not count time the machine spends suspended. A suspended machine is
/// not transmitting, but call [`IdScheduler::mark_sent`] again after a resume if in doubt.
#[derive(Clone, Debug)]
pub struct IdScheduler {
    period: Duration,
    last: Option<Instant>,
    sent_since_id: bool,
}

impl IdScheduler {
    /// True when an ID must be sent before (or instead of) the next data frame.
    pub fn due(&self) -> bool {
        self.due_at(Instant::now())
    }

    /// Call after every non-ID transmission.
    pub fn note_data_sent(&mut self) {
        self.sent_since_id = true;
    }

    /// Call after an ID frame went out.
    pub fn mark_sent(&mut self) {
        self.mark_sent_at(Instant::now());
    }

    /// True when data went out since the last ID: send a final ID when the run ends.
    pub fn final_id_needed(&self) -> bool {
        self.sent_since_id
    }

    // The clock is a parameter here so tests do not sleep.

    /// A reading earlier than the last ID (it cannot happen on a monotonic clock) or one the
    /// clock cannot place counts as due: when in doubt, identify.
    fn due_at(&self, now: Instant) -> bool {
        match self.last {
            None => true,
            Some(t) => now
                .checked_duration_since(t)
                .is_none_or(|elapsed| elapsed >= self.period),
        }
    }

    fn mark_sent_at(&mut self, now: Instant) {
        self.last = Some(now);
        self.sent_since_id = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::band::Tuning;
    use crate::gate::{check_start, check_transmit, Flags, Profile};
    use crate::region::Region;
    use crate::Mode;

    const HAM: Tuning = Tuning {
        start_hz: 435_000_000,
        end_hz: 435_000_000,
    };
    const ISM: Tuning = Tuning {
        start_hz: 915_000_000,
        end_hz: 915_000_000,
    };

    fn setup() -> (LicenceConfig, TxPermit) {
        let c = LicenceConfig::from_parts(Region::Us, Some("K1AB"), Some("Test Person")).unwrap();
        let f = Flags {
            transmit: true,
            licence: Some(&c),
            ..Flags::default()
        };
        let p = check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &f).unwrap();
        (c, p)
    }

    fn sched(p: &TxPermit) -> IdScheduler {
        match p.id_plan() {
            IdPlan::Schedule(s) => s,
            other => panic!("{other:?}"),
        }
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
        let rx = Flags {
            licence: Some(&c),
            ..Flags::default()
        };
        // A receive permit has no station ID and no way to build a frame.
        assert!(check_start(Mode::Licensed, HAM, Profile::AMATEUR, &rx).is_ok());
        let p = check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &rx);
        assert!(p.is_err());
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        let p = check_transmit(Mode::Unlicensed, ISM, Profile::ISM, &f).unwrap();
        assert!(id_frame(&p, &c).is_none() && matches!(p.id_plan(), IdPlan::NoIdRequired));
    }

    #[test]
    fn no_frame_on_ism_profile_even_when_licensed() {
        let (c, _) = setup();
        let f = Flags {
            transmit: true,
            licence: Some(&c),
            ..Flags::default()
        };
        let p = check_transmit(Mode::Licensed, ISM, Profile::ISM, &f).unwrap();
        assert!(id_frame(&p, &c).is_none() && matches!(p.id_plan(), IdPlan::NoIdRequired));
    }

    #[test]
    fn short_intervals_must_not_transmit() {
        let (_, p) = setup();
        for secs in [None, Some(0), Some(30), Some(60)] {
            assert!(
                matches!(
                    p.clone().with_id_interval(secs).id_plan(),
                    IdPlan::MustNotTransmit(ScheduleError::IntervalTooShort)
                ),
                "{secs:?}"
            );
        }
        let mut s = sched(&p.with_id_interval(Some(61)));
        let t0 = Instant::now();
        s.mark_sent_at(t0);
        assert!(!s.due_at(t0) && s.due_at(t0 + Duration::from_secs(1)));
    }

    #[test]
    fn huge_intervals_do_not_overflow() {
        let (_, p) = setup();
        let mut s = sched(&p.with_id_interval(Some(u32::MAX)));
        let t0 = Instant::now();
        s.mark_sent_at(t0);
        assert!(!s.due_at(t0 + Duration::from_secs(86_400 * 365)));
        assert!(s.due_at(t0 + Duration::from_secs(u64::from(u32::MAX))));
    }

    #[test]
    fn schedule_us_ten_minutes() {
        let (_, p) = setup();
        let mut s = sched(&p);
        let t0 = Instant::now();
        let at = |n| t0 + Duration::from_secs(n);
        assert!(s.due_at(at(0)));
        s.mark_sent_at(at(0));
        assert!(!s.final_id_needed());
        assert!(!s.due_at(at(539)));
        assert!(s.due_at(at(540)));
        assert_eq!(ID_LEAD_SECS, 60);
        assert!(s.due_at(at(600)));
        s.note_data_sent();
        assert!(s.final_id_needed());
        s.mark_sent_at(at(540));
        assert!(!s.final_id_needed());
    }

    #[test]
    fn a_reading_before_the_last_id_counts_as_due() {
        let (_, p) = setup();
        let mut s = sched(&p);
        let t0 = Instant::now() + Duration::from_secs(3600);
        s.mark_sent_at(t0);
        // The clock stepped back an hour: do not wait an hour plus 9 minutes.
        assert!(s.due_at(t0 - Duration::from_secs(3600)));
        assert!(s.due_at(t0 - Duration::from_nanos(1)));
        assert!(!s.due_at(t0));
        // Marking with an earlier reading only makes the next ID come sooner.
        s.mark_sent_at(t0 - Duration::from_secs(100));
        assert!(!s.due_at(t0 - Duration::from_secs(100) + Duration::from_secs(539)));
        assert!(s.due_at(t0 - Duration::from_secs(100) + Duration::from_secs(540)));
    }

    #[test]
    fn no_final_id_when_nothing_was_sent() {
        let (_, p) = setup();
        let mut s = sched(&p);
        assert!(!s.final_id_needed());
        s.note_data_sent();
        assert!(s.final_id_needed());
    }

    #[test]
    fn the_real_clock_is_used_without_arguments() {
        let (_, p) = setup();
        let mut s = sched(&p);
        assert!(s.due());
        s.mark_sent();
        assert!(!s.due());
    }
}
