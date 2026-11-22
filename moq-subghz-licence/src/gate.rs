//! The transmit gate. Call [`check_transmit`] before keying a radio and [`check_start`] before
//! starting a tool on a band. Fails closed: the default [`Flags`] never permit transmit.
//!
//! The gate takes the tuned frequency, not a band the caller picked, and looks the band up in the
//! table itself. A [`RxPermit`] or [`TxPermit`] can only be built here.

use crate::band::{self, Band, BandKind, Tuning};
use crate::config::{ConfigError, LicenceConfig};
use crate::region::Region;
use crate::Mode;
use std::fmt;

/// Payload protection requested for the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Encryption {
    /// Nothing sealed. Required on amateur profiles.
    #[default]
    None,
    /// ChaCha20-Poly1305 sealing of objects.
    Sealed,
    /// AES payload in the LoRaWAN-style frames of `lora2015`.
    AesPayload,
}

/// Object authentication requested for the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Auth {
    /// No signature or MAC. The only value allowed on amateur profiles until reviewed.
    #[default]
    None,
    /// Signed objects (authentication only).
    Signed,
}

/// A profile as the gate sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub kind: BandKind,
}

impl Profile {
    pub const ISM: Profile = Profile {
        kind: BandKind::Ism,
    };
    pub const AMATEUR: Profile = Profile {
        kind: BandKind::Amateur,
    };
}

/// Run flags. `Flags::default()` is receive-only, no encryption, no authentication, no licence,
/// and no amateur-band receive in unlicensed mode.
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags<'a> {
    /// `--transmit` was passed. Default false: never transmit.
    pub transmit: bool,
    pub encryption: Encryption,
    pub auth: Auth,
    /// `--rx-amateur` was passed: receive-only use of an amateur band in unlicensed mode.
    /// Receiving generally needs no licence, but verify your local rules. It never allows
    /// transmitting, and it has no effect in licensed mode. Default false.
    pub rx_amateur: bool,
    /// The loaded licence file, if any.
    pub licence: Option<&'a LicenceConfig>,
}

/// What [`check_start`] allows: opening a radio to receive. It cannot be used to transmit.
/// Only the gate builds one (private fields).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RxPermit {
    mode: Mode,
    band: &'static Band,
    tuning: Tuning,
}

impl RxPermit {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The table entry the tuned frequency fell in.
    pub fn band(&self) -> &'static Band {
        self.band
    }

    /// The span the gate checked. A radio `open` must tune inside it.
    pub fn tuning(&self) -> Tuning {
        self.tuning
    }

    /// True when the carrier `hz` lies inside the span the gate checked. The radio crates
    /// refuse to open on a frequency the permit does not cover.
    pub fn covers_hz(&self, hz: u64) -> bool {
        (self.tuning.start_hz..=self.tuning.end_hz).contains(&hz)
    }
}

/// What [`check_transmit`] allows: keying a radio. Only the gate builds one: the fields are
/// private, so no other code can make a permit that says "may transmit".
///
/// ```compile_fail
/// use moq_subghz_licence::TxPermit;
/// let forged = TxPermit { station_id: false, id_interval_secs: None };
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxPermit {
    rx: RxPermit,
    station_id: bool,
    id_interval_secs: Option<u32>,
}

impl TxPermit {
    pub fn mode(&self) -> Mode {
        self.rx.mode
    }

