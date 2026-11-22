//! Which licence mode each end of a run uses, and whether the run files carry the licence.
//!
//! The project's programs pick licensed mode on their own when a licence file sits at
//! `MOQ_SUBGHZ_LICENCE` or `$XDG_CONFIG_HOME/moq-subghz/licence.toml`. On the board that happens
//! out of sight, so the bench never leaves it to them: every program it starts gets either
//! `--licence-file PATH` or `--no-licence`, and `--redact-licence` unless `--show-licence` is
//! given. [`Lic::describe`] says which, and the commands print it before they run anything.
//! The licence ID and name are never read or printed here: only paths.

use clap::Args;
use moq_subghz_licence::GateArgs;
use std::path::PathBuf;

/// The licence options of the bench commands that start the project's programs.
#[derive(Args, Clone, Debug, Default)]
pub struct LicenceOpts {
    /// Licence file on THIS machine, for the programs that run here. Selects licensed mode for them
    #[arg(long, value_name = "PATH", conflicts_with = "no_licence")]
    pub licence_file: Option<PathBuf>,
    /// Licence file on the board, for the programs that run there. Selects licensed mode for them
    #[arg(long, value_name = "PATH", conflicts_with = "no_licence")]
    pub pi_licence_file: Option<String>,
    /// Say so: run every program unlicensed and let none of them look for a licence file. This is
    /// also what happens when no licence option is given; the bench passes it explicitly
    #[arg(long)]
    pub no_licence: bool,
    /// Keep the licence ID and name in the run files (the bench writes `[redacted]` instead unless
    /// this is given, so the files are safe to commit under results/)
    #[arg(long)]
    pub show_licence: bool,
}

/// Where a program runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Local,
    Pi,
}

/// What one end of the run does about the licence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `--no-licence`
    Unlicensed,
    /// `--licence-file PATH`
    File(String),
}

/// The resolved choice. `Lic::default()` is unlicensed on both ends, values redacted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lic {
    pub local: Mode,
    pub pi: Mode,
    pub redact: bool,
}

impl Default for Lic {
    fn default() -> Self {
        Lic {
            local: Mode::Unlicensed,
            pi: Mode::Unlicensed,
            redact: true,
        }
    }
}

impl LicenceOpts {
    pub fn resolve(&self) -> Lic {
        Lic {
            local: self
                .licence_file
                .as_ref()
                .map_or(Mode::Unlicensed, |p| Mode::File(p.display().to_string())),
            pi: self
                .pi_licence_file
                .clone()
                .map_or(Mode::Unlicensed, Mode::File),
            redact: !self.show_licence,
        }
    }

    /// The gate's arguments for a program the bench runs itself (`factory-bench`).
    pub fn gate_args(&self, transmit: bool) -> GateArgs {
        GateArgs {
            licence_file: self.licence_file.clone(),
            no_licence: self.licence_file.is_none(),
            transmit,
            redact_licence: !self.show_licence,
            ..GateArgs::default()
        }
    }
}

/// True when the user already put `flag` (alone or as `flag=value`) among `args`.
fn given(args: &[String], flag: &str) -> bool {
    args.iter()
        .any(|a| a == flag || a.strip_prefix(flag).is_some_and(|r| r.starts_with('=')))
}

impl Lic {
    /// The flags to add to a program on `side` whose own extra arguments are `args`. Nothing is
    /// added that the user already gave there: a `--licence-file` or `--no-licence` among the
    /// program's arguments wins, and says so in [`Lic::describe`] only as "set by the arguments".
    pub fn flags(&self, side: Side, args: &[String]) -> Vec<String> {
        let mut v = Vec::new();
        if !given(args, "--licence-file") && !given(args, "--no-licence") {
            match match side {
                Side::Local => &self.local,
                Side::Pi => &self.pi,
            } {
                Mode::Unlicensed => v.push("--no-licence".to_string()),
                Mode::File(p) => v.extend(["--licence-file".to_string(), p.clone()]),
            }
        }
        if self.redact && !given(args, "--redact-licence") {
            v.push("--redact-licence".into());
        }
        v
    }

