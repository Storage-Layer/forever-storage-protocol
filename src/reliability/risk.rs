//! Phase 2 Risk Estimation Engine (`RiskEstimate`).
//!
//! Computes deterministic, observable-only estimates of object recoverability loss
//! over a future lookahead window without accessing hidden simulator ground truth.

use crate::config::SECONDS_PER_EPOCH;
use crate::reliability::survival::analytical_correlated_survival_probability;

/// Explicit calibration status of the uncertainty/confidence reported by `RiskEstimate`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConfidenceCalibrationStatus {
    /// Hazards are provided from analytical/configured model parameters; `confidence` represents
    /// the interval tightness $1.0 - (P_{\max} - P_{\min}) \in [0, 1]$ across unobserved node state uncertainty.
    AnalyticalUnderConfiguredHazards { interval_width: f64 },
    /// Hazard rates are calibrated online from `observed_events >= 10` historical failures over `observation_node_epochs`.
    EmpiricalPoissonCalibrated {
        observed_events: usize,
        observation_node_epochs: f64,
    },
    /// Fewer than 10 historical failures have been observed; statistical calibration is explicitly marked
    /// unavailable, and callers should rely on `uncertainty_interval.1` (upper risk bound).
    UncalibratedInsufficientHistory { observed_events: usize },
}

/// Output of the Phase 2 risk estimator for a stored object.
#[derive(Debug, Clone, PartialEq)]
pub struct RiskEstimate {
    /// Point estimate of losing object recoverability ($< k$ surviving shards) within the evaluation window.
    pub loss_probability: f64,
    /// Conservative `(lower_bound, upper_bound)` on `loss_probability` accounting for state-observability
    /// and hazard estimation uncertainty.
    pub uncertainty_interval: (f64, f64),
    /// Statistically defined confidence/tightness in $[0.0, 1.0]$ (or `0.0` when `UncalibratedInsufficientHistory`).
    pub confidence: f64,
    /// Explicit calibration status preventing heuristic scores from being mistaken for calibrated probabilities.
    pub calibration_status: ConfidenceCalibrationStatus,
    /// Expected seconds required to repair all currently needed shards given queue backlog and bandwidth.
    pub expected_repair_duration_secs: f64,
    /// Normalized failure-domain concentration exposure score in $[0.0, 1.0]$.
    pub domain_concentration_score: f64,
    /// Expected number of currently intact durable shards $\hat{u}_{\text{eff}} \in [0, n]$.
    pub effective_durable_shards_expected: f64,
}

/// Observable-only inputs for evaluating the risk of a single stored object.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservableObjectState {
    /// Total coded shards $n$.
    pub total_shards: usize,
    /// Minimum distinct shards required for recovery $k$.
    pub recovery_threshold: usize,
    /// Number of shards currently hosted on `Healthy` or `RecoveryObserved` nodes.
    pub healthy_online_shards: usize,
    /// Posterior permanent failure probabilities $\hat{p}_{\text{perm}} \in [0, 1]$ for shards on `SuspectedUnavailable` nodes.
    pub suspected_shards_posterior_perm: Vec<f64>,
    /// Posterior permanent failure probabilities $\hat{p}_{\text{perm}} \in [0, 1]$ for shards on `ConfirmedUnavailableByPolicy` nodes.
    pub confirmed_unavailable_shards_posterior_perm: Vec<f64>,
    /// Number of shards with an active in-flight repair.
    pub in_flight_repairs: usize,
    /// Maximum believed-durable shards concentrated in a single failure domain.
    pub max_domain_shard_concentration: usize,
    /// Total failure domains in the cluster.
    pub num_failure_domains: usize,
    /// Whether a correlated domain outage/failure signal is currently active.
    pub correlated_domain_alert: bool,
    /// Total repair bytes currently queued ahead of this object.
    pub queued_repair_bytes: u64,
    /// Bytes required to repair a single shard of this object (local LRC or global RS).
    pub per_shard_repair_bytes: u64,
    /// Effective repair bandwidth in bytes/sec available to this object.
    pub effective_bandwidth_bps: u64,
    /// Base coordination latency per repair batch in seconds.
    pub base_latency_secs: f64,
}

