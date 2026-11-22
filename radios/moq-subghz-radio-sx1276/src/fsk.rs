//! SX1276 on the Dragino HAT in FSK mode. The HAT glue is in `moq-subghz-pi`.
//! Moved from `moq-subghz-send/src/radio.rs` and `moq-subghz-recv/src/radio.rs`.

use anyhow::{bail, Result};
use moq_subghz_licence::{RxPermit, TxPermit};
use moq_subghz_link::{
    check_packet, Link, LinkCaps, LinkError, LinkKind, Provenance, RxMeta, TxReport,
};
use moq_subghz_pi::{open_hat, sx_err, Hat};
use std::time::{Duration, Instant};
use sx1276::fsk::FskConfig;

fn caps(name: &'static str, tx: bool) -> LinkCaps {
    LinkCaps {
        name,
        kind: LinkKind::Fsk,
        max_payload: moq_subghz_wire::MAX_FRAME,
        max_frames_per_packet: 1,
        tx,
        rx: !tx,
        // the HAT wires no DIO1, so receive busy-polls the FIFO
        rx_needs_poll: !tx,
        reports_rssi: !tx,
        reports_snr: false,
        provenance: Provenance::Measured,
    }
}

fn io(e: impl std::fmt::Display) -> LinkError {
    LinkError::Io(e.to_string())
}

pub struct Sx1276FskTx {
    r: Hat,
    toa_us: u32,
    cfg: FskConfig,
    caps: LinkCaps,
}

impl Sx1276FskTx {
    /// Open the HAT for transmitting. `permit` is the gate's answer for this run; `freq_hz` must
    /// lie inside the span it checked.
    pub fn open(
        permit: &TxPermit,
        spi_hz: u32,
        freq_hz: u32,
        cfg: &FskConfig,
        power: i8,
    ) -> Result<Self> {
        crate::require_covered(|hz| permit.covers_hz(hz), freq_hz)?;
        Ok(Sx1276FskTx {
            r: open_hat(spi_hz, freq_hz, cfg, power)?,
            toa_us: cfg.time_on_air_us(255),
            cfg: *cfg,
            caps: caps("sx1276-fsk-tx", true),
        })
    }

    /// Transmit one frame and wait for PacketSent.
    fn transmit(&mut self, f: &[u8]) -> Result<()> {
        if f.len() > 63 {
            self.r.fsk_transmit_long(f, 1_000_000).map_err(sx_err)?;
        } else {
            self.r.fsk_start_transmit(f).map_err(sx_err)?;
        }
        if !self
            .r
            .wait_dio0(self.toa_us * 3 + 200_000)
            .map_err(sx_err)?
        {
            bail!("TX timeout");
        }
        if !self.r.fsk_finish_transmit().map_err(sx_err)? {
            bail!("PacketSent flag missing");
        }
        Ok(())
    }
}

impl Link for Sx1276FskTx {
    fn caps(&self) -> &LinkCaps {
        &self.caps
    }

    fn send(&mut self, f: &[u8]) -> Result<TxReport, LinkError> {
        check_packet(self.caps.max_payload, f)?;
        let t = Instant::now();
        self.transmit(f).map_err(io)?;
        Ok(TxReport {
            busy_us: t.elapsed().as_micros() as u64,
            airtime_us: self.cfg.time_on_air_us(f.len()) as u64,
            attempts: 1,
            delivered: None,
            acked: None,
            deliver_after_us: None,
        })
    }

    fn recv(&mut self, _: &mut [u8], _: Duration) -> Result<Option<RxMeta>, LinkError> {
        Err(LinkError::Unsupported)
    }

    fn idle(&mut self) -> Result<(), LinkError> {
        standby(&mut self.r)
    }
}

/// Best effort: the radio goes to standby if `idle` did not get there. `set_mode` only writes the
/// operating mode, so nothing here transmits; the error has nowhere to go from a drop.
impl Drop for Sx1276FskTx {
    fn drop(&mut self) {
        let _ = self.r.standby();
    }
}

impl Drop for Sx1276FskRx {
    fn drop(&mut self) {
        let _ = self.r.standby();
    }
}

fn standby(r: &mut Hat) -> Result<(), LinkError> {
    r.standby()
        .map_err(|e| LinkError::Io(sx_err(e).to_string()))
}

/// FSK receive on the HAT (reverse direction: CatWAN to Pi).
pub struct Sx1276FskRx {
    r: Hat,
    st: sx1276::FskLongRx,
    stall_us: u32,
    t0: Instant,
    caps: LinkCaps,
}

impl Sx1276FskRx {
    /// Open the HAT and enter continuous FSK receive. `cfg.crc_auto_clear` must be off:
    /// the long-packet reader drains the FIFO mid-packet.
    /// `permit` is the gate's receive permit for this run; `freq_hz` must lie inside the span it
    /// checked.
    pub fn open(
        permit: &RxPermit,
        spi_hz: u32,
        freq_hz: u32,
        cfg: &FskConfig,
        power: i8,
    ) -> Result<Self> {
        crate::require_covered(|hz| permit.covers_hz(hz), freq_hz)?;
        let mut r = open_hat(spi_hz, freq_hz, cfg, power)?;
        r.fsk_start_receive().map_err(sx_err)?;
        Ok(Sx1276FskRx {
            r,
            st: sx1276::FskLongRx::default(),
            stall_us: 36 * 8_000_000 / cfg.bitrate_bps,
            t0: Instant::now(),
            caps: caps("sx1276-fsk-rx", false),
        })
    }

    /// The poll clock now: microseconds since `open`, wrapping at 32 bits.
    pub fn now_us(&self) -> u32 {
        self.t0.elapsed().as_micros() as u32
    }
}

impl Link for Sx1276FskRx {
    fn caps(&self) -> &LinkCaps {
        &self.caps
    }

    fn send(&mut self, _: &[u8]) -> Result<TxReport, LinkError> {
        Err(LinkError::Unsupported)
    }

    /// Busy-poll the FIFO for frames > 63 B (the HAT wires no DIO1) until a frame arrives or
    /// `timeout` passes. A CRC or FIFO error is `Err(Io)`: the caller counts it and goes on.
    /// `t_us` is microseconds since `open`, as a wrapping 32-bit count like the poll's clock.
    fn recv(&mut self, buf: &mut [u8], timeout: Duration) -> Result<Option<RxMeta>, LinkError> {
        // Duration::MAX is "no deadline", not an overflow panic
        let deadline = Instant::now().checked_add(timeout);
        loop {
            let now = self.t0.elapsed().as_micros() as u32;
            match self.r.fsk_poll_long(&mut self.st, now, self.stall_us) {
                Ok(Some(p)) => {
                    let payload = self.st.payload();
                    let Some(dst) = buf.get_mut(..payload.len()) else {
                        return Err(LinkError::TooLong);
                    };
                    dst.copy_from_slice(payload);
                    return Ok(Some(RxMeta {
                        len: payload.len(),
                        t_us: now as u64,
                        rssi_dbm: Some(p.rssi_dbm),
                        snr_db: None,
                    }));
                }
                Ok(None) => {
                    if deadline.is_some_and(|d| Instant::now() >= d) {
                        return Ok(None);
                    }
                }
                Err(x) => return Err(LinkError::Io(format!("{x:?}"))),
            }
        }
    }

    fn idle(&mut self) -> Result<(), LinkError> {
        standby(&mut self.r)
    }
}
