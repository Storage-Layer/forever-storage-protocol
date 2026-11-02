//! Failure-domain-aware distributed shard placement planner (`src/coordinator/placement.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::model::node::NodeId;
use crate::node::identity::NodeFailureDomainTopology;

/// Placement policy governing how shards of an object may be distributed across failure domains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlacementPolicy {
    /// Local multi-daemon cluster mode: requires every shard on a distinct `NodeId` and at most
    /// `max_shards_per_rack` shards in any logical rack domain.
    SingleHostMultiDaemon { max_shards_per_rack: usize },
    /// Multi-host distinct-machine mode: enforces at most `max_shards_per_host` shards on any
    /// physical host (`host_domain`) and at most `max_shards_per_rack` in any rack (`rack_domain`).
    MultiHostDistinctMachines {
        max_shards_per_host: usize,
        max_shards_per_rack: usize,
    },
    /// Distinct physical disk mode: enforces at most `max_shards_per_disk` shards on any physical
    /// disk (`disk_domain`) and distinct `NodeId`s.
    DistinctPhysicalDisks { max_shards_per_disk: usize },
}

impl Default for PlacementPolicy {
    fn default() -> Self {
        Self::SingleHostMultiDaemon {
            max_shards_per_rack: 5,
        }
    }
}

/// Candidate storage node descriptor evaluated by the [`DistributedPlacementPlanner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateNodeInfo {
    pub node_id: NodeId,
    pub listen_addr: String,
    pub topology: NodeFailureDomainTopology,
    pub online: bool,
    pub available_bytes: u64,
}

/// Summary of how an object's shards are distributed across failure domains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementDistributionReport {
    pub total_shards_placed: usize,
    pub distinct_nodes: usize,
    pub distinct_hosts: usize,
    pub distinct_disks: usize,
    pub distinct_racks: usize,
    pub distinct_regions: usize,
    pub max_shards_on_any_host: usize,
    pub max_shards_on_any_disk: usize,
    pub max_shards_on_any_rack: usize,
}

/// Concrete shard-to-node assignment plan produced by [`DistributedPlacementPlanner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementPlan {
    pub policy: PlacementPolicy,
    pub shard_assignments: BTreeMap<usize, CandidateNodeInfo>,
    pub distribution_report: PlacementDistributionReport,
}

/// Errors returned when a placement request cannot satisfy capacity or failure-domain constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    /// Fewer online/capacity-eligible nodes exist than the required shard count `n`.
    InsufficientEligibleNodes {
        required_shards: usize,
        eligible_nodes: usize,
    },
    /// Failure-domain constraints (`Host`, `Disk`, or `Rack` caps) cannot be satisfied without
    /// silent degradation.
    UnsatisfiableDomainConstraint {
        policy: PlacementPolicy,
        placed_so_far: usize,
        required_shards: usize,
        reason: String,
    },
}

impl fmt::Display for PlacementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InsufficientEligibleNodes {
                required_shards,
                eligible_nodes,
            } => write!(
                f,
                "Insufficient eligible storage nodes: required {} shards, only {} online/capacity-eligible nodes available",
                required_shards, eligible_nodes
            ),
            Self::UnsatisfiableDomainConstraint {
                policy,
                placed_so_far,
                required_shards,
                reason,
            } => write!(
                f,
                "Unsatisfiable failure-domain constraint under {:?} (placed {}/{} shards): {}",
                policy, placed_so_far, required_shards, reason
            ),
        }
    }
}

impl std::error::Error for PlacementError {}

/// Failure-domain-aware distributed shard placement planner.
#[derive(Debug, Clone)]
pub struct DistributedPlacementPlanner {
    policy: PlacementPolicy,
}

impl DistributedPlacementPlanner {
    /// Creates a new [`DistributedPlacementPlanner`] with the given [`PlacementPolicy`].
    pub const fn new(policy: PlacementPolicy) -> Self {
        Self { policy }
    }

    /// Returns the configured [`PlacementPolicy`].
    pub const fn policy(&self) -> PlacementPolicy {
        self.policy
    }

