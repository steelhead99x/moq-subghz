//! A 2015-style CSS mode image test for the moq-subghz bench: what a small team could have sent
//! over CSS mode and framed with the board HAT and an RP2040-based device, and how to log it.
//!
//! The sender (`css2015-send`) runs on the board. The logger (`css2015-log`) runs on the
//! host the stick is plugged into.

pub mod chunk;
pub mod framed;
pub mod log;
pub mod plan;
pub mod profile;
pub mod ssdv;
pub mod stick;
