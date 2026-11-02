//! Phase 2 FSP Adaptive Repair v2 (`FspAdaptiveV2Policy`).
//!
//! Operates under blind failure detection (or standard strategy context) without accessing
//! hidden simulator ground truth. Implements:
//! - Configurable shock protection modes (resolving the 19/20 single-shard deferral vulnerability)
//! - Bayesian posterior permanent-failure gating to suppress false positives under transient churn
//! - Domain-level correlated hazard escalation
//! - Proactive cancellation of redundant in-flight repairs when a suspected node recovers
//! - Bandwidth-budget-aware scheduling

use crate::model::shard::ShardId;
use crate::reliability::risk::{estimate_object_risk, ObservableObjectState, RiskEstimatorConfig};
use crate::strategies::{RepairDecision, RepairStrategy, StrategyContext};

/// Protection mechanism against unexpected multi-node shocks when redundancy is mildly degraded (e.g., 19/20 shards).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ShockProtectionMode {
    /// Phase 1 behavior (unbounded deferral when priority < trigger_threshold).
    UnboundedDeferral,
    /// Enforces a hard minimum durable safety margin $u_{\text{low}} \ge k + \text{min\_margin\_shards}$.
    MinimumSafetyMargin { min_margin_shards: usize },
    /// Repairs any confirmed permanent loss once silence age $\ge \text{max\_deferral\_secs}$ OR when
    /// surviving shards fall below $k + \text{worst\_case\_burst\_shards}$ and the repair queue is idle.
    BoundedDeferralAndBurstGuard {
        worst_case_burst_shards: usize,
        max_deferral_secs: f64,
    },
}

/// Configuration for `FspAdaptiveV2Policy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveV2Config {
    /// Shock protection mode (default: `BoundedDeferralAndBurstGuard { worst_case_burst_shards: 8, max_deferral_secs: 1800.0 }`).
    pub protection_mode: ShockProtectionMode,
    /// Upper-bound short-horizon loss risk threshold $\theta_{\text{risk}}$ triggering immediate repair.
    pub emergency_risk_threshold: f64,
    /// Base composite score threshold $\theta_{\text{trigger}}$ for confirmed unreachable shards.
    pub base_trigger_threshold: f64,
    /// Higher score threshold $\theta_{\text{suspect}}$ required to repair unconfirmed suspected shards.
    pub suspect_trigger_threshold: f64,
    /// Minimum Bayesian posterior $\hat{p}_{\text{perm}}$ required to treat a confirmed timeout as likely permanent.
    pub min_posterior_for_idle_repair: f64,
    /// Whether to cancel in-flight repairs if the original host node recovers before transfer completion.
    pub cancel_on_recovery: bool,
}

impl Default for AdaptiveV2Config {
    fn default() -> Self {
        Self {
            protection_mode: ShockProtectionMode::BoundedDeferralAndBurstGuard {
                worst_case_burst_shards: 7,
                max_deferral_secs: 1_800.0,
            },
            emergency_risk_threshold: 0.005,
            base_trigger_threshold: 0.18,
            suspect_trigger_threshold: 0.55,
            min_posterior_for_idle_repair: 0.50,
            cancel_on_recovery: true,
        }
    }
}

/// Decision emitted by `FspAdaptiveV2Policy::evaluate_blind()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlindRepairDecision {
    /// Shards to schedule for repair, ordered by priority descending.
    pub shards_to_repair: Vec<ShardId>,
    /// In-flight shard repairs to cancel immediately because their host node recovered.
    pub repairs_to_cancel: Vec<ShardId>,
}

/// Concrete Phase 2 FSP Adaptive Repair v2 policy.
#[derive(Debug, Clone)]
pub struct FspAdaptiveV2Policy {
    config: AdaptiveV2Config,
    risk_cfg: RiskEstimatorConfig,
}

impl Default for FspAdaptiveV2Policy {
    fn default() -> Self {
        Self::new(AdaptiveV2Config::default())
    }
}

impl FspAdaptiveV2Policy {
    /// Creates a new `FspAdaptiveV2Policy` with the given configuration.
    pub fn new(config: AdaptiveV2Config) -> Self {
        Self {
            config,
            risk_cfg: RiskEstimatorConfig::default(),
        }
    }

