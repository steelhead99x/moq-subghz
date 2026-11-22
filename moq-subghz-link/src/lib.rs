//! One trait for every radio the tools can use: send a packet, wait for a packet, and say what the
//! link can do. Backends (the SX1276 on the Pi HAT, the RP2040-based device, emulations, replay files)
//! implement [`Link`]; the sender and receiver talk only to it.
//!
//! The trait is synchronous: the sender is a blocking loop and the receiver already runs a
//! polling thread. It carries bytes and reports timing and outcome, so one trait serves a real
//! radio and a model of one. A link never transmits unless the caller built it to and called
//! [`Link::send`]; nothing here opens hardware.

pub mod conformance;
mod loopback;

use std::fmt;
use std::time::Duration;

pub use loopback::{pair, LoopbackCfg, LoopbackLink, Loss};

/// The largest packet a wire v5 frame needs: [`moq_subghz_wire::MAX_FRAME`].
pub const MAX_FRAME: usize = moq_subghz_wire::MAX_FRAME;

/// The checks every [`Link::send`] makes first: a packet is 1..=`max_payload` bytes. Backends call
/// this so an empty or oversize packet is refused the same way everywhere, before anything is
/// queued or keyed.
pub fn check_packet(max_payload: usize, packet: &[u8]) -> Result<(), LinkError> {
    if packet.is_empty() {
        Err(LinkError::Empty)
    } else if packet.len() > max_payload {
        Err(LinkError::TooLong)
    } else {
        Ok(())
    }
}

/// What kind of channel a link is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// SX1276 (or similar) in FSK mode.
    Fsk,
    /// CSS mode modulation.
    Css,
    /// 802.11ah.
    S1g,
    /// A modeled channel in memory or a socket.
    Emu,
    /// Frames read back from a recorded file.
    Replay,
    /// A stick that speaks the serial `Q`/`R` protocol.
    Serial,
}

/// Where a link's timings and outcomes come from. Reports carry it so a modeled number is never
/// shown as a measured one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// A real radio on real hardware.
    Measured,
    /// A model or emulation.
    Modeled,
    /// A recording of an earlier run.
    Replay,
}

/// What a link can do. Fixed for the life of the link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkCaps {
    /// Short name for logs and summaries, such as `sx1276-fsk`.
    pub name: &'static str,
    pub kind: LinkKind,
    /// Largest packet [`Link::send`] accepts and [`Link::recv`] can return, in bytes.
    pub max_payload: usize,
    /// How many wire frames one packet may hold; 1 means no packing.
    pub max_frames_per_packet: usize,
    pub tx: bool,
    pub rx: bool,
    /// Receiving busy-polls inside `recv` (the Pi HAT wires no DIO1), so a waiting `recv` uses a core.
    pub rx_needs_poll: bool,
    pub reports_rssi: bool,
    pub reports_snr: bool,
    pub provenance: Provenance,
}

/// What one [`Link::send`] cost and, when the link can tell, what became of the packet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TxReport {
    /// Time the channel was occupied: airtime plus backoff, ACK wait and retries. A link that
    /// cannot observe this reports the computed airtime (see its docs), never 0 for a send that
    /// went out.
    pub busy_us: u64,
    /// On-air time of the attempts only; duty-cycle accounting counts this. Measured or computed
    /// from the radio settings, per link; 0 only for a link with no air.
    pub airtime_us: u64,
    pub attempts: u32,
    /// Whether the far side got at least one copy. `None` when the link cannot know (a real
    /// radio). A model knows this even when the sender would not: see `acked`.
    pub delivered: Option<bool>,
    /// Whether the sender saw an ACK, which is all a real sender can know. `None` when no ACK
    /// exists (broadcast, or a link without one). With ACKs on, `delivered == Some(true)` and
    /// `acked == Some(false)` means the data arrived but every ACK was lost.
    pub acked: Option<bool>,
    /// Emulations only: when the packet reaches the receiver, after the send started.
    pub deliver_after_us: Option<u64>,
}

/// A received packet: its bytes are in the buffer given to [`Link::recv`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RxMeta {
    /// Packet length in bytes; the packet is `buf[..len]`.
    pub len: usize,
    /// Arrival time in microseconds on the link's own clock.
    pub t_us: u64,
    pub rssi_dbm: Option<i16>,
    pub snr_db: Option<f32>,
}

/// Why a link call failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// The link needs settings it was not given.
    NotConfigured,
    /// The packet is longer than the link's `max_payload`, or longer than the receive buffer.
    TooLong,
    /// A zero-length packet; there is nothing to send.
    Empty,
    /// A packet arrived with a bad CRC.
    Crc,
    /// The channel is busy and the packet was not sent.
    Busy,
    /// A band, power or duty-cycle rule refused the packet.
    Regulatory(String),
    /// The device or the other end failed.
    Io(String),
    /// The link cannot do this (send on a receive-only link, or the reverse).
    Unsupported,
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::NotConfigured => f.write_str("link not configured"),
            LinkError::TooLong => f.write_str("packet too long"),
            LinkError::Empty => f.write_str("empty packet"),
            LinkError::Crc => f.write_str("bad CRC"),
            LinkError::Busy => f.write_str("channel busy"),
            LinkError::Regulatory(m) => write!(f, "refused by a regulatory limit: {m}"),
            LinkError::Io(m) => f.write_str(m),
            LinkError::Unsupported => f.write_str("not supported by this link"),
        }
    }
}

impl std::error::Error for LinkError {}

/// A radio, or a model of one, that moves packets.
pub trait Link: Send {
    fn caps(&self) -> &LinkCaps;

    /// Send one packet and return what it cost. Blocks until the packet has left the radio.
    /// An empty packet is `Empty` and one over `caps().max_payload` is `TooLong`
    /// ([`check_packet`]); a refused or failed send leaves nothing queued and is not a send.
    fn send(&mut self, packet: &[u8]) -> Result<TxReport, LinkError>;

    /// Wait up to `timeout` for one packet and copy it to `buf`. `Ok(None)` is a timeout.
    /// A backend that must poll does so inside this call.
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Result<Option<RxMeta>, LinkError>;

    /// Finish up and put the radio in standby: flush files, stop a stick, drain its queue.
    /// Every backend must leave the radio not transmitting, and must say so when it cannot:
    /// a failure to stop is an `Err`, never swallowed. Dropping a real radio does the same
    /// best-effort stop, silently.
    fn idle(&mut self) -> Result<(), LinkError> {
        Ok(())
    }

    /// Frames the link handed to its radio but could not confirm went on air, as of the last
    /// `idle` (it stops the radio, which drops what was still queued). Returns the count once
    /// and resets it. A caller must not count these as sent. 0 for a link that knows each
    /// frame's fate.
    fn unconfirmed_frames(&mut self) -> u64 {
        0
    }

    /// Transmit errors the link learned of since the last call, for links that hear about them
    /// after `send` returned (the RP2040-based device reports `ERR tx` asynchronously).
    fn take_tx_errors(&mut self) -> u64 {
        0
    }
}
