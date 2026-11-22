//! The command-line side of the gate, shared by every binary that opens a radio.
//!
//! [`GateArgs`] holds the four flags (`--licence-file`, `--rx-amateur`, `--transmit`,
//! `--redact-licence`); [`GateArgs::station`] loads the licence file and fixes the mode;
//! [`Station`] runs the gate and is the only place that formats the licence ID and name for logs
//! and reports.

use crate::band::Tuning;
use crate::config::{resolve_path, ConfigError, LicenceConfig};
use crate::gate::{self, Auth, Encryption, Flags, Permit, Profile, Refusal};
use crate::ident::IdPlan;
use crate::{Mode, Redaction};
use std::path::{Path, PathBuf};

/// The gate's flags, as a clap argument group (feature `cli`). Add it with
/// `#[command(flatten)]`.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "cli", derive(clap::Args))]
pub struct GateArgs {
    /// Licence file. Giving one (or setting MOQ_SUBGHZ_LICENCE, or having one at
    /// $XDG_CONFIG_HOME/moq-subghz/licence.toml) selects licensed mode, which needs a valid ID
    /// and name. Without one the tool runs unlicensed: licence-free ISM only. The file is
    /// never read from the command line itself.
    #[cfg_attr(feature = "cli", arg(long, value_name = "PATH"))]
    pub licence_file: Option<PathBuf>,
    /// Receive-only use of an amateur band without a licence. Never allows transmitting.
    #[cfg_attr(feature = "cli", arg(long))]
    pub rx_amateur: bool,
    /// Allow transmitting (still needs the gate to permit it and --i-have-an-antenna).
    /// Without it the tool never transmits.
    #[cfg_attr(feature = "cli", arg(long))]
    pub transmit: bool,
    /// Write [redacted] instead of the licence ID and name in logs and reports.
    #[cfg_attr(feature = "cli", arg(long))]
    pub redact_licence: bool,
}

/// Where the licence file may come from besides `--licence-file`.
#[derive(Clone, Copy, Debug, Default)]
pub struct Env<'a> {
    pub licence: Option<&'a str>,
    pub xdg_config_home: Option<&'a str>,
    pub home: Option<&'a str>,
}

impl GateArgs {
    /// Resolve the mode and load the licence file from the real environment.
    ///
    /// Licensed mode is on when a licence file is given with `--licence-file`, or found through
    /// `MOQ_SUBGHZ_LICENCE` or the XDG path. A file that is given but missing, or found but bad,
    /// is an error: the tool never falls back to unlicensed mode on a broken licence file.
    pub fn station(&self) -> Result<Station, ConfigError> {
        let get = |k: &str| std::env::var(k).ok();
        let (lic, xdg, home) = (
            get("MOQ_SUBGHZ_LICENCE"),
            get("XDG_CONFIG_HOME"),
            get("HOME"),
        );
        self.station_in(Env {
            licence: lic.as_deref(),
            xdg_config_home: xdg.as_deref(),
            home: home.as_deref(),
        })
    }

