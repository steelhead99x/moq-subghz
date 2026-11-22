//! Emulated links for [`moq_subghz_link::Link`]: SX1276 FSK, SX1276 CSS mode and an IEEE 802.11ah
//! ("802.11ah") channel, as seeded models in virtual time.
//!
//! * [`model`]: the channel models and their settings ([`model::EmuCfg`], [`model::Channel`]).
//! * [`link::EmuLink`]: the `Link` backend over a model, with in-memory, UDP and file transports.
//! * [`outfile`]: refuse an output that is the input and write outputs atomically.
//! * [`pack`]: several 255 B wire frames in one packet, for links whose packet is longer.
//! * [`s1g`], [`loss`]: the S1G rate table and the loss and noise models.
//! * [`args`]: the command line flags shared by the sender, the receiver and `moq-subghz-chan`.
//!
//! NOTHING HERE TALKS TO HARDWARE and everything it reports is MODELED, never measured. Every
//! 802.11ah parameter is labelled SOURCE or ASSUMPTION in its doc comment and in `docs/S1G-EMULATION.md`.

pub mod args;
pub mod link;
pub mod loss;
pub mod model;
pub mod outfile;
pub mod pack;
pub mod s1g;

pub use link::EmuLink;
pub use model::{Channel, CssCfg, EmuCfg, FskCfg, Outcome, PhyKind, S1gCfg};
