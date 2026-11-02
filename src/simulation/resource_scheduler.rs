//! Per-node ingress/egress NIC bandwidth and cluster repair queue scheduler.
//!
//! Enforces both:
//! 1. Per-node NIC transfer rate limits (`per_node_bandwidth_bps`) across all source nodes
//!    reading shards and the target node writing the repaired shard.
//! 2. Cluster-wide aggregate repair bandwidth limit (`cluster_bandwidth_bps`).

use crate::model::node::NodeId;
use crate::model::shard::ShardId;

/// Descriptor for a single shard repair network transfer across shared storage nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferRequest {
    /// Object identifier in a multi-object workload.
    pub object_id: usize,
    /// Repaired logical shard index.
    pub shard_id: ShardId,
    /// Storage nodes from which surviving shards are read ($k$ nodes for RS, $l$ nodes for LRC local).
    pub source_nodes: Vec<NodeId>,
    /// Storage node to which the reconstructed shard is written.
    pub target_node: NodeId,
    /// Size of a single shard in bytes.
    pub per_shard_bytes: u64,
    /// Current simulation timestamp $t$ in seconds.
    pub now_secs: f64,
}

/// Tracks earliest-free timestamps for each storage node's NIC and the aggregate cluster repair channel.
#[derive(Debug, Clone)]
pub struct PerNodeBandwidthScheduler {
    node_free_at_secs: Vec<f64>,
    cluster_free_at_secs: f64,
    per_node_bandwidth_bps: u64,
    cluster_bandwidth_bps: u64,
    base_latency_secs: f64,
}

impl PerNodeBandwidthScheduler {
    /// Creates a new per-node and cluster-wide repair bandwidth scheduler.
    pub fn new(
        num_nodes: usize,
        per_node_bandwidth_bps: u64,
        cluster_bandwidth_bps: u64,
        base_latency_secs: f64,
    ) -> Self {
        Self {
            node_free_at_secs: vec![0.0; num_nodes],
            cluster_free_at_secs: 0.0,
            per_node_bandwidth_bps: per_node_bandwidth_bps.max(1),
            cluster_bandwidth_bps: cluster_bandwidth_bps.max(1),
            base_latency_secs: base_latency_secs.max(0.0),
        }
    }

    /// Schedules `req` on the involved source nodes, target node, and cluster channel,
    /// returning the exact completion timestamp `finish_secs`.
    pub fn schedule_transfer(&mut self, req: &TransferRequest) -> f64 {
        let mut earliest_start = req.now_secs.max(self.cluster_free_at_secs);
        for &src in &req.source_nodes {
            if let Some(&t_free) = self.node_free_at_secs.get(src.0) {
                earliest_start = earliest_start.max(t_free);
            }
        }
        if let Some(&t_free) = self.node_free_at_secs.get(req.target_node.0) {
            earliest_start = earliest_start.max(t_free);
        }

        let total_shards_transferred = (req.source_nodes.len() as u64) + 1;
        let total_bytes = total_shards_transferred * req.per_shard_bytes;

        let cluster_duration = (total_bytes as f64) / (self.cluster_bandwidth_bps as f64);
        let per_node_duration = (req.per_shard_bytes as f64) / (self.per_node_bandwidth_bps as f64);
        let transfer_duration = self.base_latency_secs + cluster_duration.max(per_node_duration);

        let finish_secs = earliest_start + transfer_duration;

        self.cluster_free_at_secs = earliest_start + cluster_duration;
        for &src in &req.source_nodes {
            if let Some(slot) = self.node_free_at_secs.get_mut(src.0) {
                *slot = earliest_start + per_node_duration;
            }
        }
        if let Some(slot) = self.node_free_at_secs.get_mut(req.target_node.0) {
            *slot = earliest_start + per_node_duration;
        }

        finish_secs
    }

    /// Estimates the current cluster queue wait time at `now_secs`.
    pub fn current_queue_wait_secs(&self, now_secs: f64) -> f64 {
        (self.cluster_free_at_secs - now_secs).max(0.0)
    }
}
