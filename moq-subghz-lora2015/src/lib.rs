//! A 2015-style LoRa image test for the moq-subghz bench: what a small team could have sent
//! over LoRa and LoRaWAN with the Pi HAT and an RP2040-based device, and how to log it.
//!
//! The sender (`lora2015-send`) runs on the Pi. The logger (`lora2015-log`) runs on the
//! host the stick is plugged into.

pub mod chunk;
pub mod log;
pub mod lorawan;
pub mod plan;
pub mod profile;
pub mod ssdv;
pub mod stick;
