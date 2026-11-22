//! The transmit gate. Call [`check_transmit`] before keying a radio and [`check_start`] before
//! starting a tool on a band. Fails closed: the default [`Flags`] never permit transmit.

use crate::band::{Band, BandKind};
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

/// What the gate allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permit {
    pub mode: Mode,
    /// True only from [`check_transmit`].
    pub may_transmit: bool,
    /// Send the station ID in identification frames: licensed mode, an amateur profile and
    /// transmitting only. Never on ISM profiles.
    pub station_id: bool,
    /// Longest gap between identifications, in seconds (when `station_id`).
    pub id_interval_secs: Option<u32>,
}

/// Why the gate said no. Never carries the licence ID or name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Unlicensed mode and the band is licensed-only.
    LicensedBandInUnlicensedMode,
    /// Unlicensed mode and an amateur profile without `--rx-amateur`, or any attempt to
    /// transmit on an amateur profile in unlicensed mode.
    AmateurProfileInUnlicensedMode,
    /// The profile kind does not match the band kind (for example an ISM profile on an amateur band).
    ProfileBandMismatch,
    /// Licensed mode but no licence file was loaded.
    NoLicenceConfig,
    MissingLicenceId,
    MissingOperatorName,
    /// The ID or config failed validation.
    InvalidLicence(ConfigError),
    /// The band belongs to a different region than the licence.
    RegionMismatch,
    /// Amateur profile with sealing or an AES payload.
    EncryptionOnAmateurProfile,
    /// Amateur profile with authentication (until reviewed).
    AuthenticationOnAmateurProfile,
    /// `--transmit` was not passed (receive-only is the default).
    TransmitNotRequested,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LicensedBandInUnlicensedMode => write!(f, "this band is licensed-only and unlicensed mode blocks it"),
            Self::AmateurProfileInUnlicensedMode => write!(f, "amateur profiles need licensed mode; in unlicensed mode only receiving with --rx-amateur is allowed (receiving generally needs no licence, but verify your local rules)"),
            Self::ProfileBandMismatch => write!(f, "the profile kind does not match the band"),
            Self::NoLicenceConfig => write!(f, "licensed mode needs a licence file (--licence-file, MOQ_SUBGHZ_LICENCE or the XDG config path)"),
            Self::MissingLicenceId => write!(f, "licensed mode needs a licence ID in the licence file"),
            Self::MissingOperatorName => write!(f, "licensed mode needs an operator name in the licence file"),
            Self::InvalidLicence(e) => write!(f, "{e}"),
            Self::RegionMismatch => write!(f, "the band is not in the licence's region"),
            Self::EncryptionOnAmateurProfile => write!(f, "amateur profiles forbid encryption (no sealing, no AES payload)"),
            Self::AuthenticationOnAmateurProfile => write!(f, "amateur profiles use no authentication until reviewed"),
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

