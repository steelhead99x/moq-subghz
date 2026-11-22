//! The channel models: SX1276 FSK, SX1276 LoRa and 802.11ah HaLow, as pure seeded functions of
//! virtual time. Nothing here sleeps, opens a socket or touches hardware.
//!
//! Every constant is labelled SOURCE (a standard, datasheet or file in this repository) or
//! ASSUMPTION. The result is MODELED: it is never a measurement. `docs/HALOW.md` lists every
//! parameter with its label.
use crate::loss::{LossModel, LossState, Rng};
use crate::{pack, s1g};
use moq_subghz_wire::MAX_FRAME;

/// What one packet cost in the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    /// Microseconds the sender's channel was occupied: backoff, every attempt, ACK waits.
    pub busy_us: u64,
    /// On-air time of the data attempts only (what duty-cycle accounting counts).
    pub airtime_us: u64,
    /// At least one attempt arrived.
    pub delivered: bool,
    /// From the start of the transmission to the moment the far side has it.
    pub deliver_after_us: u64,
    /// PHY attempts made (1 without retries).
    pub attempts: u32,
}

/// HaLow MAC/PHY parameters. Defaults are ASSUMPTIONS unless a SOURCE is named.
#[derive(Clone, Debug, PartialEq)]
pub struct HalowCfg {
    /// SOURCE for the allowed values: 802.11ah S1G widths 1, 2, 4, 8, 16 MHz. The default is an
    /// ASSUMPTION (1 MHz, the narrowest and longest range choice).
    pub bw_mhz: u8,
    /// S1G MCS 0..=10 (see `s1g`). ASSUMPTION: MCS0 default (the most robust).
    pub mcs: u8,
    /// Largest PHY payload handed to the radio (bytes after the headers in `overhead`).
    /// ASSUMPTION 1472: a 1500 B IP MTU minus IPv4 (20) and UDP (8). Real modules differ.
    pub mtu: usize,
    /// Per-packet bytes on top of the payload. ASSUMPTION 64 = 28 MAC header and FCS (the
    /// workshop airtime-model's `MAC_OVERHEAD_BYTES`; 802.11ah's short S1G header is
    /// smaller) + 8 LLC/SNAP + 20 IPv4 + 8 UDP, i.e. the packet goes through the Linux
    /// network stack of a HaLow netdev. A raw-frame driver would pay about 36 less.
    pub overhead: usize,
    /// ASSUMPTION: SIFS 160 us (sub-1 GHz S1G value as recalled from 802.11ah; not verified
    /// against the standard text, no datasheet cited).
    pub sifs_us: u64,
    /// ASSUMPTION: slot time 52 us (same caveat).
    pub slot_us: u64,
    /// ASSUMPTION: AIFSN 3 (EDCA best effort): AIFS = SIFS + 3 slots.
    pub aifsn: u64,
    /// ASSUMPTION: CWmin 15, CWmax 1023 (EDCA best effort defaults).
    pub cw_min: u64,
    pub cw_max: u64,
    /// Unicast with ACK and retransmission. false = broadcast/multicast: one attempt, no ACK.
    pub ack: bool,
    /// ASSUMPTION: 7 retransmissions (dot11ShortRetryLimit default 7), so up to 8 attempts.
    pub retries: u32,
    /// ASSUMPTION: ACK is a 14 byte control frame at MCS0 of the same bandwidth.
    pub ack_bytes: usize,
    /// ASSUMPTION: receive path latency (driver, stack, UDP) added to every delivered
    /// packet, plus uniform jitter 0..=`jitter_us`.
    pub proc_latency_us: u64,
    pub jitter_us: u64,
}

impl Default for HalowCfg {
    fn default() -> Self {
        HalowCfg {
            bw_mhz: 1,
            mcs: 0,
            mtu: 1472,
            overhead: 64,
            sifs_us: 160,
            slot_us: 52,
            aifsn: 3,
            cw_min: 15,
            cw_max: 1023,
            ack: true,
            retries: 7,
            ack_bytes: 14,
            proc_latency_us: 1500,
            jitter_us: 1000,
        }
    }
}

/// SX1276 LoRa modem settings (SOURCE: Semtech SX1276 datasheet, "Time on Air").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LoraCfg {
    pub sf: u8,
    pub bw_hz: u32,
    /// 1..=4 meaning 4/5..4/8.
    pub cr: u8,
    pub preamble_symbols: u32,
    pub crc: bool,
    pub explicit_header: bool,
}