    /// Computes a deterministic, failure-domain-balanced placement for `total_shards` shards,
    /// where each shard requires at least `required_shard_bytes` of free node capacity.
    ///
    /// Fails explicitly (never silently degrades constraints) if `total_shards` cannot be placed.
    pub fn plan_object_placement(
        &self,
        candidates: &[CandidateNodeInfo],
        total_shards: usize,
        required_shard_bytes: u64,
    ) -> Result<PlacementPlan, PlacementError> {
        let mut eligible: Vec<CandidateNodeInfo> = candidates
            .iter()
            .filter(|c| c.online && c.available_bytes >= required_shard_bytes)
            .cloned()
            .collect();

        // Deduplicate by NodeId and sort deterministically
        eligible.sort_by_key(|c| c.node_id);
        eligible.dedup_by_key(|c| c.node_id);

        if eligible.len() < total_shards {
            return Err(PlacementError::InsufficientEligibleNodes {
                required_shards: total_shards,
                eligible_nodes: eligible.len(),
            });
        }

        let mut used_nodes: BTreeSet<NodeId> = BTreeSet::new();
        let mut host_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut disk_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut rack_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut region_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut shard_assignments: BTreeMap<usize, CandidateNodeInfo> = BTreeMap::new();

        for shard_id in 0..total_shards {
            // Filter remaining candidates that do not violate hard policy caps, then pick the one
            // with minimal current rack/host/disk load to spread shards maximally across domains.
            let mut best_candidate: Option<CandidateNodeInfo> = None;
            let mut best_score: (usize, usize, usize, u64, NodeId) =
                (usize::MAX, usize::MAX, usize::MAX, 0, NodeId(usize::MAX));

            for cand in &eligible {
                if used_nodes.contains(&cand.node_id) {
                    continue;
                }
                let h_cnt = *host_counts.get(&cand.topology.physical_host).unwrap_or(&0);
                let d_cnt = *disk_counts.get(&cand.topology.disk_volume).unwrap_or(&0);
                let r_cnt = *rack_counts.get(&cand.topology.rack_group).unwrap_or(&0);

                let allowed = match self.policy {
                    PlacementPolicy::SingleHostMultiDaemon {
                        max_shards_per_rack,
                    } => r_cnt < max_shards_per_rack,
                    PlacementPolicy::MultiHostDistinctMachines {
                        max_shards_per_host,
                        max_shards_per_rack,
                    } => h_cnt < max_shards_per_host && r_cnt < max_shards_per_rack,
                    PlacementPolicy::DistinctPhysicalDisks {
                        max_shards_per_disk,
                    } => d_cnt < max_shards_per_disk,
                };
                if !allowed {
                    continue;
                }

                // Prefer least-loaded rack, then least-loaded host, then least-loaded disk, then highest available bytes, then lowest NodeId
                let score = (
                    r_cnt,
                    h_cnt,
                    d_cnt,
                    u64::MAX - cand.available_bytes,
                    cand.node_id,
                );
                if score < best_score {
                    best_score = score;
                    best_candidate = Some(cand.clone());
                }
            }

            let Some(chosen) = best_candidate else {
                return Err(PlacementError::UnsatisfiableDomainConstraint {
                    policy: self.policy,
                    placed_so_far: shard_id,
                    required_shards: total_shards,
                    reason: format!(
                        "no remaining candidate node satisfies failure-domain caps (host_counts={:?}, rack_counts={:?}, disk_counts={:?})",
                        host_counts, rack_counts, disk_counts
                    ),
                });
            };

            used_nodes.insert(chosen.node_id);
            *host_counts
                .entry(chosen.topology.physical_host.clone())
                .or_insert(0) += 1;
            *disk_counts
                .entry(chosen.topology.disk_volume.clone())
                .or_insert(0) += 1;
            *rack_counts
                .entry(chosen.topology.rack_group.clone())
                .or_insert(0) += 1;
            *region_counts
                .entry(chosen.topology.region.clone())
                .or_insert(0) += 1;
            shard_assignments.insert(shard_id, chosen);
        }

        let distribution_report = PlacementDistributionReport {
            total_shards_placed: shard_assignments.len(),
            distinct_nodes: used_nodes.len(),
            distinct_hosts: host_counts.len(),
            distinct_disks: disk_counts.len(),
            distinct_racks: rack_counts.len(),
            distinct_regions: region_counts.len(),
            max_shards_on_any_host: host_counts.values().copied().max().unwrap_or(0),
            max_shards_on_any_disk: disk_counts.values().copied().max().unwrap_or(0),
            max_shards_on_any_rack: rack_counts.values().copied().max().unwrap_or(0),
        };

        Ok(PlacementPlan {
            policy: self.policy,
            shard_assignments,
            distribution_report,
        })
    }

