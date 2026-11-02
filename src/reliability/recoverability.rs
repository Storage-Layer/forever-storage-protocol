//! Erasure-code recoverability and data availability evaluation.

use std::collections::BTreeSet;

use crate::config::ErasureCodeConfig;
use crate::model::shard::ShardId;
use crate::model::ClusterState;

/// Canonical four-state distinction between availability and permanent data loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemDataState {
    /// At least $k$ distinct valid shards are currently `Online` ($|\mathcal{A}(t)| \ge k$).
    /// The object is immediately readable and repairable.
    AvailableAndRecoverable,
    /// Fewer than $k$ distinct valid shards are currently `Online` ($|\mathcal{A}(t)| < k$),
    /// BUT at least $k$ distinct valid shards exist durably across `Online` and `TemporarilyOffline`
    /// nodes ($|\mathcal{U}(t)| \ge k$). No permanent data loss has occurred.
    TemporarilyUnavailableButDurable,
    /// Fewer than $k$ distinct valid shards exist durably across all non-permanently-failed nodes
    /// ($|\mathcal{U}(t)| < k$). This is an absorbing state representing irrecoverable data loss.
    IrrecoverablyLost,
}

/// Snapshot of erasure-code recoverability and failure-domain exposure at an instant $t$.
#[derive(Debug, Clone, PartialEq)]
pub struct RecoverabilityStatus {
    /// Count of distinct valid shards currently `Online` ($|\mathcal{A}(t)|$).
    pub available_distinct_shards: usize,
    /// Count of distinct valid shards durably intact on `Online` or `TemporarilyOffline` nodes ($|\mathcal{U}(t)|$).
    pub durable_distinct_shards: usize,
    /// Required recovery threshold $k$.
    pub recovery_threshold: usize,
    /// Total configured shards $n$.
    pub total_shards: usize,
    /// Canonical data state.
    pub state: SystemDataState,
    /// Maximum number of durable shards concentrated in any single failure domain ($c_{\max}^*(t)$).
    pub max_domain_shard_concentration: usize,
    /// Normalized failure-domain exposure metric $E_{\text{domain}}(t) \in [0.0, 1.0]$.
    pub failure_domain_exposure: f64,
}

impl RecoverabilityStatus {
    /// Returns `true` iff the data can be read and reconstructed right now ($|\mathcal{A}(t)| \ge k$).
    pub const fn is_currently_available(&self) -> bool {
        matches!(self.state, SystemDataState::AvailableAndRecoverable)
    }

    /// Returns `true` iff the data has NOT suffered permanent loss ($|\mathcal{U}(t)| \ge k$).
    pub const fn is_durably_recoverable(&self) -> bool {
        matches!(
            self.state,
            SystemDataState::AvailableAndRecoverable
                | SystemDataState::TemporarilyUnavailableButDurable
        )
    }
}

/// Evaluates the exact recoverability, availability, and failure-domain exposure of `cluster`.
pub fn evaluate_recoverability(
    cluster: &ClusterState,
    ec: ErasureCodeConfig,
) -> RecoverabilityStatus {
    let available_ids: BTreeSet<ShardId> = cluster
        .available_shard_replicas()
        .into_iter()
        .map(|r| r.shard_id)
        .collect();

    let durable_ids: BTreeSet<ShardId> = cluster
        .durable_shard_replicas()
        .into_iter()
        .map(|r| r.shard_id)
        .collect();

    let available_distinct_shards = available_ids.len();
    let durable_distinct_shards = durable_ids.len();
    let k = ec.recovery_threshold;

    let state = if available_distinct_shards >= k {
        SystemDataState::AvailableAndRecoverable
    } else if durable_distinct_shards >= k {
        SystemDataState::TemporarilyUnavailableButDurable
    } else {
        SystemDataState::IrrecoverablyLost
    };

    let domain_counts = cluster.durable_shards_per_domain();
    let max_domain_shard_concentration = domain_counts.values().copied().max().unwrap_or(0);

    // Normalized domain exposure E_domain(t):
    // Ratio of max shards in a single domain to the number of shard losses needed to cause
    // irrecoverable data loss: (durable - k + 1).
    let failure_domain_exposure = if durable_distinct_shards < k {
        1.0
    } else {
        let losses_to_break = (durable_distinct_shards - k + 1).max(1);
        (max_domain_shard_concentration as f64 / losses_to_break as f64).min(1.0)
    };

    RecoverabilityStatus {
        available_distinct_shards,
        durable_distinct_shards,
        recovery_threshold: k,
        total_shards: ec.total_shards,
        state,
        max_domain_shard_concentration,
        failure_domain_exposure,
    }
}