/// May the tool start on this band with this profile? Never permits transmit by itself.
///
/// * Unlicensed: the band must be `unlicensed_ok` and the profile must not be amateur. The one
///   exception is receive-only on an amateur band with `--rx-amateur` ([`Flags::rx_amateur`]).
/// * Licensed: a licence with both ID and name, in the band's region. Amateur profiles also
///   need an amateur band, no encryption and no authentication.
pub fn check_start(
    mode: Mode,
    band: &Band,
    profile: Profile,
    flags: &Flags<'_>,
) -> Result<Permit, Refusal> {
    if profile.kind != band.kind {
        return Err(Refusal::ProfileBandMismatch);
    }
    let mut permit = Permit {
        mode,
        may_transmit: false,
        station_id: false,
        id_interval_secs: None,
    };
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
        Mode::Licensed => {
            let l = licence_ok(flags.licence)?;
            let tag = match l.region() {
                Region::Us => "US",
            };
            if band.region != tag {
                return Err(Refusal::RegionMismatch);
            }
            if profile.kind == BandKind::Amateur {
                permit.id_interval_secs = Some(l.id_interval_secs());
            }
        }
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
/// amateur profile the
/// permit also says to send station ID frames.
pub fn check_transmit(
    mode: Mode,
    band: &Band,
    profile: Profile,
    flags: &Flags<'_>,
) -> Result<Permit, Refusal> {
    let mut p = check_start(mode, band, profile, flags)?;
    if mode == Mode::Unlicensed && profile.kind == BandKind::Amateur {
        // `--rx-amateur` is receive-only. Transmitting on an amateur band needs licensed mode.
        return Err(Refusal::AmateurProfileInUnlicensedMode);
    }
    if !flags.transmit {
        return Err(Refusal::TransmitNotRequested);
    }
    p.may_transmit = true;
    p.station_id = mode == Mode::Licensed && profile.kind == BandKind::Amateur;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::band::US_BANDS;

    fn ism() -> &'static Band {
        &US_BANDS[0]
    }
    fn ham() -> &'static Band {
        US_BANDS.iter().find(|b| b.name.contains("70 cm")).unwrap()
    }
    fn cfg() -> LicenceConfig {
        LicenceConfig::from_parts(Region::Us, Some("K1AB"), Some("Test Person")).unwrap()
    }

    #[test]
    fn default_flags_never_permit_transmit() {
        let c = cfg();
        let d = Flags::default();
        for mode in [Mode::Unlicensed, Mode::Licensed] {
            for (b, p) in [(ism(), Profile::ISM), (ham(), Profile::AMATEUR)] {
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
            check_transmit(Mode::Licensed, ham(), Profile::AMATEUR, &with),
            Err(Refusal::TransmitNotRequested)
        );
        assert_eq!(
            check_transmit(Mode::Unlicensed, ism(), Profile::ISM, &d),
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
            check_transmit(Mode::Unlicensed, ham(), Profile::AMATEUR, &f),
            Err(Refusal::AmateurProfileInUnlicensedMode)
        );
        assert_eq!(
            check_start(Mode::Unlicensed, ham(), Profile::ISM, &f),
            Err(Refusal::ProfileBandMismatch)
        );
        let mut b = *ism();
        b.unlicensed_ok = false;
        assert_eq!(
            check_start(Mode::Unlicensed, &b, Profile::ISM, &f),
            Err(Refusal::LicensedBandInUnlicensedMode)
        );
    }

    #[test]
    fn unlicensed_ism_with_transmit_flag_permits_without_id() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        let p = check_transmit(Mode::Unlicensed, ism(), Profile::ISM, &f).unwrap();
        assert!(p.may_transmit && !p.station_id && p.id_interval_secs.is_none());
        let p = check_start(Mode::Unlicensed, ism(), Profile::ISM, &Flags::default()).unwrap();
        assert!(!p.may_transmit);
    }

    #[test]
    fn licensed_without_config_refuses() {
        let f = Flags {
            transmit: true,
            ..Flags::default()
        };
        assert_eq!(
            check_start(Mode::Licensed, ism(), Profile::ISM, &f),
            Err(Refusal::NoLicenceConfig)
        );
        assert_eq!(
            check_transmit(Mode::Licensed, ham(), Profile::AMATEUR, &f),
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
                check_start(Mode::Licensed, ham(), Profile::AMATEUR, &f),
                Err(want.clone())
            );
            assert_eq!(
                check_transmit(Mode::Licensed, ism(), Profile::ISM, &f),
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
        let p = check_transmit(Mode::Licensed, ham(), Profile::AMATEUR, &f).unwrap();
        assert!(p.may_transmit && p.station_id);
        assert_eq!(p.id_interval_secs, Some(600));
        let rx = Flags {
            licence: Some(&c),
            ..Flags::default()
        };
        assert_eq!(
            check_transmit(Mode::Licensed, ham(), Profile::AMATEUR, &rx),
            Err(Refusal::TransmitNotRequested)
        );
        let p = check_start(Mode::Licensed, ham(), Profile::AMATEUR, &rx).unwrap();
        assert!(!p.may_transmit && !p.station_id);
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
                check_transmit(Mode::Licensed, ham(), Profile::AMATEUR, &f),
                Err(Refusal::EncryptionOnAmateurProfile)
            );
            assert_eq!(
                check_start(Mode::Licensed, ham(), Profile::AMATEUR, &f),
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
            check_transmit(Mode::Licensed, ham(), Profile::AMATEUR, &f),
            Err(Refusal::AuthenticationOnAmateurProfile)
        );
        // ISM profiles in licensed mode may seal.
        let f = Flags {
            transmit: true,
            encryption: Encryption::Sealed,
            licence: Some(&c),
            ..Flags::default()
        };
        assert!(check_transmit(Mode::Licensed, ism(), Profile::ISM, &f).is_ok());
    }

    #[test]
    fn rx_amateur_allows_receive_only_in_unlicensed() {
        let rx = Flags {
            rx_amateur: true,
            ..Flags::default()
        };
        let p = check_start(Mode::Unlicensed, ham(), Profile::AMATEUR, &rx).unwrap();
        assert!(!p.may_transmit && !p.station_id && p.id_interval_secs.is_none());
        // Without the flag it stays blocked.
        assert_eq!(
            check_start(Mode::Unlicensed, ham(), Profile::AMATEUR, &Flags::default()),
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
                check_transmit(Mode::Unlicensed, ham(), Profile::AMATEUR, &f),
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
            check_start(Mode::Unlicensed, ham(), Profile::AMATEUR, &f),
            Err(Refusal::EncryptionOnAmateurProfile)
        );
        // It does not let an ISM profile onto an amateur band.
        assert_eq!(
            check_start(Mode::Unlicensed, ham(), Profile::ISM, &rx),
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
        let p = check_transmit(Mode::Licensed, ism(), Profile::ISM, &f).unwrap();
        assert!(p.may_transmit && !p.station_id && p.id_interval_secs.is_none());
        let p = check_transmit(Mode::Licensed, ham(), Profile::AMATEUR, &f).unwrap();
        assert!(p.station_id && p.id_interval_secs == Some(600));
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
    fn refusal_text_never_has_values() {
        let c = cfg();
        let f = Flags {
            transmit: true,
            encryption: Encryption::Sealed,
            licence: Some(&c),
            ..Flags::default()
        };
        let e = check_transmit(Mode::Licensed, ham(), Profile::AMATEUR, &f).unwrap_err();
        let s = format!("{e} {e:?}");
        assert!(!s.contains("K1AB") && !s.contains("Test Person"));
    }
}
