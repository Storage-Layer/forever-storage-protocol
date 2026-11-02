//! Multi-object simulation engine supporting heterogeneous object sizes, coding schemes
//! (Reed–Solomon, LRC, and Replication), shared storage nodes, and per-node + cluster
//! bandwidth contention.

use std::collections::{BTreeMap, BTreeSet};

use crate::coding::CodingScheme;
use crate::config::{ConfigError, SimulationConfig};
use crate::model::failure_domain::FailureDomainId;
use crate::model::node::{NodeId, NodeState, StorageNode};
use crate::model::shard::ShardId;
use crate::simulation::events::EventKind;
use crate::simulation::failure_generator::generate_exogenous_failure_trace;
use crate::simulation::resource_scheduler::{PerNodeBandwidthScheduler, TransferRequest};

/// Specification for a single stored object within a multi-object workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectWorkloadSpec {
    /// Unique identifier for the object (`0..num_objects`).
    pub object_id: usize,
    /// Logical object size in bytes.
    pub object_size_bytes: u64,
    /// Erasure code or LRC configuration for this object.
    pub coding: CodingScheme,
}

impl ObjectWorkloadSpec {
    /// Size of a single encoded shard of this object in bytes.
    pub fn shard_size_bytes(&self) -> u64 {
        let k = self.coding.data_shards().max(1) as u64;
        self.object_size_bytes.div_ceil(k)
    }
}

/// Configuration for a multi-object contention experiment.
#[derive(Debug, Clone, PartialEq)]
pub struct MultiObjectWorkloadConfig {
    /// Cluster topology and failure trace configuration.
    pub base_config: SimulationConfig,
    /// Per-node NIC bandwidth limit in bytes/sec.
    pub per_node_bandwidth_bps: u64,
    /// Stored objects sharing the cluster.
    pub objects: Vec<ObjectWorkloadSpec>,
}

/// Per-object durability and repair telemetry in a multi-object simulation.
#[derive(Debug, Clone, PartialEq)]
pub struct PerObjectMetrics {
    /// Object identifier.
    pub object_id: usize,
    /// Whether this object suffered irrecoverable data loss.
    pub irrecoverable_data_loss: bool,
    /// Timestamp when data loss occurred, if any.
    pub data_loss_time_secs: Option<f64>,
    /// Total repair bytes transferred for this object.
    pub repair_bandwidth_bytes: u64,
    /// Total completed repair operations for this object.
    pub repair_operations: u64,
    /// Number of local group repairs executed (for LRC objects).
    pub local_group_repairs: u64,
    /// Number of global reconstruction repairs executed.
    pub global_repairs: u64,
    /// Mean repair latency (from shard loss to repair completion) in seconds.
    pub mean_repair_latency_secs: f64,
}

/// Summary report of a multi-object workload run.
#[derive(Debug, Clone, PartialEq)]
pub struct MultiObjectSimulationReport {
    /// Per-object metrics.
    pub per_object_metrics: Vec<PerObjectMetrics>,
    /// Aggregate repair bytes across all objects.
    pub aggregate_repair_bandwidth_bytes: u64,
    /// Aggregate repair bandwidth in bytes/sec across the horizon.
    pub aggregate_repair_bandwidth_bps: f64,
    /// Fraction of objects that suffered irrecoverable data loss.
    pub object_loss_fraction: f64,
}

struct ObjectPlacementState {
    spec: ObjectWorkloadSpec,
    /// Map from `ShardId` to hosting `NodeId`.
    shard_to_node: BTreeMap<ShardId, NodeId>,
    /// Timestamp when each missing shard was lost.
    lost_at_secs: BTreeMap<ShardId, f64>,
    /// Scheduled repair completions: `(finish_secs, shard_id, bytes_transferred, is_local_repair)`.
    in_flight: Vec<(f64, ShardId, u64, bool)>,
    irrecoverable_loss: bool,
    loss_time_secs: Option<f64>,
    repair_bytes: u64,
    repair_ops: u64,
    local_repairs: u64,
    global_repairs: u64,
    total_latency_secs: f64,
}

