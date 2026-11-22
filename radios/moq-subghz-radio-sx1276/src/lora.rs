//! SX1276 on the Dragino HAT in LoRa mode, transmit only. Moved from `lora2015-send`'s
//! `transmit`; the profile, planning and pacing stay in `moq-subghz-lora2015`.

use anyhow::{bail, Result};
use moq_subghz_licence::TxPermit;
use moq_subghz_link::{
    check_packet, Link, LinkCaps, LinkError, LinkKind, Provenance, RxMeta, TxReport,
};
use moq_subghz_pi::{open_hat_raw, sx_err, Hat};
use std::time::{Duration, Instant};
use sx1276::{LoRaConfig, PaOutput};

pub struct Sx1276LoraTx {
    r: Hat,
    cfg: LoRaConfig,
    caps: LinkCaps,
}

impl Sx1276LoraTx {
    /// Open the HAT, configure LoRa mode at `freq_hz` and set PA_BOOST to `power` dBm. Check
    /// the modem with [`Self::modem_config`] before sending.
    /// `permit` is the gate's answer for this run; `freq_hz` must lie inside the span it checked.
    pub fn open(
        permit: &TxPermit,
        spi_hz: u32,
        freq_hz: u32,
        cfg: &LoRaConfig,
        power: i8,
    ) -> Result<Self> {
        crate::require_covered(|hz| permit.covers_hz(hz), freq_hz)?;
        let mut r = open_hat_raw(spi_hz)?;
        r.init(freq_hz, cfg).map_err(sx_err)?;
        r.set_tx_power(PaOutput::PaBoost, power).map_err(sx_err)?;
        if r.frequency().map_err(sx_err)?.abs_diff(freq_hz) > 1000 {
            bail!("frequency readback mismatch");
        }
        Ok(Sx1276LoraTx {
            r,
            cfg: *cfg,
            caps: LinkCaps {
                name: "sx1276-lora-tx",
                kind: LinkKind::Lora,
                max_payload: moq_subghz_wire::MAX_FRAME,
                max_frames_per_packet: 1,
                tx: true,
                rx: false,
                rx_needs_poll: false,
                reports_rssi: false,
                reports_snr: false,
                provenance: Provenance::Measured,
            },
        })
    }

    /// The radio's ModemConfig1..3 registers, to compare with what `open` wrote.
    pub fn modem_config(&mut self) -> Result<[u8; 3]> {
        let mut out = [0u8; 3];
        for (o, reg) in out.iter_mut().zip([
            sx1276::Register::ModemConfig1,
            sx1276::Register::ModemConfig2,
            sx1276::Register::ModemConfig3,
        ]) {
            *o = self.r.read_register(reg).map_err(sx_err)?;
        }
        Ok(out)
    }
}

impl Link for Sx1276LoraTx {
    fn caps(&self) -> &LinkCaps {
        &self.caps
    }

    fn send(&mut self, f: &[u8]) -> Result<TxReport, LinkError> {
        check_packet(self.caps.max_payload, f)?;
        let toa = self.cfg.time_on_air_us(f.len() as u8);
        let t = Instant::now();
        self.r
            .transmit(f, toa.saturating_mul(3).saturating_add(200_000))
            .map_err(|e| LinkError::Io(sx_err(e).to_string()))?;
        Ok(TxReport {
            busy_us: t.elapsed().as_micros() as u64,
            airtime_us: toa as u64,
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
        self.r
            .sleep()
            .map_err(|e| LinkError::Io(sx_err(e).to_string()))
    }
}

/// Best effort: sleep the radio if `idle` did not. Only the operating mode is written; nothing
/// here transmits, and a drop has nowhere to send the error.
impl Drop for Sx1276LoraTx {
    fn drop(&mut self) {
        let _ = self.r.sleep();
    }
}
