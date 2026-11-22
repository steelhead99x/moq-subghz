//! SX1276 backends for [`moq_subghz_link::Link`]. The code was moved here from
//! `moq-subghz-send`, `moq-subghz-recv` and `lora2015-send` as it was; the `Link` impls only
//! adapt its arguments and results. The Pi HAT's spidev and GPIO glue stays in `moq-subghz-pi`.

mod catwan;
mod file;
#[cfg(target_os = "linux")]
mod fsk;
#[cfg(target_os = "linux")]
mod lora;

pub use catwan::CatwanTx;
pub use file::FileSink;
#[cfg(target_os = "linux")]
pub use fsk::{Sx1276FskRx, Sx1276FskTx};
#[cfg(target_os = "linux")]
pub use lora::Sx1276LoraTx;
