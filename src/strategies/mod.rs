//! Storage preservation and repair scheduling strategy interfaces.

pub mod adaptive;
pub mod adaptive_v2;
pub mod fixed_repair;
pub mod replication;

use crate::config::ErasureCodeConfig;
use crate::model::shard::ShardId;
use crate::model::ClusterState;
use crate::reliability::recoverability::RecoverabilityStatus;

/// Decision returned by a repair strategy when evaluated by the simulation engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairDecision {
    /// Do not initiate any repair operations at this time.
    NoOp,
    /// Schedule repair for the specified logical shard indices.
    RepairShards(Vec<ShardId>),
}

/// Read-only context provided to a `RepairStrategy` at decision time $t$.
pub struct StrategyContext<'a> {
    /// Current simulation timestamp $t$ in seconds.
    pub now_secs: f64,
    /// Current cluster state (nodes, domains, placed shards).
    pub cluster: &'a ClusterState,
    /// Configured erasure-coding parameters $(n, k)$.
    pub erasure_code: ErasureCodeConfig,
    /// Pre-evaluated recoverability and domain-exposure status at time $t$.
    pub recoverability: &'a RecoverabilityStatus,
    /// Size of a single shard in bytes.
    pub shard_size_bytes: u64,
    /// Configured suspect timeout in seconds for temporarily offline nodes.
    pub unreachable_suspect_timeout_secs: f64,
    /// Analytical step survival probability over the next check interval without repair.
    pub analytical_step_survival_probability: f64,
    /// Analytical probability of irrecoverable data loss during the expected repair window + check interval.
    pub window_loss_risk: f64,
    /// Currently queued repair bytes waiting on the cluster bandwidth channel.
    pub queued_repair_bytes: u64,
    /// Maximum repair bytes transferable per controller check interval ($B_{\max} \cdot \Delta t_{\text{check}}$).
    pub bandwidth_budget_bytes_per_interval: u64,
    /// Expected seconds until a newly scheduled batch of missing shards completes repair.
    pub expected_repair_duration_secs: f64,
}

/// Common interface for all storage preservation strategies (Strategies A, B, and C).
pub trait RepairStrategy: Send {
    /// Human-readable strategy identifier.
    fn name(&self) -> &'static str;

    /// Evaluates the current cluster state and returns a repair scheduling decision.
    fn evaluate(&mut self, ctx: &StrategyContext<'_>) -> RepairDecision;
}