    /// Selects a single replacement node for repairing `shard_id`, avoiding all nodes in
    /// `already_holding_object` and respecting failure-domain caps and `required_shard_bytes`.
    pub fn select_repair_target_node(
        &self,
        candidates: &[CandidateNodeInfo],
        already_holding_object: &BTreeSet<NodeId>,
        required_shard_bytes: u64,
    ) -> Result<CandidateNodeInfo, PlacementError> {
        let mut host_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut disk_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut rack_counts: BTreeMap<String, usize> = BTreeMap::new();

        for c in candidates {
            if already_holding_object.contains(&c.node_id) {
                *host_counts
                    .entry(c.topology.physical_host.clone())
                    .or_insert(0) += 1;
                *disk_counts
                    .entry(c.topology.disk_volume.clone())
                    .or_insert(0) += 1;
                *rack_counts
                    .entry(c.topology.rack_group.clone())
                    .or_insert(0) += 1;
            }
        }

        let mut best: Option<CandidateNodeInfo> = None;
        let mut best_score = (
            usize::MAX,
            usize::MAX,
            usize::MAX,
            u64::MAX,
            NodeId(usize::MAX),
        );

        for cand in candidates {
            if !cand.online
                || cand.available_bytes < required_shard_bytes
                || already_holding_object.contains(&cand.node_id)
            {
                continue;
            }
            let h_cnt = *host_counts.get(&cand.topology.physical_host).unwrap_or(&0);
            let d_cnt = *disk_counts.get(&cand.topology.disk_volume).unwrap_or(&0);
            let r_cnt = *rack_counts.get(&cand.topology.rack_group).unwrap_or(&0);

            let allowed = match self.policy {
                PlacementPolicy::SingleHostMultiDaemon {
                    max_shards_per_rack,
                } => r_cnt < max_shards_per_rack,
                PlacementPolicy::MultiHostDistinctMachines {
                    max_shards_per_host,
                    max_shards_per_rack,
                } => h_cnt < max_shards_per_host && r_cnt < max_shards_per_rack,
                PlacementPolicy::DistinctPhysicalDisks {
                    max_shards_per_disk,
                } => d_cnt < max_shards_per_disk,
            };
            if !allowed {
                continue;
            }

            let score = (
                r_cnt,
                h_cnt,
                d_cnt,
                u64::MAX - cand.available_bytes,
                cand.node_id,
            );
            if score < best_score {
                best_score = score;
                best = Some(cand.clone());
            }
        }

        best.ok_or(PlacementError::InsufficientEligibleNodes {
            required_shards: 1,
            eligible_nodes: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_placement_planner_enforces_domain_caps_and_rejects_unsatisfiable() {
        let mut candidates = Vec::new();
        for i in 0..20 {
            candidates.push(CandidateNodeInfo {
                node_id: NodeId(i),
                listen_addr: format!("127.0.0.1:{}", 19000 + i),
                topology: NodeFailureDomainTopology::single_host_local_daemon(NodeId(i)),
                online: true,
                available_bytes: 100 * 1024 * 1024,
            });
        }

        // Default SingleHostMultiDaemon(max_shards_per_rack = 5) across 4 racks (5 nodes per rack) succeeds
        let planner = DistributedPlacementPlanner::new(PlacementPolicy::default());
        let plan = planner
            .plan_object_placement(&candidates, 20, 1024)
            .unwrap();
        assert_eq!(plan.distribution_report.total_shards_placed, 20);
        assert_eq!(plan.distribution_report.distinct_nodes, 20);
        assert_eq!(plan.distribution_report.distinct_racks, 4);
        assert_eq!(plan.distribution_report.max_shards_on_any_rack, 5);

        // MultiHostDistinctMachines(max_shards_per_host = 1) MUST fail on single-host candidates (all host-local-01)
        let strict_multi_host =
            DistributedPlacementPlanner::new(PlacementPolicy::MultiHostDistinctMachines {
                max_shards_per_host: 1,
                max_shards_per_rack: 5,
            });
        let err = strict_multi_host
            .plan_object_placement(&candidates, 20, 1024)
            .unwrap_err();
        assert!(matches!(
            err,
            PlacementError::UnsatisfiableDomainConstraint {
                placed_so_far: 1,
                required_shards: 20,
                ..
            }
        ));
    }
}
