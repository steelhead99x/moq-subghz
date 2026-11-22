//! SX1276 backends for [`moq_subghz_link::Link`]. The code was moved here from
//! `moq-subghz-send`, `moq-subghz-recv` and `lora2015-send` as it was; the `Link` impls only
//! adapt its arguments and results. Every `open` takes the gate's permit (`TxPermit` to transmit,
//! `RxPermit` to receive), so opening a radio without asking the gate does not compile.
//!
//! The Pi HAT's spidev and GPIO glue stays in `moq-subghz-pi`, below this gate.

//! With the permit the call compiles; without one, or with a receive permit where a transmit
//! one is needed, it does not:
//!
//! ```no_run
//! use moq_subghz_licence::TxPermit;
//! use moq_subghz_radio_sx1276::CatwanTx;
//! fn open(permit: &TxPermit, p: &moq_subghz_wire::FskProfile) {
//!     let _ = CatwanTx::open(permit, "/dev/ttyACM0", 915_000_000, p, 5, 7_093);
//! }
//! ```
//!
//! ```compile_fail
//! use moq_subghz_radio_sx1276::CatwanTx;
//! fn open(p: &moq_subghz_wire::FskProfile) {
//!     let _ = CatwanTx::open("/dev/ttyACM0", 915_000_000, p, 5, 7_093);
//! }
//! ```
//!
//! ```compile_fail
//! use moq_subghz_licence::RxPermit;
//! use moq_subghz_radio_sx1276::CatwanTx;
//! fn open(rx: &RxPermit, p: &moq_subghz_wire::FskProfile) {
//!     let _ = CatwanTx::open(rx, "/dev/ttyACM0", 915_000_000, p, 5, 7_093);
//! }
//! ```

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

/// The permit must cover the carrier being opened: the gate checked a span, and an `open` that
/// tunes elsewhere would make the permit say something false.
pub(crate) fn require_covered(covers: impl Fn(u64) -> bool, freq_hz: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        covers(u64::from(freq_hz)),
        "refusing to open the radio at {freq_hz} Hz: the licence gate did not check that frequency"
    );
    Ok(())
}
