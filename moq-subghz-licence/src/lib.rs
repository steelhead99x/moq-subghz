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
//! This crate transmits nothing. [`check_transmit`] is a gate that the send/recv tools call
//! before they key a radio. Not legal advice; you are responsible for what you transmit.

pub mod band;
pub mod config;
pub mod gate;
pub mod ident;
pub mod region;

pub use band::{Band, BandKind};
pub use config::{ConfigError, LicenceConfig};
pub use gate::{check_start, check_transmit, Auth, Encryption, Flags, Permit, Profile, Refusal};
pub use region::{IdRule, Region};

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
