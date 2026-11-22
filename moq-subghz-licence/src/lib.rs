//! Licensed vs unlicensed operating mode for moq-subghz.
//!
//! * [`Mode::Unlicensed`]: licence-free ISM operation only. Licensed-only bands are blocked.
//! * [`Mode::Licensed`]: operation under a radio licence (amateur or other licensed bands).
//!   Refuses to start, transmit or enable amateur profiles unless BOTH a licence ID and a name
//!   are configured.
//!
//! The tools check the shape of a licence ID only. They cannot verify it against any database,
//! and a plausible ID is not proof that you hold a licence. See `docs/LICENSING.md`.
//!
//! This crate transmits nothing. [`check_transmit`] is the gate that `moq-subghz-send`,
//! `moq-subghz-recv` and `css2015-send` pass before they open a radio, through [`GateArgs`].
//! The RP2040-based device firmware images shipped in this repository do not enforce it; see
//! `docs/LICENSING.md`. Not legal advice; you are responsible for what you transmit.

pub mod band;
pub mod cli;
pub mod config;
pub mod gate;
pub mod ident;
pub mod region;
#[cfg(feature = "testing")]
pub mod testing;

pub use band::{Band, BandKind, Tuning};
pub use cli::{GateArgs, Station};
pub use config::{ConfigError, LicenceConfig};
pub use gate::{
    check_start, check_transmit, Auth, Encryption, Flags, Profile, Refusal, RxPermit, TxPermit,
};
pub use ident::{IdPlan, IdScheduler};
pub use region::Region;

/// Operating mode. The default is [`Mode::Unlicensed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Licence-free ISM operation only.
    #[default]
    Unlicensed,
    /// Operation under a radio licence; needs a valid licence ID and name.
    Licensed,
}

/// Text used in place of the licence ID and name when redacting.
pub const REDACTED: &str = "[redacted]";

/// Whether log lines and reports show the licence ID and name. There is no `bool` form, so a
/// call site says which it means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Redaction {
    /// Write the values (the default, as the owner chose; `--redact-licence` turns it off).
    #[default]
    Show,
    /// Write [`REDACTED`] instead.
    Redact,
}
