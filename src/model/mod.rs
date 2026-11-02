//! Core domain models for storage nodes, failure domains, shards, and cluster placement.

pub mod failure_domain;
pub mod node;
pub mod shard;

use std::collections::{BTreeMap, BTreeSet};

use crate::model::failure_domain::{FailureDomain, FailureDomainId};
use crate::model::node::{NodeId, NodeState, StorageNode};
use crate::model::shard::{ShardId, ShardReplica};

/// Represents the full cluster state of nodes, failure domains, and active shard placements.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterState {
    /// Failure domains indexed by ID.
    domains: Vec<FailureDomain>,
    /// Storage nodes indexed by ID (`0..num_nodes`).
    nodes: Vec<StorageNode>,
    /// Active shard placements mapped by logical `ShardId`.
    /// By keying on `ShardId`, duplicate logical shard identities are structurally impossible.
    shards: BTreeMap<ShardId, ShardReplica>,
    /// Timestamps when each missing shard index was lost (for repair latency accounting).
    shard_lost_at_secs: BTreeMap<ShardId, f64>,
    /// Shard indices that currently have an in-flight repair operation scheduled.
    in_flight_repairs: BTreeSet<ShardId>,
    /// Total count of permanent node replacement re-provisionings across the cluster.
    node_replacements_count: u64,
}

impl ClusterState {
    /// Constructs a new cluster and performs initial failure-domain-aware placement of $n$ shards.
    pub fn new_with_initial_placement(
        num_nodes: usize,
        num_domains: usize,
        max_shards_per_domain: usize,
        total_shards: usize,
        shard_size_bytes: u64,
    ) -> Self {
        let domains: Vec<FailureDomain> = (0..num_domains)
            .map(|d| FailureDomain::new(FailureDomainId(d), max_shards_per_domain))
            .collect();

        let nodes: Vec<StorageNode> = (0..num_nodes)
            .map(|n| StorageNode::new(NodeId(n), FailureDomainId(n % num_domains)))
            .collect();

        let mut cluster = Self {
            domains,
            nodes,
            shards: BTreeMap::new(),
            shard_lost_at_secs: BTreeMap::new(),
            in_flight_repairs: BTreeSet::new(),
            node_replacements_count: 0,
        };

        // Round-robin placement across failure domains respecting `max_shards_per_domain`
        // and ensuring at most 1 shard per node.
        let mut domain_cursor = 0usize;
        for s_idx in 0..total_shards {
            let shard_id = ShardId(s_idx);
            let mut placed = false;
            for offset in 0..num_domains {
                let d_id = FailureDomainId((domain_cursor + offset) % num_domains);
                if let Some(target_node) = cluster.find_candidate_node_in_domain(d_id) {
                    cluster.shards.insert(
                        shard_id,
                        ShardReplica::new(shard_id, target_node, shard_size_bytes),
                    );
                    domain_cursor = (d_id.0 + 1) % num_domains;
                    placed = true;
                    break;
                }
            }
            debug_assert!(
                placed,
                "Validated config guarantees initial placement succeeds"
            );
        }

        cluster
    }

    /// Returns a slice of all failure domains.
    pub fn domains(&self) -> &[FailureDomain] {
        &self.domains
    }

    /// Returns a slice of all storage nodes.
    pub fn nodes(&self) -> &[StorageNode] {
        &self.nodes
    }

    /// Returns the number of currently offline or permanently failed nodes at this instant.
    pub fn count_offline_nodes(&self) -> usize {
        self.nodes.iter().filter(|n| !n.is_online()).count()
    }

    /// Looks up a storage node by `NodeId`.
    pub fn node(&self, node_id: NodeId) -> Option<&StorageNode> {
        self.nodes.get(node_id.0)
    }

