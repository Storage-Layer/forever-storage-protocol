//! Strategy C: FSP Adaptive Repair interface and concrete heuristic priority policy.
//!
//! Explicitly labeled as a mathematically normalized multi-factor risk-cost heuristic
//! (`priority = f(loss_risk, repair_cost, repair_latency, domain_exposure)`).

use crate::model::shard::ShardId;
use crate::strategies::{RepairDecision, RepairStrategy, StrategyContext};

/// Telemetry input vector supplied to an FSP Adaptive Repair policy at decision time $t$.
#[derive(Debug, Clone, PartialEq)]
pub struct AdaptiveRiskTelemetry {
    /// Current simulation timestamp $t$ (seconds).
    pub now_secs: f64,
    /// Number of currently online distinct valid shards $|\mathcal{A}(t)|$.
    pub available_distinct_shards: usize,
    /// Number of durably intact distinct valid shards $|\mathcal{U}(t)|$.
    pub durable_distinct_shards: usize,
    /// Number of shards currently in flight in the repair queue.
    pub in_flight_repairs: usize,
    /// Erasure code recovery threshold $k$.
    pub recovery_threshold: usize,
    /// Erasure code total shards $n$.
    pub total_shards: usize,
    /// Maximum durable shard count concentrated in any single failure domain $c_{\max}^*(t)$.
    pub max_domain_concentration: usize,
    /// Normalized failure-domain exposure ratio $E_{\text{domain}}(t) \in [0.0, 1.0]$.
    pub failure_domain_exposure: f64,
    /// Analytical survival probability over the next evaluation window without repair.
    pub estimated_survival_probability: f64,
    /// Estimated probability of irrecoverable data loss during expected repair completion window $R_{\text{window}} \in [0.0, 1.0]$.
    pub window_loss_risk: f64,
    /// Estimated bandwidth cost in bytes to repair a single shard $((k + 1) \cdot S_{\text{shard}})$.
    pub single_shard_repair_cost_bytes: u64,
    /// Total repair bytes currently queued on the bandwidth channel.
    pub queued_repair_bytes: u64,
    /// Maximum repair bytes transferable per controller check interval ($B_{\max} \cdot \Delta t_{\text{check}}$).
    pub bandwidth_budget_bytes_per_interval: u64,
    /// Expected duration in seconds to complete repairing the current missing shards.
    pub expected_repair_duration_secs: f64,
    /// Confirmed permanently missing shard indices eligible for repair.
    pub missing_shards: Vec<ShardId>,
    /// Shard indices hosted on nodes offline longer than the suspect threshold.
    pub suspected_offline_shards: Vec<ShardId>,
}

/// Dimensionless weights for the heuristic priority function:
/// $$P = w_{\text{margin}} U_{\text{margin}} + w_{\text{risk}} R_{\text{window}} + w_{\text{dom}} E_{\text{dom}} - w_{\text{cost}} C_{\text{norm}}$$
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PriorityWeights {
    /// Weight on redundancy buffer depletion urgency $U_{\text{margin}} \in [0, 1]$.
    pub w_margin: f64,
    /// Weight on probability of loss during repair latency window $R_{\text{window}} \in [0, 1]$.
    pub w_risk: f64,
    /// Weight on normalized failure-domain concentration exposure $E_{\text{dom}} \in [0, 1]$.
    pub w_domain: f64,
    /// Penalty weight on normalized repair bandwidth cost & queue congestion $C_{\text{norm}} \in [0, 1]$.
    pub w_cost: f64,
    /// Nonlinearity exponent $\gamma \ge 1.0$ on redundancy margin depletion.
    pub margin_exponent: f64,
}

impl Default for PriorityWeights {
    fn default() -> Self {
        Self {
            w_margin: 0.45,
            w_risk: 0.35,
            w_domain: 0.20,
            w_cost: 0.15,
            margin_exponent: 1.5,
        }
    }
}