impl Default for LoraCfg {
    fn default() -> Self {
        // The repo's fastest lora-2015 profile family (sf7, 500 kHz) is `sf7-500`.
        LoraCfg {
            sf: 7,
            bw_hz: 500_000,
            cr: 1,
            preamble_symbols: 8,
            crc: true,
            explicit_header: true,
        }
    }
}

impl LoraCfg {
    pub fn airtime_us(&self, payload: usize) -> u64 {
        let sf = i64::from(self.sf);
        let tsym = f64::from(1u32 << self.sf) / f64::from(self.bw_hz);
        let de = i64::from(tsym > 0.016);
        let ih = i64::from(!self.explicit_header);
        let crc = i64::from(self.crc);
        let num = 8 * payload as i64 - 4 * sf + 28 + 16 * crc - 20 * ih;
        let den = 4 * (sf - 2 * de);
        let blocks = if num <= 0 { 0 } else { (num + den - 1) / den };
        let n_payload = 8 + blocks * (i64::from(self.cr) + 4);
        ((f64::from(self.preamble_symbols) + 4.25 + n_payload as f64) * tsym * 1e6).round() as u64
    }
}

/// SX1276 FSK packet mode as the repo configures it (SOURCE: vendor/sx1276 `FskConfig`,
/// frame = preamble + 2 sync + 1 length + payload + 2 CRC at the FSK bit rate).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FskCfg {
    pub bitrate_bps: u32,
    pub preamble_bytes: u16,
}

impl Default for FskCfg {
    fn default() -> Self {
        FskCfg {
            bitrate_bps: 300_000,
            preamble_bytes: 6,
        }
    }
}