    /// The lines to print before a run: the mode of each end and what the run files will hold.
    pub fn describe(&self) -> String {
        let end = |name: &str, m: &Mode, place: &str| {
            match m {
            Mode::Unlicensed => format!(
                "  {name}: UNLICENSED (--no-licence; no licence file is read, so one on {place} cannot switch the mode)"
            ),
            Mode::File(p) => format!("  {name}: LICENSED with the licence file {p} on {place}"),
        }
        };
        format!(
            "licence mode of this run:\n{}\n{}\n  licence ID and name in the run files: {}",
            end("programs here", &self.local, "this machine"),
            end("programs on the board", &self.pi, "the board"),
            if self.redact {
                "redacted (--redact-licence)"
            } else {
                "SHOWN (--show-licence): do not commit these files"
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn the_default_is_explicitly_unlicensed_and_redacted_on_both_ends() {
        let l = LicenceOpts::default().resolve();
        assert_eq!(l, Lic::default());
        for side in [Side::Local, Side::Pi] {
            assert_eq!(l.flags(side, &[]), s(&["--no-licence", "--redact-licence"]));
        }
    }

    #[test]
    fn a_licence_file_is_passed_to_its_own_end_only() {
        let o = LicenceOpts {
            licence_file: Some("/l/lic.toml".into()),
            pi_licence_file: Some("/p/lic.toml".into()),
            ..LicenceOpts::default()
        };
        let l = o.resolve();
        assert_eq!(
            l.flags(Side::Local, &[]),
            s(&["--licence-file", "/l/lic.toml", "--redact-licence"])
        );
        assert_eq!(
            l.flags(Side::Pi, &[]),
            s(&["--licence-file", "/p/lic.toml", "--redact-licence"])
        );
        let only_pi = LicenceOpts {
            pi_licence_file: Some("/p/lic.toml".into()),
            ..LicenceOpts::default()
        }
        .resolve();
        assert_eq!(only_pi.local, Mode::Unlicensed);
    }

    #[test]
    fn show_licence_drops_the_redaction_flag() {
        let l = LicenceOpts {
            show_licence: true,
            ..LicenceOpts::default()
        }
        .resolve();
        assert_eq!(l.flags(Side::Pi, &[]), s(&["--no-licence"]));
        assert!(l.describe().contains("SHOWN"));
    }

    #[test]
    fn what_the_user_gave_to_the_program_is_not_repeated() {
        let l = Lic::default();
        assert_eq!(
            l.flags(Side::Pi, &s(&["--licence-file=/x", "--redact-licence"])),
            Vec::<String>::new()
        );
        assert_eq!(
            l.flags(Side::Pi, &s(&["--no-licence"])),
            s(&["--redact-licence"])
        );
    }

    #[test]
    fn describe_names_each_end_and_never_a_value() {
        let l = LicenceOpts {
            pi_licence_file: Some("/p/lic.toml".into()),
            ..LicenceOpts::default()
        }
        .resolve();
        let d = l.describe();
        assert!(d.contains("programs here: UNLICENSED"), "{d}");
        assert!(
            d.contains("programs on the board: LICENSED with the licence file /p/lic.toml"),
            "{d}"
        );
        assert!(d.contains("redacted"));
    }

    #[test]
    fn gate_args_are_explicit() {
        let a = LicenceOpts::default().gate_args(true);
        assert!(a.no_licence && a.transmit && a.redact_licence && a.licence_file.is_none());
        let b = LicenceOpts {
            licence_file: Some("/l".into()),
            ..LicenceOpts::default()
        }
        .gate_args(false);
        assert!(!b.no_licence && !b.transmit && b.licence_file.is_some());
    }
}