/// Computes the normalized heuristic repair priority score $P \in [-1.0, 1.0]$ from `telemetry` and `weights`.
///
/// Formally documented in `docs/PHASE1_MATHEMATICAL_MODEL.md`:
/// - $U_{\text{margin}} = ((n - u_{\text{eff}}) / \max(1, n - k))^{\gamma} \in [0, 1]$
/// - $R_{\text{window}} \in [0, 1]$ (probability of permanent loss before repair finishes)
/// - $E_{\text{dom}} \in [0, 1]$ (failure-domain concentration exposure)
/// - $C_{\text{norm}} = \min(1, (W_{\text{shard}} + Q_{\text{bytes}}) / B_{\text{interval}}) \in [0, 1]$
pub fn compute_heuristic_repair_priority(
    telemetry: &AdaptiveRiskTelemetry,
    weights: &PriorityWeights,
) -> f64 {
    let n = telemetry.total_shards;
    let k = telemetry.recovery_threshold;
    let u_eff = (telemetry.durable_distinct_shards + telemetry.in_flight_repairs).min(n);

    // Critical safety override: if effective durable shards are within one domain failure
    // (or within 2 shards) of the recovery threshold k, priority is maximal (1.0).
    let safety_floor = k + telemetry.max_domain_concentration.max(2);
    if u_eff <= safety_floor {
        return 1.0;
    }

    let max_faults = n.saturating_sub(k).max(1) as f64;
    let lost_faults = n.saturating_sub(u_eff) as f64;
    let u_margin = (lost_faults / max_faults)
        .clamp(0.0, 1.0)
        .powf(weights.margin_exponent);

    let r_window = telemetry.window_loss_risk.clamp(0.0, 1.0);
    let e_dom = telemetry.failure_domain_exposure.clamp(0.0, 1.0);

    let budget = telemetry.bandwidth_budget_bytes_per_interval.max(1) as f64;
    let cost_bytes =
        (telemetry.single_shard_repair_cost_bytes + telemetry.queued_repair_bytes) as f64;
    let c_norm = (cost_bytes / budget).clamp(0.0, 1.0);

    let raw_priority =
        weights.w_margin * u_margin + weights.w_risk * r_window + weights.w_domain * e_dom
            - weights.w_cost * c_norm;

    raw_priority.clamp(-1.0, 1.0)
}

/// Trait defining the pluggable policy interface for Strategy C (FSP Adaptive Repair).
pub trait AdaptiveRepairPolicy: Send {
    /// Policy identifier for experiment reporting.
    fn policy_name(&self) -> &'static str;

    /// Evaluates risk telemetry and returns which shards (if any) to schedule for repair.
    fn select_repairs(&mut self, telemetry: &AdaptiveRiskTelemetry) -> RepairDecision;
}

/// Concrete Phase 1 FSP Adaptive Repair Heuristic Policy.
///
/// Features:
/// 1. Suppresses false-positive repairs on short transient outages.
/// 2. Batches/defers non-critical single-shard permanent losses when redundancy buffer is high
///    and domain exposure is low ($P < \theta_{\text{trigger}}$), reducing peak bandwidth contention.
/// 3. Immediately triggers critical repairs when redundancy approaches $k + c_{\max}^*$ or when
///    correlated risk $R_{\text{window}}$ rises.
/// 4. Enforces per-interval bandwidth budget so repair bursts never cause unbounded queue bloat.
#[derive(Debug, Clone)]
pub struct FspAdaptivePolicy {
    /// Priority weights.
    pub weights: PriorityWeights,
    /// Priority threshold $\theta_{\text{trigger}}$ above which confirmed missing shards are repaired.
    pub trigger_threshold: f64,
    /// Higher priority threshold $\theta_{\text{suspect}}$ required to proactively replace long-offline
    /// suspected shards when online availability drops near $k$.
    pub suspect_trigger_threshold: f64,
}

impl Default for FspAdaptivePolicy {
    fn default() -> Self {
        Self {
            weights: PriorityWeights::default(),
            trigger_threshold: 0.22,
            suspect_trigger_threshold: 0.55,
        }
    }
}

impl FspAdaptivePolicy {
    /// Creates a new adaptive policy with custom thresholds.
    pub fn new(trigger_threshold: f64, suspect_trigger_threshold: f64) -> Self {
        Self {
            weights: PriorityWeights::default(),
            trigger_threshold,
            suspect_trigger_threshold,
        }
    }
}

