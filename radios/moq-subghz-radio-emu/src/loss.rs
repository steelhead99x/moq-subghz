//! Seeded random numbers and the loss models. No external RNG crate.

/// splitmix64: tiny, fast, good enough for channel noise. Deterministic per seed.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform integer in 0..=hi.
    pub fn upto(&mut self, hi: u64) -> u64 {
        self.next_u64() % (hi + 1)
    }
}

/// Packet (attempt) loss model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LossModel {
    None,
    /// Independent loss with probability `p` per attempt.
    Iid(f64),
    /// Gilbert-Elliott two-state Markov channel, stepped once per attempt:
    /// Good -> Bad with `p_gb`, Bad -> Good with `p_bg`; loss probability `loss_good` /
    /// `loss_bad` in each state. Mean burst length is 1/p_bg attempts.
    Ge {
        p_gb: f64,
        p_bg: f64,
        loss_good: f64,
        loss_bad: f64,
    },
}

impl LossModel {
    /// Long-run mean loss probability (for reporting).
    pub fn mean_loss(&self) -> f64 {
        match *self {
            LossModel::None => 0.0,
            LossModel::Iid(p) => p,
            LossModel::Ge {
                p_gb,
                p_bg,
                loss_good,
                loss_bad,
            } => {
                let pi_bad = p_gb / (p_gb + p_bg);
                (1.0 - pi_bad) * loss_good + pi_bad * loss_bad
            }
        }
    }
    /// `p_gb:p_bg:loss_good:loss_bad`, or `iid:P`, or `none`.
    pub fn parse(s: &str) -> Result<Self, String> {
        if s == "none" {
            return Ok(LossModel::None);
        }
        if let Some(p) = s.strip_prefix("iid:") {
            let p: f64 = p.parse().map_err(|_| "iid:P needs a number")?;
            return if (0.0..=1.0).contains(&p) {
                Ok(LossModel::Iid(p))
            } else {
                Err("loss must be 0..=1".into())
            };
        }
        let v: Result<Vec<f64>, _> = s.split(':').map(str::parse::<f64>).collect();
        match v.map_err(|_| "loss spec: none | iid:P | p_gb:p_bg:loss_good:loss_bad")?[..] {
            [a, b, c, d] if [a, b, c, d].iter().all(|x| (0.0..=1.0).contains(x)) && a + b > 0.0 => {
                Ok(LossModel::Ge {
                    p_gb: a,
                    p_bg: b,
                    loss_good: c,
                    loss_bad: d,
                })
            }
            _ => Err("loss spec: none | iid:P | p_gb:p_bg:loss_good:loss_bad (all 0..=1)".into()),
        }
    }
}

/// Stateful draw from a [`LossModel`].
#[derive(Clone, Debug)]
pub struct LossState {
    pub model: LossModel,
    bad: bool,
}

impl LossState {
    pub fn new(model: LossModel) -> Self {
        LossState { model, bad: false }
    }
    /// True when this attempt is lost. Advances the channel state.
    pub fn lost(&mut self, rng: &mut Rng) -> bool {
        match self.model {
            LossModel::None => false,
            LossModel::Iid(p) => rng.unit() < p,
            LossModel::Ge {
                p_gb,
                p_bg,
                loss_good,
                loss_bad,
            } => {
                let flip = rng.unit();
                if self.bad {
                    if flip < p_bg {
                        self.bad = false;
                    }
                } else if flip < p_gb {
                    self.bad = true;
                }
                rng.unit() < if self.bad { loss_bad } else { loss_good }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iid_loss_rate_converges() {
        let mut r = Rng::new(1);
        let mut s = LossState::new(LossModel::Iid(0.05));
        let n = 200_000;
        let lost = (0..n).filter(|_| s.lost(&mut r)).count() as f64 / n as f64;
        assert!((lost - 0.05).abs() < 0.003, "{lost}");
    }

    #[test]
    fn ge_mean_and_burstiness() {
        let m = LossModel::Ge {
            p_gb: 0.01,
            p_bg: 0.1,
            loss_good: 0.0,
            loss_bad: 1.0,
        };
        let mut r = Rng::new(7);
        let mut s = LossState::new(m);
        let n = 400_000;
        let v: Vec<bool> = (0..n).map(|_| s.lost(&mut r)).collect();
        let mean = v.iter().filter(|x| **x).count() as f64 / n as f64;
        assert!(
            (mean - m.mean_loss()).abs() < 0.01,
            "{mean} vs {}",
            m.mean_loss()
        );
        // loss given previous loss is far above the mean: bursty
        let pairs = v.windows(2).filter(|w| w[0]).count() as f64;
        let both = v.windows(2).filter(|w| w[0] && w[1]).count() as f64;
        assert!(both / pairs > 0.8, "{}", both / pairs);
    }

    #[test]
    fn ge_mean_burst_length_is_one_over_p_bg() {
        // With loss_bad = 1 and loss_good = 0 a loss run is a visit to the bad state, whose
        // length is geometric with mean 1 / p_bg (here 10 attempts).
        let m = LossModel::Ge {
            p_gb: 0.02,
            p_bg: 0.1,
            loss_good: 0.0,
            loss_bad: 1.0,
        };
        let mut r = Rng::new(11);
        let mut s = LossState::new(m);
        let (mut runs, mut lost_total, mut cur) = (0u64, 0u64, 0u64);
        for _ in 0..1_000_000 {
            if s.lost(&mut r) {
                cur += 1;
                lost_total += 1;
            } else if cur > 0 {
                runs += 1;
                cur = 0;
            }
        }
        let mean_burst = lost_total as f64 / runs as f64;
        assert!((mean_burst - 10.0).abs() < 0.8, "{mean_burst}");
    }

    #[test]
    fn parse_specs() {
        assert_eq!(LossModel::parse("none"), Ok(LossModel::None));
        assert_eq!(LossModel::parse("iid:0.01"), Ok(LossModel::Iid(0.01)));
        assert!(LossModel::parse("0.01:0.1:0:1").is_ok());
        assert!(LossModel::parse("2").is_err());
        assert!(LossModel::parse("iid:2").is_err());
    }
}