impl FskCfg {
    pub fn airtime_us(&self, payload: usize) -> u64 {
        (u64::from(self.preamble_bytes) + 2 + 1 + payload as u64 + 2) * 8_000_000
            / u64::from(self.bitrate_bps)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum PhyKind {
    Fsk(FskCfg),
    Lora(LoraCfg),
    Halow(HalowCfg),
}

/// Per-run emulation settings shared by every kind.
#[derive(Clone, Debug)]
pub struct EmuCfg {
    pub kind: PhyKind,
    pub loss: LossModel,
    pub seed: u64,
    /// Cap on wire frames per packet (HaLow only). 0 = as many as fit.
    pub max_frames: usize,
    /// Use the mean backoff (CW/2 slots) and no jitter instead of random draws: a
    /// deterministic airtime for tables.
    pub expected: bool,
    /// Latency added to FSK/LoRa deliveries. SOURCE: none; 0 by default (the airtime is the
    /// latency, as in the repo's own loopback path).
    pub fixed_latency_us: u64,
}

impl EmuCfg {
    pub fn new(kind: PhyKind) -> Self {
        EmuCfg {
            kind,
            loss: LossModel::None,
            seed: 1,
            max_frames: 0,
            expected: false,
            fixed_latency_us: 0,
        }
    }

    /// Is the configuration usable (MCS defined at this bandwidth, MTU holds a wire frame)?
    pub fn validate(&self) -> Result<(), String> {
        match &self.kind {
            PhyKind::Halow(h) => {
                if s1g::n_dbps(h.bw_mhz, h.mcs).is_none() {
                    return Err(format!(
                        "MCS{} is not defined at {} MHz (1 stream)",
                        h.mcs, h.bw_mhz
                    ));
                }
                if h.mtu < MAX_FRAME + pack::RECORD_OVERHEAD {
                    return Err(
                        "halow mtu must be at least 256 (one 255 B wire frame plus its length byte)"
                            .into(),
                    );
                }
            }
            PhyKind::Fsk(f) => {
                if f.bitrate_bps == 0 {
                    return Err("fsk bit rate must be above 0".into());
                }
            }
            PhyKind::Lora(l) => {
                if !(6..=12).contains(&l.sf) || !(1..=4).contains(&l.cr) || l.bw_hz == 0 {
                    return Err("lora: sf 6..=12, cr 1..=4, bw > 0".into());
                }
            }
        }
        Ok(())
    }

    /// Short name for logs, such as `halow-emu 1 MHz MCS0 ack`.
    pub fn name(&self) -> String {
        match &self.kind {
            PhyKind::Fsk(f) => format!("fsk-emu {} bit/s", f.bitrate_bps),
            PhyKind::Lora(l) => {
                format!("lora-emu SF{} BW{}k CR4/{}", l.sf, l.bw_hz / 1000, l.cr + 4)
            }
            PhyKind::Halow(h) => format!(
                "halow-emu {} MHz MCS{} {}",
                h.bw_mhz,
                h.mcs,
                if h.ack { "ack" } else { "bcast" }
            ),
        }
    }

    /// Largest PHY payload in bytes.
    pub fn mtu(&self) -> usize {
        match &self.kind {
            PhyKind::Halow(h) => h.mtu,
            _ => MAX_FRAME,
        }
    }

    /// Most wire frames one packet may hold (1 = no packing).
    pub fn max_frames_per_packet(&self) -> usize {
        match &self.kind {
            PhyKind::Halow(h) => {
                let fit = h.mtu / (MAX_FRAME + pack::RECORD_OVERHEAD);
                if self.max_frames == 0 {
                    fit
                } else {
                    fit.min(self.max_frames)
                }
                .max(1)
            }
            _ => 1,
        }
    }

    /// Channel time of one clean attempt carrying `payload_len` bytes, no backoff.
    pub fn airtime_us(&self, payload_len: usize) -> u64 {
        match &self.kind {
            PhyKind::Fsk(f) => f.airtime_us(payload_len),
            PhyKind::Lora(l) => l.airtime_us(payload_len),
            PhyKind::Halow(h) => data_us(h, payload_len),
        }
    }
}

/// Airtime of the ACK control frame.
fn ack_us(h: &HalowCfg) -> u64 {
    s1g::ppdu_us(h.bw_mhz, 0, h.ack_bytes).unwrap_or(0)
}

/// Time of the data PPDU for `payload_len` payload bytes, headers included.
fn data_us(h: &HalowCfg, payload_len: usize) -> u64 {
    s1g::ppdu_us(h.bw_mhz, h.mcs, payload_len + h.overhead).unwrap_or(u64::MAX / 4)
}

/// One direction of a modeled channel: seeded, stateful (Gilbert-Elliott state, RNG), virtual time.
pub struct Channel {
    pub cfg: EmuCfg,
    rng: Rng,
    loss: LossState,
}

impl Channel {
    pub fn new(cfg: EmuCfg) -> Self {
        let rng = Rng::new(cfg.seed);
        let loss = LossState::new(cfg.loss);
        Channel { cfg, rng, loss }
    }

    /// Put one packet of `n` payload bytes on the modeled air.
    pub fn transmit(&mut self, n: usize) -> Outcome {
        let expected = self.cfg.expected;
        match self.cfg.kind.clone() {
            PhyKind::Fsk(_) | PhyKind::Lora(_) => {
                let air = self.cfg.airtime_us(n);
                let lost = self.loss.lost(&mut self.rng);
                Outcome {
                    busy_us: air,
                    airtime_us: air,
                    delivered: !lost,
                    deliver_after_us: air + self.cfg.fixed_latency_us,
                    attempts: 1,
                }
            }
            PhyKind::Halow(h) => {
                let data = data_us(&h, n);
                let ack = ack_us(&h);
                let aifs = h.sifs_us + h.aifsn * h.slot_us;
                let max_attempts = if h.ack { h.retries + 1 } else { 1 };
                let mut cw = h.cw_min;
                let mut t = 0u64;
                let mut first_rx: Option<u64> = None;
                let mut attempts = 0;
                for _ in 0..max_attempts {
                    attempts += 1;
                    let slots = if expected { cw / 2 } else { self.rng.upto(cw) };
                    t += aifs + slots * h.slot_us + data;
                    let data_lost = self.loss.lost(&mut self.rng);
                    if !data_lost && first_rx.is_none() {
                        first_rx = Some(t);
                    }
                    if !h.ack {
                        break;
                    }
                    if !data_lost {
                        t += h.sifs_us + ack;
                        if !self.loss.lost(&mut self.rng) {
                            break; // ACK received
                        }
                    } else {
                        t += h.sifs_us + ack + h.slot_us; // ACK timeout
                    }
                    // a missing ACK means the sender retries (receiver drops the duplicate)
                    cw = (cw * 2 + 1).min(h.cw_max);
                }
                let jitter = if expected || h.jitter_us == 0 {
                    0
                } else {
                    self.rng.upto(h.jitter_us)
                };
                Outcome {
                    busy_us: t,
                    airtime_us: data * u64::from(attempts),
                    delivered: first_rx.is_some(),
                    deliver_after_us: first_rx.map_or(t, |x| x + h.proc_latency_us + jitter),
                    attempts,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsk_matches_the_senders_formula() {
        // moq-subghz-send: (preamble + 2 + 1 + 255 + 2) * 8e6 / bitrate for a 255 B frame
        assert_eq!(
            FskCfg::default().airtime_us(255),
            (6 + 2 + 1 + 255 + 2) * 8_000_000 / 300_000
        );
    }

    #[test]
    fn lora_matches_semtech_reference() {
        // SX1276 datasheet formula, 125 kHz CR4/5, 8 symbols preamble, explicit header, CRC:
        // SF7 10 B = 41.216 ms, SF12 10 B = 991.232 ms, SF7 51 B = 102.656 ms (the same
        // reference values as the workshop's airtime-model tests).
        let l = |sf| LoraCfg {
            sf,
            bw_hz: 125_000,
            ..LoraCfg::default()
        };
        assert_eq!(l(7).airtime_us(10), 41_216);
        assert_eq!(l(12).airtime_us(10), 991_232);
        assert_eq!(l(7).airtime_us(51), 102_656);
    }

    fn expected_halow() -> Channel {
        let mut cfg = EmuCfg::new(PhyKind::Halow(HalowCfg::default()));
        cfg.expected = true;
        Channel::new(cfg)
    }

    #[test]
    fn halow_clean_attempt_adds_up() {
        let o = expected_halow().transmit(1000);
        let h = HalowCfg::default();
        let data = s1g::ppdu_us(1, 0, 1064).unwrap();
        let ack = s1g::ppdu_us(1, 0, 14).unwrap();
        assert_eq!(o.busy_us, 160 + 3 * 52 + 7 * 52 + data + h.sifs_us + ack);
        assert_eq!(o.airtime_us, data);
        assert!(o.delivered && o.attempts == 1);
    }

    #[test]
    fn retries_hide_random_loss_but_cost_airtime() {
        let mut cfg = EmuCfg::new(PhyKind::Halow(HalowCfg::default()));
        cfg.loss = LossModel::Iid(0.2);
        let mut p = Channel::new(cfg.clone());
        let n = 20_000;
        let (mut lost, mut att) = (0, 0);
        for _ in 0..n {
            let o = p.transmit(500);
            lost += !o.delivered as u32;
            att += o.attempts;
        }
        // residual loss after 8 attempts at 20 % is 0.2^8 = 2.6e-6
        assert!(lost <= 2, "{lost}");
        let mean = att as f64 / n as f64;
        assert!(mean > 1.3 && mean < 1.6, "{mean}"); // about 1 / (1 - 0.2 * ...) with ACK loss
                                                     // broadcast: no retry, loss passes through
        let mut b = cfg;
        if let PhyKind::Halow(h) = &mut b.kind {
            h.ack = false;
        }
        let mut p = Channel::new(b);
        let lost = (0..n).filter(|_| !p.transmit(500).delivered).count() as f64 / n as f64;
        assert!((lost - 0.2).abs() < 0.02, "{lost}");
    }

    #[test]
    fn invalid_settings_are_refused() {
        let h = HalowCfg {
            bw_mhz: 2,
            mcs: 9,
            ..HalowCfg::default()
        };
        assert!(EmuCfg::new(PhyKind::Halow(h)).validate().is_err());
        let h = HalowCfg {
            mtu: 255,
            ..HalowCfg::default()
        };
        assert!(EmuCfg::new(PhyKind::Halow(h)).validate().is_err());
        let l = LoraCfg {
            sf: 5,
            ..LoraCfg::default()
        };
        assert!(EmuCfg::new(PhyKind::Lora(l)).validate().is_err());
        assert!(EmuCfg::new(PhyKind::Halow(HalowCfg::default()))
            .validate()
            .is_ok());
    }

    #[test]
    fn frames_per_packet_follows_mtu() {
        let c = EmuCfg::new(PhyKind::Halow(HalowCfg::default()));
        assert_eq!(c.max_frames_per_packet(), 5);
        let mut c = c;
        c.max_frames = 1;
        assert_eq!(c.max_frames_per_packet(), 1);
        assert_eq!(
            EmuCfg::new(PhyKind::Fsk(FskCfg::default())).max_frames_per_packet(),
            1
        );
    }

    #[test]
    fn same_seed_same_outcomes() {
        let mut c = EmuCfg::new(PhyKind::Halow(HalowCfg::default()));
        c.loss = LossModel::Iid(0.3);
        let run = |c: &EmuCfg| {
            let mut p = Channel::new(c.clone());
            (0..50).map(|_| p.transmit(700)).collect::<Vec<_>>()
        };
        assert_eq!(run(&c), run(&c));
        let mut d = c.clone();
        d.seed = 2;
        assert_ne!(run(&c), run(&d));
    }
}
