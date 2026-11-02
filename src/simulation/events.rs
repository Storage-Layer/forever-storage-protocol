//! Discrete-event types and deterministic event ordering.

use std::cmp::Ordering;

use crate::model::failure_domain::FailureDomainId;
use crate::model::node::NodeId;
use crate::model::shard::ShardId;

/// Categories of discrete events in the FSP simulation.
#[derive(Debug, Clone, PartialEq)]
pub enum EventKind {
    /// A storage node enters a temporary outage for `duration_secs` (transient churn).
    NodeOutage {
        /// Target storage node.
        node_id: NodeId,
        /// Duration of the temporary outage in seconds.
        duration_secs: f64,
    },
    /// A storage node recovers from a temporary outage back to `Online`.
    NodeRecovery {
        /// Target storage node.
        node_id: NodeId,
    },
    /// A storage node suffers permanent hardware/disk failure, destroying hosted shards.
    NodePermanentFailure {
        /// Target storage node.
        node_id: NodeId,
    },
    /// A specific logical shard replica suffers silent corruption or permanent deletion.
    ShardPermanentLoss {
        /// Target logical shard index.
        shard_id: ShardId,
    },
    /// Correlated temporary outage of an entire failure domain for `duration_secs`.
    DomainOutage {
        /// Target failure domain.
        domain_id: FailureDomainId,
        /// Outage duration in seconds.
        duration_secs: f64,
    },
    /// Correlated permanent disaster destroying all nodes and shards in a failure domain.
    DomainPermanentFailure {
        /// Target failure domain.
        domain_id: FailureDomainId,
    },
    /// Multi-node correlated burst failure affecting multiple distinct storage nodes simultaneously.
    BurstNodeFailure {
        /// Affected storage node IDs.
        node_ids: Vec<NodeId>,
        /// Whether the burst permanently destroys storage on those nodes (`true`) or causes a temporary outage (`false`).
        is_permanent: bool,
        /// Outage duration in seconds if `is_permanent == false`.
        outage_duration_secs: f64,
    },
    /// Completion of an in-flight shard repair transfer after bandwidth + latency delay.
    RepairComplete {
        /// Repaired logical shard index.
        shard_id: ShardId,
        /// Total network bytes read and written for this repair operation.
        bytes_transferred: u64,
    },
    /// Periodic controller check tick (evaluates strategy & records survival curve point).
    ControllerCheck,
}

/// A timestamped event in the discrete-event priority queue.
///
/// Ordered deterministically by `(time_secs, sequence)` so ties at identical timestamps
/// are broken strictly in FIFO insertion order.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledEvent {
    /// Simulation timestamp in seconds ($t \ge 0.0$).
    pub time_secs: f64,
    /// Monotonically increasing tie-breaker assigned on insertion.
    pub sequence: u64,
    /// Payload of the event.
    pub kind: EventKind,
}

impl Eq for ScheduledEvent {}

impl PartialOrd for ScheduledEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ScheduledEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap in Rust is a max-heap; reverse ordering to create a min-heap by (time_secs, sequence).
        other
            .time_secs
            .total_cmp(&self.time_secs)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}
