//! Storage node state machine and metadata.

use crate::model::failure_domain::FailureDomainId;

/// Strongly-typed identifier for a storage node in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub usize);

/// Operational and durability state of a storage node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NodeState {
    /// Node is reachable, can serve reads, accept repairs, and verify shards.
    Online,
    /// Node is temporarily unreachable due to transient churn or domain outage.
    /// Shards stored on its disk are NOT deleted and become accessible again upon recovery.
    TemporarilyOffline {
        /// Simulation timestamp (seconds) when the node entered the offline state.
        since_secs: f64,
        /// Scheduled simulation timestamp (seconds) when the node will recover.
        recover_at_secs: f64,
    },
    /// Node has suffered permanent disk/hardware failure or permanent departure.
    /// All shards stored on this node are irreversibly lost.
    PermanentlyFailed {
        /// Simulation timestamp (seconds) when permanent failure occurred.
        failed_at_secs: f64,
    },
}

/// A storage node participating in the FSP cluster.
#[derive(Debug, Clone, PartialEq)]
pub struct StorageNode {
    /// Unique identifier for this node.
    pub id: NodeId,
    /// Failure domain to which this node belongs.
    pub domain_id: FailureDomainId,
    /// Current state of the node.
    pub state: NodeState,
}

impl StorageNode {
    /// Creates a new online storage node in the specified failure domain.
    pub const fn new(id: NodeId, domain_id: FailureDomainId) -> Self {
        Self {
            id,
            domain_id,
            state: NodeState::Online,
        }
    }

    /// Returns `true` iff the node is currently `Online` and able to serve shards.
    pub const fn is_online(&self) -> bool {
        matches!(self.state, NodeState::Online)
    }

    /// Returns `true` iff the node's persistent storage is still intact (`Online` or `TemporarilyOffline`).
    pub const fn is_storage_durable(&self) -> bool {
        matches!(
            self.state,
            NodeState::Online | NodeState::TemporarilyOffline { .. }
        )
    }

    /// Returns `true` iff the node has permanently failed.
    pub const fn is_permanently_failed(&self) -> bool {
        matches!(self.state, NodeState::PermanentlyFailed { .. })
    }
}
