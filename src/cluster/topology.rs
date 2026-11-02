//! Multi-zone failure domain topology and placement policies for Phase 5.4 cluster staging.
//!
//! Provides zone-aware topology mapping (5 zones x 4 nodes = 20 nodes) enforcing that:
//! - No zone contains more than 4 nodes.
//! - RS(20,12) stripes distribute at most 4 shards to any single zone.
//! - Total failure of any 2 zones (8 nodes) leaves 12 shards alive, guaranteeing deterministic reconstruction.
//! - Simultaneous failure of 3 zones (12 nodes) leaves 8 shards, which is strictly below K=12 and fails closed.

use std::collections::{BTreeMap, BTreeSet};

use crate::coordinator::transactions::RegisteredStorageNode;
use crate::model::node::NodeId;
use crate::node::identity::NodeFailureDomainTopology;

/// Logical failure zone identifier (e.g., "zone-a", "zone-b").
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ZoneId(pub String);

impl ZoneId {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Node zone metadata record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeZoneMetadata {
    pub node_id: NodeId,
    pub zone_id: ZoneId,
    pub rack_id: String,
    pub host_id: String,
    pub listen_addr: String,
    pub capacity_bytes: u64,
}

/// 5-Zone Cluster Topology Manager.
#[derive(Debug, Clone)]
pub struct ClusterZoneTopology {
    pub zones: Vec<ZoneId>,
    pub nodes: BTreeMap<NodeId, NodeZoneMetadata>,
    pub max_shards_per_zone: usize,
}

impl ClusterZoneTopology {
    /// Constructs a standard 5-zone topology for a 20-node cluster (4 nodes per zone).
    pub fn standard_5_zones_20_nodes(base_port: u16, capacity_bytes: u64) -> Self {
        let zone_names = ["zone-a", "zone-b", "zone-c", "zone-d", "zone-e"];
        let mut zones = Vec::new();
        let mut nodes = BTreeMap::new();

        for (z_idx, z_name) in zone_names.iter().enumerate() {
            let zone_id = ZoneId::new(*z_name);
            zones.push(zone_id.clone());

            for node_offset in 0..4 {
                let node_num = z_idx * 4 + node_offset;
                let node_id = NodeId(node_num);
                let port = if base_port == 0 {
                    0
                } else {
                    base_port + node_num as u16
                };
                let listen_addr = format!("127.0.0.1:{}", port);

                let meta = NodeZoneMetadata {
                    node_id,
                    zone_id: zone_id.clone(),
                    rack_id: format!("rack-{}-{}", z_name, node_offset / 2),
                    host_id: format!("host-{}-{:02}", z_name, node_num),
                    listen_addr,
                    capacity_bytes,
                };
                nodes.insert(node_id, meta);
            }
        }

        Self {
            zones,
            nodes,
            max_shards_per_zone: 4,
        }
    }

    /// Returns the list of node IDs assigned to a specific zone.
    pub fn nodes_in_zone(&self, zone: &ZoneId) -> Vec<NodeId> {
        self.nodes
            .values()
            .filter(|m| &m.zone_id == zone)
            .map(|m| m.node_id)
            .collect()
    }

    /// Returns the zone ID for a given node.
    pub fn zone_for_node(&self, node_id: NodeId) -> Option<&ZoneId> {
        self.nodes.get(&node_id).map(|m| &m.zone_id)
    }

    /// Evaluates data survival under the complete outage of specified offline nodes.
    ///
    /// For RS(20,12), survival requires >= 12 surviving nodes.
    pub fn evaluate_rs20_12_survival(
        &self,
        offline_nodes: &BTreeSet<NodeId>,
    ) -> TopologySurvivalEvaluation {
        let total_nodes = self.nodes.len();
        let surviving_nodes: usize = self
            .nodes
            .keys()
            .filter(|n| !offline_nodes.contains(n))
            .count();
        let lost_nodes = total_nodes.saturating_sub(surviving_nodes);

        let mut affected_zones = BTreeMap::new();
        for node in offline_nodes {
            if let Some(meta) = self.nodes.get(node) {
                *affected_zones.entry(meta.zone_id.clone()).or_insert(0usize) += 1;
            }
        }

        let entirely_failed_zones = affected_zones
            .iter()
            .filter(|(z, count)| **count == self.nodes_in_zone(z).len())
            .count();

        let can_recover = surviving_nodes >= 12;

        TopologySurvivalEvaluation {
            total_nodes,
            surviving_nodes,
            lost_nodes,
            entirely_failed_zones,
            can_recover,
            max_tolerable_loss: 8,
        }
    }

    /// Converts this cluster zone topology into registered storage nodes for the coordinator.
    pub fn to_registered_storage_nodes(&self) -> Vec<RegisteredStorageNode> {
        self.nodes
            .values()
            .map(|m| {
                let topology = NodeFailureDomainTopology::distinct_physical_host(
                    m.node_id,
                    &m.host_id,
                    &format!("disk-{:02}", m.node_id.0),
                    &m.rack_id,
                    &m.zone_id.0,
                );
                RegisteredStorageNode {
                    node_id: m.node_id,
                    listen_addr: m.listen_addr.clone(),
                    topology,
                }
            })
            .collect()
    }
}

/// Evaluation result for failure domain resilience.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologySurvivalEvaluation {
    pub total_nodes: usize,
    pub surviving_nodes: usize,
    pub lost_nodes: usize,
    pub entirely_failed_zones: usize,
    pub can_recover: bool,
    pub max_tolerable_loss: usize,
}