    /// Returns all currently durable and valid shard replicas (hosted on `Online` or `TemporarilyOffline` nodes).
    pub fn durable_shard_replicas(&self) -> Vec<ShardReplica> {
        self.shards
            .values()
            .filter(|r| {
                r.is_valid
                    && self
                        .node(r.node_id)
                        .map(StorageNode::is_storage_durable)
                        .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    /// Returns all currently online and valid shard replicas.
    pub fn available_shard_replicas(&self) -> Vec<ShardReplica> {
        self.shards
            .values()
            .filter(|r| {
                r.is_valid
                    && self
                        .node(r.node_id)
                        .map(StorageNode::is_online)
                        .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    /// Returns the `NodeId`s currently hosting durable shards, sorted by `ShardId`.
    pub fn hosted_shard_nodes(&self) -> Vec<NodeId> {
        self.durable_shard_replicas()
            .into_iter()
            .map(|r| r.node_id)
            .collect()
    }

    /// Returns a map from `FailureDomainId` to the count of durable valid shards currently in that domain.
    pub fn durable_shards_per_domain(&self) -> BTreeMap<FailureDomainId, usize> {
        let mut counts: BTreeMap<FailureDomainId, usize> =
            self.domains.iter().map(|d| (d.id, 0usize)).collect();
        for replica in self.durable_shard_replicas() {
            if let Some(node) = self.node(replica.node_id) {
                *counts.entry(node.domain_id).or_insert(0) += 1;
            }
        }
        counts
    }

    /// Returns the set of nodes currently hosting any durable shard.
    pub fn occupied_nodes(&self) -> BTreeSet<NodeId> {
        self.durable_shard_replicas()
            .into_iter()
            .map(|r| r.node_id)
            .collect()
    }

    /// Finds an `Online` node in `domain_id` that does not currently host a shard, if the domain
    /// is strictly below its `max_shards_allowed` concentration limit.
    pub fn find_candidate_node_in_domain(&self, domain_id: FailureDomainId) -> Option<NodeId> {
        let domain = self.domains.get(domain_id.0)?;
        let current_in_domain = self
            .durable_shards_per_domain()
            .get(&domain_id)
            .copied()
            .unwrap_or(0);
        if current_in_domain >= domain.max_shards_allowed {
            return None;
        }
        let occupied = self.occupied_nodes();
        self.nodes
            .iter()
            .find(|n| n.domain_id == domain_id && n.is_online() && !occupied.contains(&n.id))
            .map(|n| n.id)
    }

    /// Selects the best `Online` candidate node across all failure domains for repairing a shard,
    /// preferring the domain with the lowest current shard concentration to minimize domain exposure.
    pub fn select_repair_target_node(&self) -> Option<NodeId> {
        let domain_counts = self.durable_shards_per_domain();
        let occupied = self.occupied_nodes();

        // Sort domains by (current_count, domain_id) for deterministic least-loaded selection.
        let mut candidate_domains: Vec<(usize, FailureDomainId, usize)> = self
            .domains
            .iter()
            .map(|d| {
                let c = domain_counts.get(&d.id).copied().unwrap_or(0);
                (c, d.id, d.max_shards_allowed)
            })
            .filter(|&(c, _, max_allowed)| c < max_allowed)
            .collect();
        candidate_domains.sort_by_key(|&(c, id, _)| (c, id));

        for (_, dom_id, _) in candidate_domains {
            if let Some(node) = self
                .nodes
                .iter()
                .find(|n| n.domain_id == dom_id && n.is_online() && !occupied.contains(&n.id))
            {
                return Some(node.id);
            }
        }
        None
    }

    /// Transitions a node into `TemporarilyOffline` until `recover_at_secs`.
    /// Has no effect if the node is already `PermanentlyFailed`.
    pub fn set_node_temporarily_offline(
        &mut self,
        node_id: NodeId,
        now_secs: f64,
        recover_at_secs: f64,
    ) -> bool {
        if let Some(node) = self.nodes.get_mut(node_id.0) {
            match node.state {
                NodeState::Online => {
                    node.state = NodeState::TemporarilyOffline {
                        since_secs: now_secs,
                        recover_at_secs,
                    };
                    return true;
                }
                NodeState::TemporarilyOffline {
                    since_secs,
                    recover_at_secs: existing_recover,
                } => {
                    node.state = NodeState::TemporarilyOffline {
                        since_secs,
                        recover_at_secs: existing_recover.max(recover_at_secs),
                    };
                    return true;
                }
                NodeState::PermanentlyFailed { .. } => return false,
            }
        }
        false
    }

    /// Recovers a `TemporarilyOffline` node back to `Online` if `now_secs >= recover_at_secs`.
    /// Never recovers a `PermanentlyFailed` node.
    pub fn recover_node(&mut self, node_id: NodeId, now_secs: f64) -> bool {
        if let Some(node) = self.nodes.get_mut(node_id.0) {
            if let NodeState::TemporarilyOffline {
                recover_at_secs, ..
            } = node.state
            {
                if now_secs + 1e-9 >= recover_at_secs {
                    node.state = NodeState::Online;
                    return true;
                }
            }
        }
        false
    }

    /// Permanently fails a node, destroying any shard replica hosted on it.
    ///
    /// Phase 1 bias correction: When `provision_empty_replacement == true`, the physical slot
    /// `node_id` is re-provisioned in place as a fresh, empty `Online` replacement host in the
    /// same failure domain (after destroying its shard), so that any shard subsequently repaired
    /// onto this slot continues to experience future exogenous failures scheduled for `node_id`.
    pub fn fail_node_permanently(
        &mut self,
        node_id: NodeId,
        now_secs: f64,
        provision_empty_replacement: bool,
    ) -> Option<ShardId> {
        {
            let node = self.nodes.get_mut(node_id.0)?;
            if node.is_permanently_failed() {
                return None;
            }
            if provision_empty_replacement {
                // Re-provision the node slot as an empty online host so it remains subject
                // to future exogenous events targeting `node_id`.
                node.state = NodeState::Online;
                self.node_replacements_count += 1;
            } else {
                node.state = NodeState::PermanentlyFailed {
                    failed_at_secs: now_secs,
                };
            }
        }

        // Find and permanently remove any shard hosted on this failed node.
        let lost_shard = self
            .shards
            .iter()
            .find(|(_, r)| r.node_id == node_id)
            .map(|(&s_id, _)| s_id);

        if let Some(s_id) = lost_shard {
            self.shards.remove(&s_id);
            self.shard_lost_at_secs.entry(s_id).or_insert(now_secs);
        }

        lost_shard
    }

    /// Corrupts or permanently deletes a specific shard replica on disk without failing the whole node.
    pub fn fail_shard_permanently(&mut self, shard_id: ShardId, now_secs: f64) -> bool {
        if self.shards.remove(&shard_id).is_some() {
            self.shard_lost_at_secs.entry(shard_id).or_insert(now_secs);
            true
        } else {
            false
        }
    }

    /// Returns all logical shard indices $i \in \{0, \dots, n-1\}$ that are permanently missing
    /// (not in `shards`) and do not already have an in-flight repair.
    pub fn missing_unscheduled_shards(&self, total_shards: usize) -> Vec<ShardId> {
        (0..total_shards)
            .map(ShardId)
            .filter(|s_id| {
                !self.shards.contains_key(s_id) && !self.in_flight_repairs.contains(s_id)
            })
            .collect()
    }

    /// Returns logical shard indices hosted on nodes that have been `TemporarilyOffline` for at least
    /// `suspect_timeout_secs` at `now_secs` and do not have an in-flight repair.
    pub fn suspected_offline_shards(
        &self,
        now_secs: f64,
        suspect_timeout_secs: f64,
    ) -> Vec<ShardId> {
        self.shards
            .values()
            .filter_map(|r| {
                if self.in_flight_repairs.contains(&r.shard_id) {
                    return None;
                }
                let node = self.node(r.node_id)?;
                if let NodeState::TemporarilyOffline { since_secs, .. } = node.state {
                    if now_secs - since_secs >= suspect_timeout_secs {
                        return Some(r.shard_id);
                    }
                }
                None
            })
            .collect()
    }

    /// Marks a shard as having an in-flight repair operation.
    pub fn mark_repair_in_flight(&mut self, shard_id: ShardId, now_secs: f64) {
        self.in_flight_repairs.insert(shard_id);
        self.shard_lost_at_secs.entry(shard_id).or_insert(now_secs);
    }

    /// Clears the in-flight repair flag for a shard (on completion or abort).
    pub fn clear_repair_in_flight(&mut self, shard_id: ShardId) {
        self.in_flight_repairs.remove(&shard_id);
    }

    /// Returns whether a shard currently has an in-flight repair.
    pub fn is_repair_in_flight(&self, shard_id: ShardId) -> bool {
        self.in_flight_repairs.contains(&shard_id)
    }

    /// Returns the number of currently in-flight shard repairs.
    pub fn in_flight_repair_count(&self) -> usize {
        self.in_flight_repairs.len()
    }

    /// Returns how long `shard_id` has been missing (seconds) up to `now_secs`.
    pub fn shard_missing_duration_secs(&self, shard_id: ShardId, now_secs: f64) -> f64 {
        self.shard_lost_at_secs
            .get(&shard_id)
            .map(|&lost_at| (now_secs - lost_at).max(0.0))
            .unwrap_or(0.0)
    }

    /// Completes a repair of `shard_id`, installing a valid replica on a healthy target node
    /// that respects failure-domain concentration limits and deduplicates any stale copy.
    /// Returns the repair latency ($t_{\text{now}} - t_{\text{lost}}$) if successful.
    pub fn complete_shard_repair(
        &mut self,
        shard_id: ShardId,
        shard_size_bytes: u64,
        now_secs: f64,
    ) -> Option<f64> {
        self.in_flight_repairs.remove(&shard_id);

        // If the shard already has a valid online replica (e.g., an offline node recovered before
        // a suspect-triggered repair finished), do not create a duplicate replica.
        if let Some(existing) = self.shards.get(&shard_id) {
            if existing.is_valid
                && self
                    .node(existing.node_id)
                    .map(StorageNode::is_online)
                    .unwrap_or(false)
            {
                self.shard_lost_at_secs.remove(&shard_id);
                return None;
            }
        }

        let target_node = self.select_repair_target_node()?;
        self.shards.insert(
            shard_id,
            ShardReplica::new(shard_id, target_node, shard_size_bytes),
        );
        let lost_at = self
            .shard_lost_at_secs
            .remove(&shard_id)
            .unwrap_or(now_secs);
        Some((now_secs - lost_at).max(0.0))
    }
}