    /// The table entry the tuned frequency fell in.
    pub fn band(&self) -> &'static Band {
        self.rx.band
    }

    /// The span the gate checked. A radio `open` must tune inside it.
    pub fn tuning(&self) -> Tuning {
        self.rx.tuning
    }

    /// See [`RxPermit::covers_hz`].
    pub fn covers_hz(&self, hz: u64) -> bool {
        self.rx.covers_hz(hz)
    }

    /// The receive half of this permit, for a radio that is opened for both directions.
    pub fn as_rx(&self) -> &RxPermit {
        &self.rx
    }

    /// Send the station ID in identification frames: licensed mode and an amateur profile.
    /// Never on ISM profiles.
    pub fn station_id(&self) -> bool {
        self.station_id
    }

    /// Longest gap between identifications, in seconds (when `station_id`).
    pub fn id_interval_secs(&self) -> Option<u32> {
        self.id_interval_secs
    }

    /// A copy with another interval, so the scheduler's refusals can be tested.
    #[cfg(test)]
    pub(crate) fn with_id_interval(mut self, secs: Option<u32>) -> Self {
        self.id_interval_secs = secs;
        self
    }
}

/// Why the gate said no. Never carries the licence ID or name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Unlicensed mode and the band is licensed-only.
    LicensedBandInUnlicensedMode,
    /// Unlicensed mode and an amateur profile without `--rx-amateur`, or any attempt to
    /// transmit on an amateur profile in unlicensed mode.
    AmateurProfileInUnlicensedMode,
    /// The profile kind does not match the band the frequency is in (for example an ISM profile
    /// on an amateur-only band).
    ProfileBandMismatch,
    /// The tuned span is not inside any band of the table for this profile.
    OutOfBand,
    /// The span touches the project's own no-go zone around 925.0 MHz.
    ProjectGuard,
    /// Licensed mode but no licence file was loaded.
    NoLicenceConfig,
    MissingLicenceId,
    MissingOperatorName,
    /// The ID or config failed validation.
    InvalidLicence(ConfigError),
    /// Amateur profile with sealing or an AES payload.
    EncryptionOnAmateurProfile,
    /// Amateur profile with authentication (until reviewed).
    AuthenticationOnAmateurProfile,
    /// The permit calls for station identification frames and this tool cannot send them.
    StationIdNotSupported,
    /// The permit calls for station identification frames but no schedule can be built.
    StationIdNotSchedulable,
    /// `--transmit` was not passed (receive-only is the default).
    TransmitNotRequested,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LicensedBandInUnlicensedMode => write!(f, "this band is licensed-only and unlicensed mode blocks it"),
            Self::AmateurProfileInUnlicensedMode => write!(f, "amateur profiles need licensed mode; in unlicensed mode only receiving with --rx-amateur is allowed (receiving generally needs no licence, but verify your local rules)"),
            Self::ProfileBandMismatch => write!(f, "the profile kind does not match the band the frequency is in"),
            Self::OutOfBand => write!(f, "the tuned frequency is not inside a listed US band for this profile (902-928 MHz for ISM)"),
            Self::ProjectGuard => write!(f, "the project never operates within 0.3 MHz of 925.0 MHz"),
            Self::NoLicenceConfig => write!(f, "licensed mode needs a licence file (--licence-file, MOQ_SUBGHZ_LICENCE or the XDG config path)"),
            Self::MissingLicenceId => write!(f, "licensed mode needs a licence ID in the licence file"),
            Self::MissingOperatorName => write!(f, "licensed mode needs an operator name in the licence file"),
            Self::InvalidLicence(e) => write!(f, "{e}"),
            Self::EncryptionOnAmateurProfile => write!(f, "amateur profiles forbid encryption (no sealing, no AES payload)"),
            Self::AuthenticationOnAmateurProfile => write!(f, "amateur profiles use no authentication until reviewed"),
            Self::StationIdNotSupported => write!(f, "amateur transmit needs station identification frames, which this tool does not send yet"),
            Self::StationIdNotSchedulable => write!(f, "the station identification interval is too short to schedule"),
            Self::TransmitNotRequested => write!(f, "receive-only: pass --transmit to allow transmitting"),
        }
    }
}

impl std::error::Error for Refusal {}

fn licence_ok(licence: Option<&LicenceConfig>) -> Result<&LicenceConfig, Refusal> {
    let l = licence.ok_or(Refusal::NoLicenceConfig)?;
    if !l.has_id() {
        return Err(Refusal::MissingLicenceId);
    }
    if !l.has_name() {
        return Err(Refusal::MissingOperatorName);
    }
    Ok(l)
}

