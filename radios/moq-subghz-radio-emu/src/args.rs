//! Command line flags shared by the sender, the receiver and the offline channel tool. Every
//! default is documented in `docs/HALOW.md` as an ASSUMPTION or a SOURCE.
use crate::loss::LossModel;
use crate::model::{EmuCfg, FskCfg, HalowCfg, LoraCfg, PhyKind};
use clap::{Args, ValueEnum};

/// Which modeled channel to run.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum EmuSel {
    /// SX1276 FSK airtime and loss model, no radio.
    #[value(name = "fsk-emu")]
    Fsk,
    /// SX1276 LoRa airtime and loss model, no radio.
    #[value(name = "lora-emu")]
    Lora,
    /// 802.11ah S1G (HaLow) channel model, no radio.
    #[value(name = "halow-emu")]
    Halow,
}

/// Emulation flags.
#[derive(Args, Clone, Debug)]
pub struct EmuArgs {
    /// HaLow channel width in MHz (1, 2, 4, 8, 16)
    #[arg(long, default_value_t = 1)]
    pub halow_bw: u8,
    /// HaLow S1G MCS 0..=10 (10 only at 1 MHz)
    #[arg(long, default_value_t = 0)]
    pub halow_mcs: u8,
    /// HaLow PHY payload limit in bytes
    #[arg(long, default_value_t = 1472)]
    pub halow_mtu: usize,
    /// Per-packet header bytes on air (MAC + LLC + IP + UDP)
    #[arg(long, default_value_t = 64)]
    pub halow_overhead: usize,
    /// Most wire frames per HaLow packet (0 = as many as fit, 1 = no packing)
    #[arg(long, default_value_t = 0)]
    pub halow_pack: usize,
    /// Broadcast mode: no ACK, no retry (default is unicast with ACK and 7 retries)
    #[arg(long)]
    pub halow_bcast: bool,
    /// MAC retransmissions after the first attempt (unicast)
    #[arg(long, default_value_t = 7)]
    pub halow_retries: u32,
    /// Receive path latency in microseconds (driver and stack)
    #[arg(long, default_value_t = 1500)]
    pub halow_latency_us: u64,
    /// Receive jitter, uniform 0..=N microseconds
    #[arg(long, default_value_t = 1000)]
    pub halow_jitter_us: u64,
    /// Loss per PHY attempt: none | iid:P | p_gb:p_bg:loss_good:loss_bad (Gilbert-Elliott)
    #[arg(long, default_value = "none")]
    pub emu_loss: String,
    /// Seed of the channel noise
    #[arg(long, default_value_t = 1)]
    pub emu_seed: u64,
    /// LoRa spreading factor 6..=12
    #[arg(long, default_value_t = 7)]
    pub lora_sf: u8,
    /// LoRa bandwidth in Hz
    #[arg(long, default_value_t = 500_000)]
    pub lora_bw: u32,
    /// LoRa coding rate 1..=4 (4/5..4/8)
    #[arg(long, default_value_t = 1)]
    pub lora_cr: u8,
    /// FSK bit rate of the modeled FSK channel
    #[arg(long, default_value_t = 300_000)]
    pub fsk_bitrate: u32,
    /// FSK preamble bytes of the modeled FSK channel
    #[arg(long, default_value_t = 6)]
    pub fsk_preamble: u16,
}

impl EmuArgs {
    /// Build and validate the model settings for `sel`.
    pub fn to_cfg(&self, sel: EmuSel) -> Result<EmuCfg, String> {
        let kind = match sel {
            EmuSel::Fsk => PhyKind::Fsk(FskCfg {
                bitrate_bps: self.fsk_bitrate,
                preamble_bytes: self.fsk_preamble,
            }),
            EmuSel::Lora => PhyKind::Lora(LoraCfg {
                sf: self.lora_sf,
                bw_hz: self.lora_bw,
                cr: self.lora_cr,
                ..LoraCfg::default()
            }),
            EmuSel::Halow => PhyKind::Halow(HalowCfg {
                bw_mhz: self.halow_bw,
                mcs: self.halow_mcs,
                mtu: self.halow_mtu,
                overhead: self.halow_overhead,
                ack: !self.halow_bcast,
                retries: self.halow_retries,
                proc_latency_us: self.halow_latency_us,
                jitter_us: self.halow_jitter_us,
                ..HalowCfg::default()
            }),
        };
        let mut c = EmuCfg::new(kind);
        c.loss = LossModel::parse(&self.emu_loss)?;
        c.seed = self.emu_seed;
        c.max_frames = self.halow_pack;
        c.validate()?;
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct P {
        #[command(flatten)]
        e: EmuArgs,
    }

    #[test]
    fn defaults_are_the_documented_assumptions() {
        let p = P::parse_from(["x"]);
        let c = p.e.to_cfg(EmuSel::Halow).unwrap();
        assert_eq!(c.kind, PhyKind::Halow(HalowCfg::default()));
        assert_eq!(c.max_frames_per_packet(), 5);
        assert_eq!(
            p.e.to_cfg(EmuSel::Fsk).unwrap().kind,
            PhyKind::Fsk(FskCfg::default())
        );
        assert_eq!(
            p.e.to_cfg(EmuSel::Lora).unwrap().kind,
            PhyKind::Lora(LoraCfg::default())
        );
    }

    #[test]
    fn bad_settings_are_errors_not_panics() {
        for a in [
            &["x", "--halow-bw", "3"][..],
            &["x", "--halow-bw", "2", "--halow-mcs", "10"],
            &["x", "--emu-loss", "iid:2"],
            &["x", "--halow-mtu", "100"],
            &["x", "--halow-retries", "4294967295"],
            &["x", "--halow-overhead", "18446744073709551615"],
            &["x", "--halow-latency-us", "18446744073709551615"],
            &["x", "--halow-jitter-us", "18446744073709551615"],
        ] {
            assert!(P::parse_from(a).e.to_cfg(EmuSel::Halow).is_err(), "{a:?}");
        }
        assert!(P::parse_from(["x", "--lora-sf", "5"])
            .e
            .to_cfg(EmuSel::Lora)
            .is_err());
        assert!(P::parse_from(["x", "--fsk-bitrate", "0"])
            .e
            .to_cfg(EmuSel::Fsk)
            .is_err());
    }
}
