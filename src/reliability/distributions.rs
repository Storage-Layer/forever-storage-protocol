//! Non-stationary hardware failure distributions (Exponential, Weibull, Piecewise-Constant,
//! and Correlated Domain Shock).
//!
//! Supports deterministic seeded sampling and exact instantaneous hazard $h(t)$ evaluation:
//! - Weibull $\beta < 1$: decreasing hazard (infant mortality)
//! - Weibull $\beta = 1$: constant hazard (exponential)
//! - Weibull $\beta > 1$: increasing hazard (wear-out aging)

use crate::simulation::engine::DeterministicRng;

/// Segment of a piecewise-constant hazard function on $[t_{\text{prev}}, \text{until\_secs})$.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PiecewiseHazardSegment {
    /// Upper time boundary of this segment in seconds.
    pub until_secs: f64,
    /// Constant hazard rate $\lambda$ (per second) active during this segment.
    pub hazard_per_sec: f64,
}

/// Configurable lifetime / time-to-failure distribution for storage nodes or domains.
#[derive(Debug, Clone, PartialEq)]
pub enum FailureDistribution {
    /// Memoryless exponential distribution with constant hazard rate $\lambda = 1 / \text{mean\_secs}$.
    Exponential { mean_secs: f64 },
    /// Weibull distribution with shape $\beta > 0$ and scale $\eta > 0$ (in seconds).
    /// Survival function: $S(t) = \exp(-(t / \eta)^\beta)$, hazard: $h(t) = (\beta / \eta) (t / \eta)^{\beta - 1}$.
    Weibull {
        shape_beta: f64,
        scale_eta_secs: f64,
    },
    /// Piecewise-constant hazard schedule across ordered time intervals plus a tail hazard rate.
    PiecewiseConstant {
        segments: Vec<PiecewiseHazardSegment>,
        tail_hazard_per_sec: f64,
    },
}

impl FailureDistribution {
    /// Evaluates the instantaneous hazard rate $h(t)$ (in $\text{s}^{-1}$) at age $t > 0$.
    pub fn instantaneous_hazard(&self, t_secs: f64) -> f64 {
        let t = t_secs.max(1e-9);
        match self {
            Self::Exponential { mean_secs } => {
                if *mean_secs <= 0.0 {
                    0.0
                } else {
                    1.0 / mean_secs
                }
            }
            Self::Weibull {
                shape_beta,
                scale_eta_secs,
            } => {
                let beta = shape_beta.max(1e-9);
                let eta = scale_eta_secs.max(1e-9);
                (beta / eta) * (t / eta).powf(beta - 1.0)
            }
            Self::PiecewiseConstant {
                segments,
                tail_hazard_per_sec,
            } => {
                for seg in segments {
                    if t <= seg.until_secs {
                        return seg.hazard_per_sec.max(0.0);
                    }
                }
                tail_hazard_per_sec.max(0.0)
            }
        }
    }

    /// Evaluates the cumulative hazard $H(t) = \int_0^t h(u)\,du$ at age $t \ge 0$.
    pub fn cumulative_hazard(&self, t_secs: f64) -> f64 {
        let t = t_secs.max(0.0);
        if t == 0.0 {
            return 0.0;
        }
        match self {
            Self::Exponential { mean_secs } => {
                if *mean_secs <= 0.0 {
                    0.0
                } else {
                    t / mean_secs
                }
            }
            Self::Weibull {
                shape_beta,
                scale_eta_secs,
            } => {
                let beta = shape_beta.max(1e-9);
                let eta = scale_eta_secs.max(1e-9);
                (t / eta).powf(beta)
            }
            Self::PiecewiseConstant {
                segments,
                tail_hazard_per_sec,
            } => {
                let mut h_cum = 0.0;
                let mut prev_t = 0.0;
                for seg in segments {
                    if t <= seg.until_secs {
                        h_cum += (t - prev_t).max(0.0) * seg.hazard_per_sec.max(0.0);
                        return h_cum;
                    }
                    if seg.until_secs > prev_t {
                        h_cum += (seg.until_secs - prev_t) * seg.hazard_per_sec.max(0.0);
                        prev_t = seg.until_secs;
                    }
                }
                h_cum + (t - prev_t).max(0.0) * tail_hazard_per_sec.max(0.0)
            }
        }
    }

    /// Conditional probability that a component of current age `age_secs` fails during `[age_secs, age_secs + dt_secs]`.
    pub fn conditional_failure_probability(&self, age_secs: f64, dt_secs: f64) -> f64 {
        let h0 = self.cumulative_hazard(age_secs);
        let h1 = self.cumulative_hazard(age_secs + dt_secs.max(0.0));
        let delta_h = (h1 - h0).max(0.0);
        (1.0 - (-delta_h).exp()).clamp(0.0, 1.0)
    }

    /// Deterministically samples a time-to-failure $T > 0$ (in seconds) using inverse-transform sampling $H^{-1}(-\ln(1 - U))$.
    pub fn sample_lifetime_secs(&self, rng: &mut DeterministicRng) -> f64 {
        let u = rng.next_f64().clamp(1e-15, 1.0 - 1e-15);
        let target_cum_hazard = -(1.0 - u).ln();

        match self {
            Self::Exponential { mean_secs } => (mean_secs.max(1.0)) * target_cum_hazard,
            Self::Weibull {
                shape_beta,
                scale_eta_secs,
            } => {
                let beta = shape_beta.max(1e-9);
                let eta = scale_eta_secs.max(1.0);
                eta * target_cum_hazard.powf(1.0 / beta)
            }
            Self::PiecewiseConstant {
                segments,
                tail_hazard_per_sec,
            } => {
                let mut remaining_h = target_cum_hazard;
                let mut prev_t = 0.0;
                for seg in segments {
                    if seg.until_secs <= prev_t {
                        continue;
                    }
                    let dt = seg.until_secs - prev_t;
                    let rate = seg.hazard_per_sec.max(0.0);
                    let seg_h = dt * rate;
                    if remaining_h <= seg_h && rate > 0.0 {
                        return prev_t + remaining_h / rate;
                    }
                    remaining_h -= seg_h;
                    prev_t = seg.until_secs;
                }
                let tail = tail_hazard_per_sec.max(1e-12);
                prev_t + remaining_h / tail
            }
        }
    }
}