/// May the tool start on this frequency with this profile? Never permits transmit by itself.
///
/// * The span must lie inside a band of the table for the profile's kind and clear the project
///   guard around 925.0 MHz. Unlicensed mode uses the US table (the only one enabled).
/// * Unlicensed: the band must be `unlicensed_ok` and the profile must not be amateur. The one
///   exception is receive-only on an amateur band with `--rx-amateur` ([`Flags::rx_amateur`]).
/// * Licensed: a licence with both ID and name, whose region's table is used. Amateur profiles
///   also need no encryption and no authentication.
pub fn check_start(
    mode: Mode,
    tuning: Tuning,
    profile: Profile,
    flags: &Flags<'_>,
) -> Result<RxPermit, Refusal> {
    let licence = match mode {
        Mode::Unlicensed => None,
        Mode::Licensed => Some(licence_ok(flags.licence)?),
    };
    let region = licence.map_or(Region::Us, |l| l.region().clone());
    if band::span_in_project_guard(tuning) {
        return Err(Refusal::ProjectGuard);
    }
    let Some(band) = band::find(&region, profile.kind, tuning.start_hz, tuning.end_hz) else {
        let other = match profile.kind {
            BandKind::Ism => BandKind::Amateur,
            BandKind::Amateur => BandKind::Ism,
        };
        return Err(
            if band::find(&region, other, tuning.start_hz, tuning.end_hz).is_some() {
                Refusal::ProfileBandMismatch
            } else {
                Refusal::OutOfBand
            },
        );
    };
    let permit = RxPermit { mode, band, tuning };
    match mode {
        Mode::Unlicensed => {
            if profile.kind == BandKind::Amateur {
                if !flags.rx_amateur {
                    return Err(Refusal::AmateurProfileInUnlicensedMode);
                }
            } else if !band.unlicensed_ok {
                return Err(Refusal::LicensedBandInUnlicensedMode);
            }
        }
        Mode::Licensed => {}
    }
    if profile.kind == BandKind::Amateur {
        if flags.encryption != Encryption::None {
            return Err(Refusal::EncryptionOnAmateurProfile);
        }
        if flags.auth != Auth::None {
            return Err(Refusal::AuthenticationOnAmateurProfile);
        }
    }
    Ok(permit)
}