/// Configuration for the risk estimation engine.
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(non_snake_case)]
pub struct RiskEstimatorConfig {
    /// Additional lookahead window in seconds beyond `expected_repair_duration_secs`.
    pub lookahead_window_secs: f64,
    /// Assumed independent per-node permanent failure probability per epoch.
    pub assumed_node_perm_Rate_per_epoch: f64,
    /// Assumed correlated per-domain permanent failure probability per epoch.
    pub assumed_domain_perm_rate_per_epoch: f64,
    /// Optional count of historically observed permanent failures (for empirical calibration).
    pub historical_observed_failures: Option<usize>,
    /// Total node-epochs of historical observation when `historical_observed_failures` is used.
    pub historical_node_epochs: f64,
}

impl Default for RiskEstimatorConfig {
    fn default() -> Self {
        Self {
            lookahead_window_secs: 3_600.0,
            assumed_node_perm_Rate_per_epoch: 0.015,
            assumed_domain_perm_rate_per_epoch: 0.002,
            historical_observed_failures: None,
            historical_node_epochs: 0.0,
        }
    }
}

/// Deterministically estimates object recoverability risk over `expected_repair_duration_secs + lookahead_window_secs`.
pub fn estimate_object_risk(
    state: &ObservableObjectState,
    cfg: &RiskEstimatorConfig,
) -> RiskEstimate {
    let n = state.total_shards;
    let k = state.recovery_threshold;

    // 1. Compute expected durable shards, conservative lower bound u_low, and optimistic upper bound u_high.
    let suspected_expected_surv: f64 = state
        .suspected_shards_posterior_perm
        .iter()
        .map(|&p| 1.0 - p.clamp(0.0, 1.0))
        .sum();
    let confirmed_expected_surv: f64 = state
        .confirmed_unavailable_shards_posterior_perm
        .iter()
        .map(|&p| 1.0 - p.clamp(0.0, 1.0))
        .sum();

    let expected_durable = ((state.healthy_online_shards as f64)
        + suspected_expected_surv
        + confirmed_expected_surv
        + (state.in_flight_repairs as f64))
        .clamp(0.0, n as f64);

    // Conservative bound u_low: only healthy online + suspected shards with posterior < 0.5 + in_flight
    let low_suspected_intact = state
        .suspected_shards_posterior_perm
        .iter()
        .filter(|&&p| p < 0.5)
        .count();
    let u_low =
        (state.healthy_online_shards + low_suspected_intact + state.in_flight_repairs).min(n);

    // Optimistic bound u_high: all healthy + all suspected + confirmed with posterior < 0.90 + in_flight
    let high_confirmed_intact = state
        .confirmed_unavailable_shards_posterior_perm
        .iter()
        .filter(|&&p| p < 0.90)
        .count();
    let u_high = (state.healthy_online_shards
        + state.suspected_shards_posterior_perm.len()
        + high_confirmed_intact
        + state.in_flight_repairs)
        .min(n);

    // 2. Estimate repair completion duration for needed shards.
    let shards_needing_repair = n.saturating_sub(u_low);
    let bps = state.effective_bandwidth_bps.max(1) as f64;
    let queue_wait_secs = (state.queued_repair_bytes as f64) / bps;
    let transfer_secs = if shards_needing_repair == 0 {
        0.0
    } else {
        (shards_needing_repair as f64)
            * (state.base_latency_secs + (state.per_shard_repair_bytes as f64) / bps)
    };
    let expected_repair_duration_secs = queue_wait_secs + transfer_secs;
    let total_eval_window_secs =
        (expected_repair_duration_secs + cfg.lookahead_window_secs).max(60.0);

    // 3. Determine hazard rate bounds and calibration status.
    let (p_node_low, p_node_mid, p_node_high, calibration_status) =
        match cfg.historical_observed_failures {
            None => {
                let p = cfg.assumed_node_perm_Rate_per_epoch.clamp(0.0, 0.999);
                (
                    p * 0.8,
                    p,
                    (p * 1.25).min(0.999),
                    ConfidenceCalibrationStatus::AnalyticalUnderConfiguredHazards {
                        interval_width: 0.0,
                    },
                )
            }
            Some(obs) if obs < 10 => {
                let p = cfg.assumed_node_perm_Rate_per_epoch.clamp(0.0, 0.999);
                (
                    p * 0.5,
                    p,
                    (p * 2.0).min(0.999),
                    ConfidenceCalibrationStatus::UncalibratedInsufficientHistory {
                        observed_events: obs,
                    },
                )
            }
            Some(obs) => {
                let epochs = cfg.historical_node_epochs.max(1.0);
                let hat_p = ((obs as f64) / epochs).clamp(1e-6, 0.999);
                let se = ((obs as f64).sqrt() / epochs).max(1e-6);
                let low = (hat_p - 1.96 * se).clamp(0.0, 0.999);
                let high = (hat_p + 1.96 * se).clamp(0.0, 0.999);
                (
                    low,
                    hat_p,
                    high,
                    ConfidenceCalibrationStatus::EmpiricalPoissonCalibrated {
                        observed_events: obs,
                        observation_node_epochs: epochs,
                    },
                )
            }
        };

    let dom_mult = if state.correlated_domain_alert {
        3.0
    } else {
        1.0
    };
    let p_dom_mid = (cfg.assumed_domain_perm_rate_per_epoch * dom_mult).clamp(0.0, 0.999);
    let p_dom_low = (p_dom_mid * 0.75).clamp(0.0, 0.999);
    let p_dom_high = (p_dom_mid * 1.5).clamp(0.0, 0.999);

    let max_per_dom = state.max_domain_shard_concentration.max(1);
    let num_doms = state.num_failure_domains.max(1);

    // 4. Compute lower, point, and upper loss probabilities over total_eval_window_secs.
    let surv_optimistic = analytical_correlated_survival_probability(
        u_high,
        k,
        num_doms,
        max_per_dom,
        p_node_low,
        p_dom_low,
        total_eval_window_secs,
    );
    let surv_conservative = analytical_correlated_survival_probability(
        u_low,
        k,
        num_doms,
        max_per_dom,
        p_node_high,
        p_dom_high,
        total_eval_window_secs,
    );

    let u_floor = (expected_durable.floor() as usize).min(n);
    let u_ceil = (expected_durable.ceil() as usize).min(n);
    let frac = (expected_durable - (u_floor as f64)).clamp(0.0, 1.0);
    let surv_floor = analytical_correlated_survival_probability(
        u_floor,
        k,
        num_doms,
        max_per_dom,
        p_node_mid,
        p_dom_mid,
        total_eval_window_secs,
    );
    let surv_ceil = analytical_correlated_survival_probability(
        u_ceil,
        k,
        num_doms,
        max_per_dom,
        p_node_mid,
        p_dom_mid,
        total_eval_window_secs,
    );
    let surv_mid = (1.0 - frac) * surv_floor + frac * surv_ceil;

    let loss_min = (1.0 - surv_optimistic).clamp(0.0, 1.0);
    let loss_max = (1.0 - surv_conservative).clamp(loss_min, 1.0);
    let loss_point = (1.0 - surv_mid).clamp(loss_min, loss_max);

    let interval_width = (loss_max - loss_min).clamp(0.0, 1.0);
    let final_calibration = match calibration_status {
        ConfidenceCalibrationStatus::AnalyticalUnderConfiguredHazards { .. } => {
            ConfidenceCalibrationStatus::AnalyticalUnderConfiguredHazards { interval_width }
        }
        other => other,
    };

    let confidence = match final_calibration {
        ConfidenceCalibrationStatus::UncalibratedInsufficientHistory { .. } => 0.0,
        _ => (1.0 - interval_width).clamp(0.0, 1.0),
    };

    let domain_concentration_score = if u_low < k {
        1.0
    } else {
        let margin = (u_low - k + 1).max(1) as f64;
        ((state.max_domain_shard_concentration as f64) / margin).clamp(0.0, 1.0)
    };

    let _ = SECONDS_PER_EPOCH;

    RiskEstimate {
        loss_probability: loss_point,
        uncertainty_interval: (loss_min, loss_max),
        confidence,
        calibration_status: final_calibration,
        expected_repair_duration_secs,
        domain_concentration_score,
        effective_durable_shards_expected: expected_durable,
    }
}
