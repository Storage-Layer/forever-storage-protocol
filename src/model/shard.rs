//! Logical erasure-coded shard identity and replica state.

use crate::model::node::NodeId;

/// Logical shard index $i \in \{0, 1, \dots, n - 1\}$ within an $(n, k)$ erasure-coded object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardId(pub usize);

/// A placed shard replica on a specific storage node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardReplica {
    /// Logical shard index $i \in \{0, \dots, n-1\}$.
    pub shard_id: ShardId,
    /// Storage node hosting this shard.
    pub node_id: NodeId,
    /// Size of this shard in bytes.
    pub size_bytes: u64,
    /// Whether the shard's cryptographic integrity check is valid (uncorrupted on disk).
    pub is_valid: bool,
}

impl ShardReplica {
    /// Creates a new valid shard replica placed on `node_id`.
    pub const fn new(shard_id: ShardId, node_id: NodeId, size_bytes: u64) -> Self {
        Self {
            shard_id,
            node_id,
            size_bytes,
            is_valid: true,
        }
    }
}