/// May the tool transmit? Needs [`check_start`] to pass AND `--transmit`. In licensed mode on an
/// amateur profile the permit also says to send station ID frames.
pub fn check_transmit(
    mode: Mode,
    tuning: Tuning,
    profile: Profile,
    flags: &Flags<'_>,
) -> Result<TxPermit, Refusal> {
    let rx = check_start(mode, tuning, profile, flags)?;
    if mode == Mode::Unlicensed && profile.kind == BandKind::Amateur {
        // `--rx-amateur` is receive-only. Transmitting on an amateur band needs licensed mode.
        return Err(Refusal::AmateurProfileInUnlicensedMode);
    }
    if !flags.transmit {
        return Err(Refusal::TransmitNotRequested);
    }
    let station_id = mode == Mode::Licensed && profile.kind == BandKind::Amateur;
    Ok(TxPermit {
        rx,
        station_id,
        id_interval_secs: flags
            .licence
            .filter(|_| station_id)
            .map(LicenceConfig::id_interval_secs),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISM: Tuning = Tuning {
        start_hz: 915_000_000,
        end_hz: 915_000_000,
    };
    /// 70 cm amateur.
    const HAM: Tuning = Tuning {
        start_hz: 435_000_000,
        end_hz: 435_000_000,
    };
    fn cfg() -> LicenceConfig {
        LicenceConfig::from_parts(Region::Us, Some("K1AB"), Some("Test Person")).unwrap()
    }

    #[test]
    fn a_permit_carries_the_span_it_was_checked_for() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        let t = Tuning::occupied(915_000_000, 500_000);
        let p = check_transmit(Mode::Unlicensed, t, Profile::ISM, &f).unwrap();
        assert_eq!(p.tuning(), t);
        assert!(p.covers_hz(915_000_000));
        assert!(p.covers_hz(914_750_000));
        assert!(!p.covers_hz(915_250_001));
        assert!(!p.covers_hz(868_100_000));
        assert_eq!(p.as_rx().tuning(), t);
    }

    #[test]
    fn the_eu_868_band_is_refused_because_the_table_has_no_such_entry() {
        // The stock Dragino app keys 868.1 MHz. No region profile for it is enabled.
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        let t = Tuning::occupied(868_100_000, 125_000);
        assert_eq!(
            check_transmit(Mode::Unlicensed, t, Profile::ISM, &f),
            Err(Refusal::OutOfBand)
        );
    }

    #[test]
    fn default_flags_never_permit_transmit() {
        let c = cfg();
        let d = Flags::default();
        for mode in [Mode::Unlicensed, Mode::Licensed] {
            for (b, p) in [(ISM, Profile::ISM), (HAM, Profile::AMATEUR)] {
                assert!(check_transmit(mode, b, p, &d).is_err());
                let with = Flags {
                    licence: Some(&c),
                    ..Flags::default()
                };
                assert!(check_transmit(mode, b, p, &with).is_err());
            }
        }
        let with = Flags {
            licence: Some(&c),
            ..Flags::default()
        };
        assert_eq!(
            check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &with),
            Err(Refusal::TransmitNotRequested)
        );
        assert_eq!(
            check_transmit(Mode::Unlicensed, ISM, Profile::ISM, &d),
            Err(Refusal::TransmitNotRequested)
        );
        assert_eq!(Mode::default(), Mode::Unlicensed);
        assert!(!Flags::default().transmit);
    }

    #[test]
    fn unlicensed_blocks_licensed_only_band() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        assert_eq!(
            check_transmit(Mode::Unlicensed, HAM, Profile::AMATEUR, &f),
            Err(Refusal::AmateurProfileInUnlicensedMode)
        );
        assert_eq!(
            check_start(Mode::Unlicensed, HAM, Profile::ISM, &f),
            Err(Refusal::ProfileBandMismatch)
        );
    }

    #[test]
    fn unlicensed_ism_with_transmit_flag_permits_without_id() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        let p = check_transmit(Mode::Unlicensed, ISM, Profile::ISM, &f).unwrap();
        assert!(!p.station_id() && p.id_interval_secs().is_none());
        assert!(check_start(Mode::Unlicensed, ISM, Profile::ISM, &Flags::default()).is_ok());
    }

    #[test]
    fn licensed_without_config_refuses() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        assert_eq!(
            check_start(Mode::Licensed, ISM, Profile::ISM, &f),
            Err(Refusal::NoLicenceConfig)
        );
        assert_eq!(
            check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &f),
            Err(Refusal::NoLicenceConfig)
        );
    }

    #[test]
    fn licensed_without_id_or_name_refuses() {
        let no_id = LicenceConfig::from_parts(Region::Us, None, Some("Test Person")).unwrap();
        let no_name = LicenceConfig::from_parts(Region::Us, Some("K1AB"), None).unwrap();
        let empty = LicenceConfig::from_parts(Region::Us, Some("  "), Some("")).unwrap();
        for (c, want) in [
            (&no_id, Refusal::MissingLicenceId),
            (&no_name, Refusal::MissingOperatorName),
            (&empty, Refusal::MissingLicenceId),
        ] {
            let f = Flags {
                transmit: true,
                licence: Some(c),
                ..Flags::default()
            };
            assert_eq!(
                check_start(Mode::Licensed, HAM, Profile::AMATEUR, &f),
                Err(want.clone())
            );
            assert_eq!(
                check_transmit(Mode::Licensed, ISM, Profile::ISM, &f),
                Err(want)
            );
        }
    }

    #[test]
    fn licensed_amateur_full_path() {
        let c = cfg();
        let f = Flags {
            transmit: true,
            licence: Some(&c),
            ..Flags::default()
        };
        let p = check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &f).unwrap();
        assert!(p.station_id());
        assert_eq!(p.id_interval_secs(), Some(600));
        let rx = Flags {
            licence: Some(&c),
            ..Flags::default()
        };
        assert_eq!(
            check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &rx),
            Err(Refusal::TransmitNotRequested)
        );
        assert!(check_start(Mode::Licensed, HAM, Profile::AMATEUR, &rx).is_ok());
    }

    #[test]
    fn amateur_profile_forces_no_encryption_no_auth() {
        let c = cfg();
        for enc in [Encryption::Sealed, Encryption::AesPayload] {
            let f = Flags {
                transmit: true,
                encryption: enc,
                licence: Some(&c),
                ..Flags::default()
            };
            assert_eq!(
                check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &f),
                Err(Refusal::EncryptionOnAmateurProfile)
            );
            assert_eq!(
                check_start(Mode::Licensed, HAM, Profile::AMATEUR, &f),
                Err(Refusal::EncryptionOnAmateurProfile)
            );
        }
        let f = Flags {
            transmit: true,
            auth: Auth::Signed,
            licence: Some(&c),
            ..Flags::default()
        };
        assert_eq!(
            check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &f),
            Err(Refusal::AuthenticationOnAmateurProfile)
        );
        // ISM profiles in licensed mode may seal.
        let f = Flags {
            transmit: true,
            encryption: Encryption::Sealed,
            licence: Some(&c),
            ..Flags::default()
        };
        assert!(check_transmit(Mode::Licensed, ISM, Profile::ISM, &f).is_ok());
    }

    #[test]
    fn rx_amateur_allows_receive_only_in_unlicensed() {
        let rx = Flags {
            rx_amateur: true,
            ..Flags::default()
        };
        assert!(check_start(Mode::Unlicensed, HAM, Profile::AMATEUR, &rx).is_ok());

        // Without the flag it stays blocked.
        assert_eq!(
            check_start(Mode::Unlicensed, HAM, Profile::AMATEUR, &Flags::default()),
            Err(Refusal::AmateurProfileInUnlicensedMode)
        );
        // Transmit stays blocked, with or without --transmit.
        for transmit in [false, true] {
            let f = Flags {
                rx_amateur: true,
                transmit,
                ..Flags::default()
            };
            assert_eq!(
                check_transmit(Mode::Unlicensed, HAM, Profile::AMATEUR, &f),
                Err(Refusal::AmateurProfileInUnlicensedMode)
            );
        }
        // Encryption and authentication rules still apply on the amateur profile.
        let f = Flags {
            rx_amateur: true,
            encryption: Encryption::Sealed,
            ..Flags::default()
        };
        assert_eq!(
            check_start(Mode::Unlicensed, HAM, Profile::AMATEUR, &f),
            Err(Refusal::EncryptionOnAmateurProfile)
        );
        // It does not let an ISM profile onto an amateur band.
        assert_eq!(
            check_start(Mode::Unlicensed, HAM, Profile::ISM, &rx),
            Err(Refusal::ProfileBandMismatch)
        );
    }

    #[test]
    fn station_id_only_on_amateur_profiles_in_licensed_mode() {
        let c = cfg();
        let f = Flags {
            transmit: true,
            licence: Some(&c),
            ..Flags::default()
        };
        let p = check_transmit(Mode::Licensed, ISM, Profile::ISM, &f).unwrap();
        assert!(!p.station_id() && p.id_interval_secs().is_none());
        let p = check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &f).unwrap();
        assert!(p.station_id() && p.id_interval_secs() == Some(600));
    }

    #[test]
    fn other_region_is_blocked() {
        // A licence for another region cannot even be built: see config.rs and region.rs.
        assert_eq!(
            Region::from_name("other"),
            Err(crate::config::ConfigError::RegionNotReviewed)
        );
    }

    #[test]
    fn gate_checks_the_tuned_frequency_against_the_band_table() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        let hz = Tuning::carrier;
        // Outside every listed band, or in a band of the other kind.
        for t in [
            hz(460_000_000),
            hz(901_999_999),
            hz(928_000_001),
            hz(868_000_000),
        ] {
            assert_eq!(
                check_transmit(Mode::Unlicensed, t, Profile::ISM, &f),
                Err(Refusal::OutOfBand),
                "{t:?}"
            );
        }
        assert_eq!(
            check_transmit(Mode::Unlicensed, HAM, Profile::ISM, &f),
            Err(Refusal::ProfileBandMismatch)
        );
        // A wide signal that crosses the band edge is out, a narrow one inside is in.
        assert_eq!(
            check_transmit(
                Mode::Unlicensed,
                Tuning::occupied(902_100_000, 500_000),
                Profile::ISM,
                &f
            ),
            Err(Refusal::OutOfBand)
        );
        assert!(check_transmit(
            Mode::Unlicensed,
            Tuning::occupied(902_300_000, 500_000),
            Profile::ISM,
            &f
        )
        .is_ok());
        // The span must lie in ONE band: straddling two listed bands is out.
        assert_eq!(
            check_start(
                Mode::Unlicensed,
                Tuning {
                    start_hz: 927_000_000,
                    end_hz: 2_400_000_000
                },
                Profile::ISM,
                &f
            ),
            Err(Refusal::OutOfBand)
        );
        // Reversed span.
        assert_eq!(
            check_start(
                Mode::Unlicensed,
                Tuning {
                    start_hz: 916_000_000,
                    end_hz: 915_000_000
                },
                Profile::ISM,
                &f
            ),
            Err(Refusal::OutOfBand)
        );
    }

    #[test]
    fn project_guard_around_925_mhz() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        for t in [
            Tuning::carrier(925_000_000),
            Tuning::carrier(925_299_999),
            Tuning::occupied(924_500_000, 1_000_000),
        ] {
            assert_eq!(
                check_transmit(Mode::Unlicensed, t, Profile::ISM, &f),
                Err(Refusal::ProjectGuard),
                "{t:?}"
            );
        }
        assert!(check_transmit(
            Mode::Unlicensed,
            Tuning::carrier(925_300_000),
            Profile::ISM,
            &f
        )
        .is_ok());
    }

    #[test]
    fn permit_reports_the_band_it_found() {
        let p = check_start(Mode::Unlicensed, ISM, Profile::ISM, &Flags::default()).unwrap();
        assert_eq!(p.band().name, "US 902-928 MHz ISM");
        assert_eq!(p.mode(), Mode::Unlicensed);
        let c = cfg();
        let f = Flags {
            licence: Some(&c),
            ..Flags::default()
        };
        // The same 915 MHz carrier is the amateur 33 cm band under the amateur profile.
        let p = check_start(Mode::Licensed, ISM, Profile::AMATEUR, &f).unwrap();
        assert_eq!(p.band().name, "US amateur 33 cm");
    }

    #[test]
    fn licensed_mode_checks_the_licence_before_the_frequency() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        assert_eq!(
            check_start(Mode::Licensed, Tuning::carrier(1), Profile::ISM, &f),
            Err(Refusal::NoLicenceConfig)
        );
    }

    #[test]
    fn refusal_text_never_has_values() {
        let c = cfg();
        let f = Flags {
            transmit: true,
            encryption: Encryption::Sealed,
            licence: Some(&c),
            ..Flags::default()
        };
        let e = check_transmit(Mode::Licensed, HAM, Profile::AMATEUR, &f).unwrap_err();
        let s = format!("{e} {e:?}");
        assert!(!s.contains("K1AB") && !s.contains("Test Person"));
    }
}