    /// Evaluates blind observable telemetry and returns both new repairs to schedule and
    /// redundant in-flight repairs to cancel.
    ///
    /// Arguments:
    /// - `confirmed_shards`: `(shard_id, posterior_perm_prob, silence_age_secs, is_confirmed_by_policy)`
    /// - `suspected_shards`: `(shard_id, posterior_perm_prob, silence_age_secs)`
    /// - `in_flight_shards`: `(shard_id, host_node_recovered)`
    #[allow(clippy::too_many_arguments)]
    pub fn evaluate_blind(
        &mut self,
        _now_secs: f64,
        total_shards: usize,
        recovery_threshold: usize,
        healthy_online_shards: usize,
        confirmed_shards: &[(ShardId, f64, f64, bool)],
        suspected_shards: &[(ShardId, f64, f64)],
        in_flight_shards: &[(ShardId, bool)],
        max_domain_concentration: usize,
        num_failure_domains: usize,
        correlated_domain_alert: bool,
        queued_repair_bytes: u64,
        per_shard_repair_bytes: u64,
        effective_bandwidth_bps: u64,
        check_interval_secs: f64,
    ) -> BlindRepairDecision {
        // 1. Identify any in-flight repairs whose original host node has recovered (`RecoveryObserved`).
        let mut repairs_to_cancel = Vec::new();
        let mut active_in_flight = 0usize;
        for &(shard_id, recovered) in in_flight_shards {
            if self.config.cancel_on_recovery && recovered {
                repairs_to_cancel.push(shard_id);
            } else {
                active_in_flight += 1;
            }
        }

        // Cannot reconstruct if fewer than k shards are healthy online.
        if healthy_online_shards < recovery_threshold {
            return BlindRepairDecision {
                shards_to_repair: vec![],
                repairs_to_cancel,
            };
        }

        if confirmed_shards.is_empty() && suspected_shards.is_empty() {
            return BlindRepairDecision {
                shards_to_repair: vec![],
                repairs_to_cancel,
            };
        }

        // 2. Invoke the Phase 2 Risk Estimation Engine on observable state.
        let obj_state = ObservableObjectState {
            total_shards,
            recovery_threshold,
            healthy_online_shards,
            suspected_shards_posterior_perm: suspected_shards.iter().map(|s| s.1).collect(),
            confirmed_unavailable_shards_posterior_perm: confirmed_shards
                .iter()
                .map(|s| s.1)
                .collect(),
            in_flight_repairs: active_in_flight,
            max_domain_shard_concentration: max_domain_concentration,
            num_failure_domains,
            correlated_domain_alert,
            queued_repair_bytes,
            per_shard_repair_bytes,
            effective_bandwidth_bps,
            base_latency_secs: 2.0,
        };
        let risk = estimate_object_risk(&obj_state, &self.risk_cfg);

        // Conservative durable shard count u_low:
        let u_low = (healthy_online_shards
            + suspected_shards.iter().filter(|s| s.1 < 0.5).count()
            + active_in_flight)
            .min(total_shards);

        let max_faults = total_shards.saturating_sub(recovery_threshold).max(1) as f64;
        let lost_faults = total_shards.saturating_sub(u_low) as f64;
        let u_margin = (lost_faults / max_faults).clamp(0.0, 1.0).powf(1.35);

        let budget_bytes = ((effective_bandwidth_bps as f64) * check_interval_secs)
            .round()
            .max(per_shard_repair_bytes as f64) as u64;
        let available_budget = budget_bytes.saturating_sub(queued_repair_bytes);
        let max_shards_by_budget =
            (available_budget / per_shard_repair_bytes.max(1)).max(1) as usize;
        let cost_norm = ((per_shard_repair_bytes + queued_repair_bytes) as f64
            / (budget_bytes.max(1) as f64))
            .clamp(0.0, 1.0);

        let dom_escalation = if correlated_domain_alert { 0.25 } else { 0.0 };

        let emergency_triggered = risk.uncertainty_interval.1
            >= self.config.emergency_risk_threshold
            || u_low <= recovery_threshold + max_domain_concentration.max(2);

        // Score and sort candidates
        let mut scored_candidates: Vec<(f64, ShardId)> = Vec::new();

        for &(shard_id, posterior_perm, silence_age_secs, _is_confirmed) in confirmed_shards {
            let p_perm = posterior_perm.clamp(0.0, 1.0);

            let shock_guard_triggers = match self.config.protection_mode {
                ShockProtectionMode::UnboundedDeferral => false,
                ShockProtectionMode::MinimumSafetyMargin { min_margin_shards } => {
                    u_low <= recovery_threshold + min_margin_shards
                }
                ShockProtectionMode::BoundedDeferralAndBurstGuard {
                    worst_case_burst_shards,
                    max_deferral_secs,
                } => {
                    let age_exceeded = silence_age_secs >= max_deferral_secs;
                    let burst_margin_breached = u_low
                        < recovery_threshold + worst_case_burst_shards
                        && queued_repair_bytes == 0;
                    p_perm >= self.config.min_posterior_for_idle_repair
                        && (age_exceeded || burst_margin_breached)
                }
            };

            let age_factor = match self.config.protection_mode {
                ShockProtectionMode::BoundedDeferralAndBurstGuard {
                    max_deferral_secs, ..
                } if max_deferral_secs > 0.0 => {
                    (silence_age_secs / max_deferral_secs).clamp(0.0, 1.0)
                }
                _ => 0.0,
            };

            let score = if emergency_triggered || shock_guard_triggers {
                1.0 + p_perm
            } else {
                p_perm
                    * (0.45 * u_margin
                        + 0.35 * risk.uncertainty_interval.1
                        + 0.20 * risk.domain_concentration_score
                        + 0.20 * age_factor
                        + dom_escalation)
                    - 0.12 * cost_norm
            };

            let effective_threshold =
                self.config.base_trigger_threshold * (1.0 - 0.35 * risk.domain_concentration_score);
            if score >= effective_threshold {
                scored_candidates.push((score, shard_id));
            }
        }

        // Suspected (unconfirmed) shards: only schedule if online availability is critically near k
        if healthy_online_shards <= recovery_threshold + 2 {
            for &(shard_id, posterior_perm, _age) in suspected_shards {
                let p_perm = posterior_perm.clamp(0.0, 1.0);
                let score = p_perm
                    * (0.50 * u_margin
                        + 0.35 * risk.uncertainty_interval.1
                        + 0.25 * risk.domain_concentration_score
                        + dom_escalation)
                    - 0.15 * cost_norm;
                if score >= self.config.suspect_trigger_threshold {
                    scored_candidates.push((score, shard_id));
                }
            }
        }

        // Sort deterministically by (score DESC, shard_id ASC)
        scored_candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        let mut shards_to_repair = Vec::new();
        for (_, shard_id) in scored_candidates {
            if shards_to_repair.len() >= max_shards_by_budget {
                break;
            }
            if !shards_to_repair.contains(&shard_id) {
                shards_to_repair.push(shard_id);
            }
        }

        BlindRepairDecision {
            shards_to_repair,
            repairs_to_cancel,
        }
    }
}