/// Deterministic multi-object simulation engine with shared node placement and NIC contention.
pub struct MultiObjectSimulationEngine {
    cfg: MultiObjectWorkloadConfig,
    nodes: Vec<StorageNode>,
    objects: Vec<ObjectPlacementState>,
    scheduler: PerNodeBandwidthScheduler,
}

impl MultiObjectSimulationEngine {
    /// Initializes the multi-object cluster and places all objects across failure domains.
    pub fn new(cfg: MultiObjectWorkloadConfig) -> Result<Self, ConfigError> {
        cfg.base_config.validate()?;
        let num_nodes = cfg.base_config.num_nodes;
        let num_domains = cfg.base_config.num_failure_domains;

        let nodes: Vec<StorageNode> = (0..num_nodes)
            .map(|i| StorageNode::new(NodeId(i), FailureDomainId(i % num_domains)))
            .collect();

        let mut objects = Vec::with_capacity(cfg.objects.len());
        for (obj_idx, spec) in cfg.objects.iter().cloned().enumerate() {
            let n = spec.coding.total_shards();
            if n > num_nodes {
                return Err(ConfigError::InsufficientNodes {
                    node_count: num_nodes,
                    required_shards: n,
                });
            }
            // Stagger starting node offset by object_id so multi-object placements distribute evenly across nodes
            let mut shard_to_node = BTreeMap::new();
            for s in 0..n {
                let node_idx = (s * 2 + obj_idx * 7) % num_nodes;
                // Ensure distinct nodes per object stripe
                let mut candidate = node_idx;
                while shard_to_node.values().any(|&v| v == NodeId(candidate)) {
                    candidate = (candidate + 1) % num_nodes;
                }
                shard_to_node.insert(ShardId(s), NodeId(candidate));
            }
            objects.push(ObjectPlacementState {
                spec,
                shard_to_node,
                lost_at_secs: BTreeMap::new(),
                in_flight: Vec::new(),
                irrecoverable_loss: false,
                loss_time_secs: None,
                repair_bytes: 0,
                repair_ops: 0,
                local_repairs: 0,
                global_repairs: 0,
                total_latency_secs: 0.0,
            });
        }

        let scheduler = PerNodeBandwidthScheduler::new(
            num_nodes,
            cfg.per_node_bandwidth_bps,
            cfg.base_config.repair_bandwidth_bytes_per_sec,
            cfg.base_config.repair_base_latency_secs,
        );

        Ok(Self {
            cfg,
            nodes,
            objects,
            scheduler,
        })
    }

