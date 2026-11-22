//! Command line flags shared by the sender, the receiver and the offline channel tool. Every
//! default is documented in `docs/S1G-EMULATION.md` as an ASSUMPTION or a SOURCE.
use crate::loss::LossModel;
use crate::model::{CssCfg, EmuCfg, FskCfg, PhyKind, S1gCfg};
use clap::{Args, ValueEnum};

/// Which modeled channel to run.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum EmuSel {
    /// SX1276 FSK airtime and loss model, no radio.
    #[value(name = "fsk-emu")]
    Fsk,
    /// SX1276 CSS mode airtime and loss model, no radio.
    #[value(name = "css-emu")]
    Css,
    /// 802.11ah S1G channel model, no radio.
    #[value(name = "s1g-emu")]
    S1g,
}

/// Emulation flags.
#[derive(Args, Clone, Debug)]
pub struct EmuArgs {
    /// 802.11ah channel width in MHz (1, 2, 4, 8, 16)
    #[arg(long, default_value_t = 1)]
    pub s1g_bw: u8,
    /// 802.11ah S1G MCS 0..=10 (10 only at 1 MHz)
    #[arg(long, default_value_t = 0)]
    pub s1g_mcs: u8,
    /// 802.11ah PHY payload limit in bytes
    #[arg(long, default_value_t = 1472)]
    pub s1g_mtu: usize,
    /// Per-packet header bytes on air (MAC + LLC + IP + UDP)
    #[arg(long, default_value_t = 64)]
    pub s1g_overhead: usize,
    /// Most wire frames per 802.11ah packet (0 = as many as fit, 1 = no packing)
    #[arg(long, default_value_t = 0)]
    pub s1g_pack: usize,
    /// Broadcast mode: no ACK, no retry (default is unicast with ACK and 7 retries)
    #[arg(long)]
    pub s1g_bcast: bool,
    /// MAC retransmissions after the first attempt (unicast)
    #[arg(long, default_value_t = 7)]
    pub s1g_retries: u32,
    /// Receive path latency in microseconds (driver and stack)
    #[arg(long, default_value_t = 1500)]
    pub s1g_latency_us: u64,
    /// Receive jitter, uniform 0..=N microseconds
    #[arg(long, default_value_t = 1000)]
    pub s1g_jitter_us: u64,
    /// Loss per PHY attempt: none | iid:P | p_gb:p_bg:loss_good:loss_bad (Gilbert-Elliott)
    #[arg(long, default_value = "none")]
    pub emu_loss: String,
    /// Seed of the channel noise
    #[arg(long, default_value_t = 1)]
    pub emu_seed: u64,
    /// CSS mode spreading factor 6..=12
    #[arg(long, default_value_t = 7)]
    pub css_sf: u8,
    /// CSS mode bandwidth in Hz
    #[arg(long, default_value_t = 500_000)]
    pub css_bw: u32,
    /// CSS mode coding rate 1..=4 (4/5..4/8)
    #[arg(long, default_value_t = 1)]
    pub css_cr: u8,
    /// CSS mode implicit header: no header on air, both ends know the length (SF6 needs it)
    #[arg(long)]
    pub css_implicit: bool,
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
            EmuSel::Css => PhyKind::Css(CssCfg {
                sf: self.css_sf,
                bw_hz: self.css_bw,
                cr: self.css_cr,
                explicit_header: !self.css_implicit,
                ..CssCfg::default()
            }),
            EmuSel::S1g => PhyKind::S1g(S1gCfg {
                bw_mhz: self.s1g_bw,
                mcs: self.s1g_mcs,
                mtu: self.s1g_mtu,
                overhead: self.s1g_overhead,
                ack: !self.s1g_bcast,
                retries: self.s1g_retries,
                proc_latency_us: self.s1g_latency_us,
                jitter_us: self.s1g_jitter_us,
                ..S1gCfg::default()
            }),
        };
        let mut c = EmuCfg::new(kind);
        c.loss = LossModel::parse(&self.emu_loss)?;
        c.seed = self.emu_seed;
        c.max_frames = self.s1g_pack;
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
        let c = p.e.to_cfg(EmuSel::S1g).unwrap();
        assert_eq!(c.kind, PhyKind::S1g(S1gCfg::default()));
        assert_eq!(c.max_frames_per_packet(), 5);
        assert_eq!(
            p.e.to_cfg(EmuSel::Fsk).unwrap().kind,
            PhyKind::Fsk(FskCfg::default())
        );
        assert_eq!(
            p.e.to_cfg(EmuSel::Css).unwrap().kind,
            PhyKind::Css(CssCfg::default())
        );
    }

    #[test]
    fn bad_settings_are_errors_not_panics() {
        for a in [
            &["x", "--s1g-bw", "3"][..],
            &["x", "--s1g-bw", "2", "--s1g-mcs", "10"],
            &["x", "--emu-loss", "iid:2"],
            &["x", "--s1g-mtu", "100"],
            &["x", "--s1g-retries", "4294967295"],
            &["x", "--s1g-overhead", "18446744073709551615"],
            &["x", "--s1g-latency-us", "18446744073709551615"],
            &["x", "--s1g-jitter-us", "18446744073709551615"],
        ] {
            assert!(P::parse_from(a).e.to_cfg(EmuSel::S1g).is_err(), "{a:?}");
        }
        // SF6 only works with the implicit header
        assert!(P::parse_from(["x", "--css-sf", "6"])
            .e
            .to_cfg(EmuSel::Css)
            .is_err());
        let c = P::parse_from(["x", "--css-sf", "6", "--css-implicit"])
            .e
            .to_cfg(EmuSel::Css)
            .unwrap();
        assert!(matches!(c.kind, PhyKind::Css(l) if l.sf == 6 && !l.explicit_header));
        assert!(P::parse_from(["x", "--css-sf", "5"])
            .e
            .to_cfg(EmuSel::Css)
            .is_err());
        assert!(P::parse_from(["x", "--fsk-bitrate", "0"])
            .e
            .to_cfg(EmuSel::Fsk)
            .is_err());
    }

    #[test]
    fn only_the_neutral_spellings_parse() {
        use clap::ValueEnum;
        let p = P::parse_from(["x", "--s1g-bw", "4", "--css-sf", "9", "--s1g-bcast"]);
        assert_eq!((p.e.s1g_bw, p.e.css_sf, p.e.s1g_bcast), (4, 9, true));
        assert_eq!(EmuSel::from_str("css-emu", false).unwrap(), EmuSel::Css);
        assert_eq!(EmuSel::from_str("s1g-emu", false).unwrap(), EmuSel::S1g);
    }
}