impl RepairStrategy for FspAdaptiveV2Policy {
    fn name(&self) -> &'static str {
        "StrategyC_FspAdaptiveV2"
    }

    fn evaluate(&mut self, ctx: &StrategyContext<'_>) -> RepairDecision {
        if !ctx.recoverability.is_currently_available() {
            return RepairDecision::NoOp;
        }

        let total_n = ctx.erasure_code.total_shards;
        let k = ctx.erasure_code.recovery_threshold;
        let single_shard_bytes = ((k as u64) + 1) * ctx.shard_size_bytes;

        // Build observable inputs from strategy context:
        // Shards missing for at least suspect_timeout or confirmed missing:
        let missing = ctx.cluster.missing_unscheduled_shards(total_n);
        let suspected = ctx
            .cluster
            .suspected_offline_shards(ctx.now_secs, ctx.unreachable_suspect_timeout_secs);

        let confirmed_tuples: Vec<(ShardId, f64, f64, bool)> = missing
            .into_iter()
            .map(|s| {
                let age = ctx.cluster.shard_missing_duration_secs(s, ctx.now_secs);
                // Under blind Bayesian progression, posterior increases with silence age:
                let posterior = 0.55 + 0.44 * (1.0 - (-age / 3600.0).exp());
                (s, posterior, age, true)
            })
            .collect();

        let suspected_tuples: Vec<(ShardId, f64, f64)> = suspected
            .into_iter()
            .map(|s| (s, 0.35, ctx.unreachable_suspect_timeout_secs))
            .collect();

        let bps = ((ctx.bandwidth_budget_bytes_per_interval as f64) / 3600.0)
            .round()
            .max(1.0) as u64;

        let decision = self.evaluate_blind(
            ctx.now_secs,
            total_n,
            k,
            ctx.recoverability.available_distinct_shards,
            &confirmed_tuples,
            &suspected_tuples,
            &[],
            ctx.recoverability.max_domain_shard_concentration,
            ctx.cluster.domains().len(),
            ctx.recoverability.failure_domain_exposure >= 0.5,
            ctx.queued_repair_bytes,
            single_shard_bytes,
            bps,
            3600.0,
        );

        if decision.shards_to_repair.is_empty() {
            RepairDecision::NoOp
        } else {
            RepairDecision::RepairShards(decision.shards_to_repair)
        }
    }
}