    /// [`GateArgs::station`] with the environment passed in, for tests.
    pub fn station_in(&self, env: Env<'_>) -> Result<Station, ConfigError> {
        let path = resolve_path(
            self.licence_file.as_deref(),
            env.licence,
            env.xdg_config_home,
            env.home,
        );
        let licence = match path {
            Some(p) => match LicenceConfig::load(&p) {
                Ok(l) => Some(l),
                // Only the search locations may be absent; an explicit path must exist.
                Err(ConfigError::NotFound) if !self.explicit_path(&p, env) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        Ok(Station {
            mode: if licence.is_some() {
                Mode::Licensed
            } else {
                Mode::Unlicensed
            },
            licence,
            redaction: if self.redact_licence {
                Redaction::Redact
            } else {
                Redaction::Show
            },
            rx_amateur: self.rx_amateur,
            transmit: self.transmit,
        })
    }

    /// True when `p` came from `--licence-file` or MOQ_SUBGHZ_LICENCE rather than the XDG search.
    fn explicit_path(&self, p: &Path, env: Env<'_>) -> bool {
        self.licence_file.as_deref() == Some(p)
            || env
                .licence
                .is_some_and(|e| !e.is_empty() && Path::new(e) == p)
    }
}

/// The run's mode, licence and flags. Build it with [`GateArgs::station`].
#[derive(Clone, Debug)]
pub struct Station {
    mode: Mode,
    licence: Option<LicenceConfig>,
    redaction: Redaction,
    rx_amateur: bool,
    transmit: bool,
}

impl Station {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn licence(&self) -> Option<&LicenceConfig> {
        self.licence.as_ref()
    }

    /// May the tool open a radio to receive on this span? Never permits transmit.
    pub fn check_start(
        &self,
        tuning: Tuning,
        profile: Profile,
        encryption: Encryption,
        auth: Auth,
    ) -> Result<Permit, Refusal> {
        gate::check_start(self.mode, tuning, profile, &self.flags(encryption, auth))
    }

    /// May the tool open a radio to transmit on this span? Needs `--transmit`.
    pub fn check_transmit(
        &self,
        tuning: Tuning,
        profile: Profile,
        encryption: Encryption,
        auth: Auth,
    ) -> Result<Permit, Refusal> {
        gate::check_transmit(self.mode, tuning, profile, &self.flags(encryption, auth))
    }

    fn flags(&self, encryption: Encryption, auth: Auth) -> Flags<'_> {
        Flags {
            transmit: self.transmit,
            encryption,
            auth,
            rx_amateur: self.rx_amateur,
            licence: self.licence.as_ref(),
        }
    }

    /// For a tool that cannot send station identification frames (every tool today): a transmit
    /// permit, or why not. Refuses when the permit would need IDs on air, instead of
    /// transmitting without them.
    pub fn check_transmit_without_station_id(
        &self,
        tuning: Tuning,
        profile: Profile,
        encryption: Encryption,
        auth: Auth,
    ) -> Result<Permit, Refusal> {
        let permit = self.check_transmit(tuning, profile, encryption, auth)?;
        match permit.id_plan() {
            IdPlan::NoIdRequired => Ok(permit),
            IdPlan::Schedule(_) => Err(Refusal::StationIdNotSupported),
            IdPlan::MustNotTransmit(_) => Err(Refusal::StationIdNotSchedulable),
        }
    }

    /// [`Station::log_line`] as a `,"licence":"..."` JSON member to splice into a summary
    /// object, or an empty string in unlicensed mode.
    pub fn json_member(&self) -> String {
        let Some(line) = self.log_line() else {
            return String::new();
        };
        let mut o = String::from(",\"licence\":\"");
        for c in line.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                c if c.is_control() => o.push_str(&format!("\\u{:04x}", u32::from(c))),
                c => o.push(c),
            }
        }
        o.push('"');
        o
    }

    /// The one line for run logs and reports, or `None` in unlicensed mode (nothing to record).
    /// Honours `--redact-licence`. Every log and report writes the licence through this.
    pub fn log_line(&self) -> Option<String> {
        self.licence.as_ref().map(|l| l.log_line(self.redaction))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::region::Region;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moq-cli-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn write_licence(d: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = d.join("licence.toml");
        std::fs::write(
            &p,
            "region = \"us\"\nlicence_id = \"K1AB\"\noperator_name = \"Test Person\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        p
    }

    #[test]
    fn no_licence_means_unlicensed_and_never_transmits_by_default() {
        let s = GateArgs::default().station_in(Env::default()).unwrap();
        assert_eq!(s.mode(), Mode::Unlicensed);
        assert!(s.log_line().is_none());
        let t = Tuning::carrier(915_000_000);
        assert_eq!(
            s.check_transmit(t, Profile::ISM, Encryption::None, Auth::None),
            Err(Refusal::TransmitNotRequested)
        );
        assert!(s
            .check_start(t, Profile::ISM, Encryption::None, Auth::None)
            .is_ok());
    }

    #[test]
    fn transmit_flag_alone_permits_unlicensed_ism_only() {
        let a = GateArgs {
            transmit: true,
            ..GateArgs::default()
        };
        let s = a.station_in(Env::default()).unwrap();
        let t = Tuning::carrier(915_000_000);
        assert!(s
            .check_transmit(t, Profile::ISM, Encryption::Sealed, Auth::None)
            .unwrap()
            .may_transmit());
        assert!(s
            .check_transmit(t, Profile::AMATEUR, Encryption::None, Auth::None)
            .is_err());
    }

    #[test]
    fn a_given_licence_file_that_is_missing_is_an_error() {
        let a = GateArgs {
            licence_file: Some(PathBuf::from("/nonexistent/moq-licence.toml")),
            ..GateArgs::default()
        };
        assert_eq!(
            a.station_in(Env::default()).unwrap_err(),
            ConfigError::NotFound
        );
        let env = Env {
            licence: Some("/nonexistent/moq-licence.toml"),
            ..Env::default()
        };
        assert_eq!(
            GateArgs::default().station_in(env).unwrap_err(),
            ConfigError::NotFound
        );
    }

    #[test]
    fn the_xdg_search_location_may_be_absent() {
        let env = Env {
            xdg_config_home: Some("/nonexistent/xdg"),
            home: Some("/nonexistent/home"),
            ..Env::default()
        };
        let s = GateArgs::default().station_in(env).unwrap();
        assert_eq!(s.mode(), Mode::Unlicensed);
    }

    #[cfg(unix)]
    #[test]
    fn a_licence_file_selects_licensed_mode_and_a_bad_one_never_falls_back() {
        let d = dir("lic");
        let p = write_licence(&d);
        let a = GateArgs {
            licence_file: Some(p.clone()),
            ..GateArgs::default()
        };
        let s = a.station_in(Env::default()).unwrap();
        assert_eq!(s.mode(), Mode::Licensed);
        assert_eq!(
            s.log_line().unwrap(),
            "licence_id=K1AB operator=Test Person"
        );
        // From MOQ_SUBGHZ_LICENCE and the XDG path too.
        let env = Env {
            licence: p.to_str(),
            ..Env::default()
        };
        assert_eq!(
            GateArgs::default().station_in(env).unwrap().mode(),
            Mode::Licensed
        );
        let xdg = d.join("xdg");
        std::fs::create_dir_all(xdg.join("moq-subghz")).unwrap();
        std::fs::copy(&p, xdg.join("moq-subghz/licence.toml")).unwrap();
        let env = Env {
            xdg_config_home: xdg.to_str(),
            ..Env::default()
        };
        assert_eq!(
            GateArgs::default().station_in(env).unwrap().mode(),
            Mode::Licensed
        );
        // A world-readable file is an error, not an unlicensed run.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            a.station_in(Env::default()).unwrap_err(),
            ConfigError::TooPermissive
        );
        let _ = std::fs::remove_file(xdg.join("moq-subghz/licence.toml"));
        let _ = std::fs::remove_dir(xdg.join("moq-subghz"));
        let _ = std::fs::remove_dir(&xdg);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir(&d);
    }

    #[cfg(unix)]
    #[test]
    fn redact_licence_changes_the_log_line_only() {
        let d = dir("red");
        let p = write_licence(&d);
        let a = GateArgs {
            licence_file: Some(p.clone()),
            redact_licence: true,
            transmit: true,
            ..GateArgs::default()
        };
        let s = a.station_in(Env::default()).unwrap();
        assert_eq!(
            s.log_line().unwrap(),
            "licence_id=[redacted] operator=[redacted]"
        );
        // Redaction does not touch the gate: licensed ISM transmit still works.
        assert!(s
            .check_transmit(
                Tuning::carrier(915_000_000),
                Profile::ISM,
                Encryption::None,
                Auth::None
            )
            .is_ok());
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir(&d);
    }

    #[test]
    fn json_member_escapes_and_is_empty_when_unlicensed() {
        let s = GateArgs::default().station_in(Env::default()).unwrap();
        assert_eq!(s.json_member(), "");
        let mut s = s;
        s.licence = Some(
            LicenceConfig::from_parts(Region::Us, Some("K1AB"), Some("Ann \"Q\\\" Doe")).unwrap(),
        );
        assert_eq!(
            s.json_member(),
            ",\"licence\":\"licence_id=K1AB operator=Ann \\\"Q\\\\\\\" Doe\""
        );
        s.redaction = Redaction::Redact;
        assert_eq!(
            s.json_member(),
            ",\"licence\":\"licence_id=[redacted] operator=[redacted]\""
        );
    }

    #[test]
    fn a_tool_without_id_frames_refuses_amateur_transmit() {
        let c = LicenceConfig::from_parts(Region::Us, Some("K1AB"), Some("Test Person")).unwrap();
        let s = Station {
            mode: Mode::Licensed,
            licence: Some(c),
            redaction: Redaction::Show,
            rx_amateur: false,
            transmit: true,
        };
        let ham = Tuning::carrier(435_000_000);
        assert_eq!(
            s.check_transmit_without_station_id(
                ham,
                Profile::AMATEUR,
                Encryption::None,
                Auth::None
            ),
            Err(Refusal::StationIdNotSupported)
        );
        // ISM needs no ID frames.
        assert!(s
            .check_transmit_without_station_id(
                Tuning::carrier(915_000_000),
                Profile::ISM,
                Encryption::None,
                Auth::None
            )
            .is_ok());
    }

    #[cfg(feature = "cli")]
    #[test]
    fn flags_parse() {
        use clap::Parser;
        #[derive(Parser)]
        struct P {
            #[command(flatten)]
            g: GateArgs,
        }
        let p = P::parse_from([
            "x",
            "--licence-file",
            "/a",
            "--rx-amateur",
            "--transmit",
            "--redact-licence",
        ]);
        assert!(p.g.rx_amateur && p.g.transmit && p.g.redact_licence);
        assert_eq!(p.g.licence_file, Some(PathBuf::from("/a")));
        let p = P::parse_from(["x"]);
        assert!(
            !p.g.rx_amateur && !p.g.transmit && !p.g.redact_licence && p.g.licence_file.is_none()
        );
    }
}