    /// Runs the multi-object workload to completion across the exogenous failure trace.
    pub fn run(&mut self) -> Result<MultiObjectSimulationReport, ConfigError> {
        let trace = generate_exogenous_failure_trace(&self.cfg.base_config, 0);
        let horizon = self.cfg.base_config.horizon_secs();

        // Merge exogenous events with periodic controller checks every hour (3600s)
        let mut events = trace.events;
        let check_interval = self
            .cfg
            .base_config
            .controller_check_interval_secs
            .max(300.0);
        let mut t = check_interval;
        while t <= horizon {
            events.push(crate::simulation::events::ScheduledEvent {
                time_secs: t,
                sequence: 0,
                kind: EventKind::ControllerCheck,
            });
            t += check_interval;
        }
        events.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));

        for ev in events {
            if ev.time_secs > horizon {
                break;
            }
            self.complete_due_repairs(ev.time_secs);
            self.apply_event(ev.time_secs, &ev.kind);
            self.evaluate_and_schedule_repairs(ev.time_secs);
        }
        self.complete_due_repairs(horizon);

        let mut per_object_metrics = Vec::with_capacity(self.objects.len());
        let mut aggregate_repair_bandwidth_bytes = 0u64;
        let mut lost_objects = 0usize;

        for obj in &self.objects {
            if obj.irrecoverable_loss {
                lost_objects += 1;
            }
            aggregate_repair_bandwidth_bytes += obj.repair_bytes;
            let mean_lat = if obj.repair_ops > 0 {
                obj.total_latency_secs / (obj.repair_ops as f64)
            } else {
                0.0
            };
            per_object_metrics.push(PerObjectMetrics {
                object_id: obj.spec.object_id,
                irrecoverable_data_loss: obj.irrecoverable_loss,
                data_loss_time_secs: obj.loss_time_secs,
                repair_bandwidth_bytes: obj.repair_bytes,
                repair_operations: obj.repair_ops,
                local_group_repairs: obj.local_repairs,
                global_repairs: obj.global_repairs,
                mean_repair_latency_secs: mean_lat,
            });
        }

        let object_loss_fraction = if self.objects.is_empty() {
            0.0
        } else {
            (lost_objects as f64) / (self.objects.len() as f64)
        };

        Ok(MultiObjectSimulationReport {
            per_object_metrics,
            aggregate_repair_bandwidth_bytes,
            aggregate_repair_bandwidth_bps: (aggregate_repair_bandwidth_bytes as f64)
                / horizon.max(1.0),
            object_loss_fraction,
        })
    }

    fn complete_due_repairs(&mut self, now_secs: f64) {
        let num_nodes = self.nodes.len();
        for obj in &mut self.objects {
            if obj.irrecoverable_loss {
                obj.in_flight.clear();
                continue;
            }
            let mut remaining = Vec::new();
            for (finish_t, shard_id, bytes, is_local) in obj.in_flight.drain(..) {
                if finish_t <= now_secs + 1e-9 {
                    // Pick an online node not currently hosting a shard of this object
                    let occupied: BTreeSet<NodeId> = obj.shard_to_node.values().copied().collect();
                    if let Some(target) = (0..num_nodes)
                        .map(NodeId)
                        .find(|n| self.nodes[n.0].is_online() && !occupied.contains(n))
                    {
                        obj.shard_to_node.insert(shard_id, target);
                        obj.repair_bytes += bytes;
                        obj.repair_ops += 1;
                        if is_local {
                            obj.local_repairs += 1;
                        } else {
                            obj.global_repairs += 1;
                        }
                        if let Some(lost_t) = obj.lost_at_secs.remove(&shard_id) {
                            obj.total_latency_secs += (finish_t - lost_t).max(0.0);
                        }
                    }
                } else {
                    remaining.push((finish_t, shard_id, bytes, is_local));
                }
            }
            obj.in_flight = remaining;
        }
    }

    fn apply_event(&mut self, now_secs: f64, kind: &EventKind) {
        // Recover any nodes whose temporary outage has expired by `now_secs`
        for node in &mut self.nodes {
            if let NodeState::TemporarilyOffline {
                recover_at_secs, ..
            } = node.state
            {
                if now_secs >= recover_at_secs {
                    node.state = NodeState::Online;
                }
            }
        }

        match kind {
            EventKind::NodeOutage {
                node_id,
                duration_secs,
            } => {
                if let Some(node) = self.nodes.get_mut(node_id.0) {
                    if node.is_online() {
                        node.state = NodeState::TemporarilyOffline {
                            since_secs: now_secs,
                            recover_at_secs: now_secs + duration_secs,
                        };
                    }
                }
            }
            EventKind::NodePermanentFailure { node_id } => {
                self.fail_node_in_all_objects(*node_id, now_secs, true);
            }
            EventKind::DomainPermanentFailure { domain_id } => {
                let dom_nodes: Vec<NodeId> = self
                    .nodes
                    .iter()
                    .filter(|n| n.domain_id == *domain_id)
                    .map(|n| n.id)
                    .collect();
                for n_id in dom_nodes {
                    self.fail_node_in_all_objects(n_id, now_secs, false);
                }
            }
            EventKind::BurstNodeFailure {
                node_ids,
                is_permanent,
                outage_duration_secs,
            } => {
                if *is_permanent {
                    for &n_id in node_ids {
                        self.fail_node_in_all_objects(n_id, now_secs, true);
                    }
                } else {
                    for &n_id in node_ids {
                        if let Some(node) = self.nodes.get_mut(n_id.0) {
                            if node.is_online() {
                                node.state = NodeState::TemporarilyOffline {
                                    since_secs: now_secs,
                                    recover_at_secs: now_secs + outage_duration_secs,
                                };
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        // Check decodability of durable shards for each object
        for obj in &mut self.objects {
            if obj.irrecoverable_loss {
                continue;
            }
            let durable_shards: BTreeSet<ShardId> = obj
                .shard_to_node
                .iter()
                .filter(|(_, &n_id)| self.nodes[n_id.0].is_storage_durable())
                .map(|(&s_id, _)| s_id)
                .collect();
            if !obj.spec.coding.is_decodable(&durable_shards) {
                obj.irrecoverable_loss = true;
                obj.loss_time_secs = Some(now_secs);
                obj.in_flight.clear();
            }
        }
    }

    fn fail_node_in_all_objects(&mut self, node_id: NodeId, now_secs: f64, reprovision: bool) {
        if let Some(node) = self.nodes.get_mut(node_id.0) {
            if node.is_permanently_failed() {
                return;
            }
            if reprovision {
                node.state = NodeState::Online;
            } else {
                node.state = NodeState::PermanentlyFailed {
                    failed_at_secs: now_secs,
                };
            }
        }
        for obj in &mut self.objects {
            let lost: Vec<ShardId> = obj
                .shard_to_node
                .iter()
                .filter(|(_, &n)| n == node_id)
                .map(|(&s, _)| s)
                .collect();
            for s_id in lost {
                obj.shard_to_node.remove(&s_id);
                obj.lost_at_secs.entry(s_id).or_insert(now_secs);
            }
        }
    }

    fn evaluate_and_schedule_repairs(&mut self, now_secs: f64) {
        let num_nodes = self.nodes.len();
        for obj in &mut self.objects {
            if obj.irrecoverable_loss {
                continue;
            }
            let total_n = obj.spec.coding.total_shards();
            let online_shards: BTreeSet<ShardId> = obj
                .shard_to_node
                .iter()
                .filter(|(_, &n_id)| self.nodes[n_id.0].is_online())
                .map(|(&s_id, _)| s_id)
                .collect();

            let missing_shards: Vec<ShardId> = (0..total_n)
                .map(ShardId)
                .filter(|s_id| {
                    !obj.shard_to_node.contains_key(s_id)
                        && !obj.in_flight.iter().any(|(_, inf_s, _, _)| inf_s == s_id)
                })
                .collect();

            if missing_shards.is_empty() {
                continue;
            }

            let shard_bytes = obj.spec.shard_size_bytes();
            let occupied: BTreeSet<NodeId> = obj.shard_to_node.values().copied().collect();

            for missing_s in missing_shards {
                let Some(method) = obj.spec.coding.plan_repair(missing_s, &online_shards) else {
                    break;
                };
                let reads_needed = method.shards_to_read();
                let source_nodes: Vec<NodeId> = online_shards
                    .iter()
                    .filter_map(|s| obj.shard_to_node.get(s).copied())
                    .take(reads_needed)
                    .collect();
                if source_nodes.len() < reads_needed {
                    continue;
                }
                let Some(target_node) = (0..num_nodes)
                    .map(NodeId)
                    .find(|n| self.nodes[n.0].is_online() && !occupied.contains(n))
                else {
                    break;
                };

                let req = TransferRequest {
                    object_id: obj.spec.object_id,
                    shard_id: missing_s,
                    source_nodes,
                    target_node,
                    per_shard_bytes: shard_bytes,
                    now_secs,
                };
                let finish_secs = self.scheduler.schedule_transfer(&req);
                if finish_secs <= self.cfg.base_config.horizon_secs() {
                    let is_local = matches!(method, crate::coding::RepairMethod::LocalGroup { .. });
                    let total_bytes = method.total_transfer_bytes(shard_bytes);
                    obj.in_flight
                        .push((finish_secs, missing_s, total_bytes, is_local));
                }
            }
        }
    }
}