impl AdaptiveRepairPolicy for FspAdaptivePolicy {
    fn policy_name(&self) -> &'static str {
        "StrategyC_FspAdaptiveRepair"
    }

    fn select_repairs(&mut self, telemetry: &AdaptiveRiskTelemetry) -> RepairDecision {
        if telemetry.available_distinct_shards < telemetry.recovery_threshold {
            return RepairDecision::NoOp;
        }
        if telemetry.missing_shards.is_empty() && telemetry.suspected_offline_shards.is_empty() {
            return RepairDecision::NoOp;
        }

        let priority = compute_heuristic_repair_priority(telemetry, &self.weights);
        let single_cost = telemetry.single_shard_repair_cost_bytes.max(1);
        let available_budget = telemetry
            .bandwidth_budget_bytes_per_interval
            .saturating_sub(telemetry.queued_repair_bytes);
        let max_shards_by_budget = (available_budget / single_cost).max(1) as usize;

        let mut selected: Vec<ShardId> = Vec::new();

        // 1. Confirmed permanently missing shards: schedule if priority >= trigger_threshold
        if priority >= self.trigger_threshold && !telemetry.missing_shards.is_empty() {
            for &shard_id in &telemetry.missing_shards {
                if selected.len() >= max_shards_by_budget {
                    break;
                }
                if !selected.contains(&shard_id) {
                    selected.push(shard_id);
                }
            }
        }

        // 2. Suspected offline shards: only repair if online availability is critically close to k
        // AND priority >= suspect_trigger_threshold.
        let critical_online_floor = telemetry.recovery_threshold + 2;
        if telemetry.available_distinct_shards <= critical_online_floor
            && priority >= self.suspect_trigger_threshold
        {
            for &shard_id in &telemetry.suspected_offline_shards {
                if selected.len() >= max_shards_by_budget {
                    break;
                }
                if !selected.contains(&shard_id) {
                    selected.push(shard_id);
                }
            }
        }

        if selected.is_empty() {
            RepairDecision::NoOp
        } else {
            RepairDecision::RepairShards(selected)
        }
    }
}

/// Strategy C adapter wrapping any `AdaptiveRepairPolicy` into a `RepairStrategy`.
pub struct FspAdaptiveStrategy<P: AdaptiveRepairPolicy> {
    policy: P,
}

impl<P: AdaptiveRepairPolicy> FspAdaptiveStrategy<P> {
    /// Wraps an adaptive policy implementation into a simulation `RepairStrategy`.
    pub const fn new(policy: P) -> Self {
        Self { policy }
    }
}

impl<P: AdaptiveRepairPolicy> RepairStrategy for FspAdaptiveStrategy<P> {
    fn name(&self) -> &'static str {
        self.policy.policy_name()
    }

    fn evaluate(&mut self, ctx: &StrategyContext<'_>) -> RepairDecision {
        if !ctx.recoverability.is_currently_available() {
            return RepairDecision::NoOp;
        }

        let single_shard_repair_cost_bytes =
            ((ctx.erasure_code.recovery_threshold as u64) + 1) * ctx.shard_size_bytes;

        let telemetry = AdaptiveRiskTelemetry {
            now_secs: ctx.now_secs,
            available_distinct_shards: ctx.recoverability.available_distinct_shards,
            durable_distinct_shards: ctx.recoverability.durable_distinct_shards,
            in_flight_repairs: ctx.cluster.in_flight_repair_count(),
            recovery_threshold: ctx.erasure_code.recovery_threshold,
            total_shards: ctx.erasure_code.total_shards,
            max_domain_concentration: ctx.recoverability.max_domain_shard_concentration,
            failure_domain_exposure: ctx.recoverability.failure_domain_exposure,
            estimated_survival_probability: ctx.analytical_step_survival_probability,
            window_loss_risk: ctx.window_loss_risk,
            single_shard_repair_cost_bytes,
            queued_repair_bytes: ctx.queued_repair_bytes,
            bandwidth_budget_bytes_per_interval: ctx.bandwidth_budget_bytes_per_interval,
            expected_repair_duration_secs: ctx.expected_repair_duration_secs,
            missing_shards: ctx
                .cluster
                .missing_unscheduled_shards(ctx.erasure_code.total_shards),
            suspected_offline_shards: ctx
                .cluster
                .suspected_offline_shards(ctx.now_secs, ctx.unreachable_suspect_timeout_secs),
        };

        self.policy.select_repairs(&telemetry)
    }
}
