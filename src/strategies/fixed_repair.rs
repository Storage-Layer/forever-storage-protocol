//! Strategy B: Erasure-coded storage with a fixed repair threshold.

use crate::model::shard::ShardId;
use crate::strategies::{RepairDecision, RepairStrategy, StrategyContext};

/// Strategy B: Fixed-threshold repair for $(n, k)$ erasure-coded storage.
///
/// Triggers repair of missing shards whenever the count of durable (plus in-flight) distinct
/// shards drops at or below `repair_threshold`.
#[derive(Debug, Clone)]
pub struct FixedRepairStrategy {
    /// Durable shard count threshold $T_{\text{repair}}$ at or below which repair is triggered.
    pub repair_threshold: usize,
    /// Whether to also repair shards on nodes offline longer than the suspect timeout when
    /// currently available shards drop at or below `repair_threshold`.
    pub repair_suspected_offline: bool,
}

impl FixedRepairStrategy {
    /// Creates a fixed repair strategy that triggers whenever durable shards $\le$ `repair_threshold`.
    pub const fn new(repair_threshold: usize) -> Self {
        Self {
            repair_threshold,
            repair_suspected_offline: false,
        }
    }

    /// Creates a fixed repair strategy that only triggers on confirmed permanent shard loss
    /// when durable shards $\le$ `repair_threshold` (never on temporary outages).
    pub const fn only_permanent_below(repair_threshold: usize) -> Self {
        Self {
            repair_threshold,
            repair_suspected_offline: false,
        }
    }

    /// Enables or disables suspect-timeout repair for long-offline nodes.
    pub const fn with_repair_suspected_offline(mut self, enabled: bool) -> Self {
        self.repair_suspected_offline = enabled;
        self
    }
}

impl RepairStrategy for FixedRepairStrategy {
    fn name(&self) -> &'static str {
        "StrategyB_FixedThresholdRepair"
    }

    fn evaluate(&mut self, ctx: &StrategyContext<'_>) -> RepairDecision {
        if self.repair_threshold == 0 || !ctx.recoverability.is_currently_available() {
            return RepairDecision::NoOp;
        }

        let effective_durable =
            ctx.recoverability.durable_distinct_shards + ctx.cluster.in_flight_repair_count();

        let mut to_repair: Vec<ShardId> = Vec::new();

        if effective_durable <= self.repair_threshold {
            to_repair.extend(
                ctx.cluster
                    .missing_unscheduled_shards(ctx.erasure_code.total_shards),
            );
        }

        if self.repair_suspected_offline
            && ctx.recoverability.available_distinct_shards <= self.repair_threshold
        {
            for s in ctx
                .cluster
                .suspected_offline_shards(ctx.now_secs, ctx.unreachable_suspect_timeout_secs)
            {
                if !to_repair.contains(&s) {
                    to_repair.push(s);
                }
            }
        }

        if to_repair.is_empty() {
            RepairDecision::NoOp
        } else {
            RepairDecision::RepairShards(to_repair)
        }
    }
}
