//! Strategy A: Conventional $R$-way replication baseline.

use crate::model::shard::ShardId;
use crate::strategies::{RepairDecision, RepairStrategy, StrategyContext};

/// Strategy A: Conventional $R$-way replication baseline ($k = 1, n = R$).
///
/// Repairs missing replicas whenever the durable replica count falls below `target_replicas`,
/// and optionally replaces replicas hosted on nodes that have been unreachable longer than
/// the configured suspect timeout.
#[derive(Debug, Clone)]
pub struct ReplicationStrategy {
    target_replicas: usize,
    repair_suspected_offline: bool,
}

impl ReplicationStrategy {
    /// Creates a new $R$-way replication baseline strategy.
    pub const fn new(target_replicas: usize) -> Self {
        Self {
            target_replicas,
            repair_suspected_offline: false,
        }
    }

    /// Enables or disables proactive replacement of replicas on long-offline nodes.
    pub const fn with_repair_suspected_offline(mut self, enabled: bool) -> Self {
        self.repair_suspected_offline = enabled;
        self
    }
}

impl RepairStrategy for ReplicationStrategy {
    fn name(&self) -> &'static str {
        "StrategyA_ReplicationBaseline"
    }

    fn evaluate(&mut self, ctx: &StrategyContext<'_>) -> RepairDecision {
        // Reconstruction requires at least k (for replication, k = 1) currently online replicas.
        if !ctx.recoverability.is_currently_available() {
            return RepairDecision::NoOp;
        }

        let total_n = ctx.erasure_code.total_shards.min(self.target_replicas);
        let mut to_repair: Vec<ShardId> = ctx.cluster.missing_unscheduled_shards(total_n);

        if self.repair_suspected_offline {
            let suspected = ctx
                .cluster
                .suspected_offline_shards(ctx.now_secs, ctx.unreachable_suspect_timeout_secs);
            for s in suspected {
                if s.0 < total_n && !to_repair.contains(&s) {
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
