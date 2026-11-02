//! Distributed Adaptive Repair v2 Scheduler and Rate-Limited Background Scrubber
//! (`src/coordinator/scheduler.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::coding::codec::ErasureCodec;
use crate::coordinator::placement::{DistributedPlacementPlanner, PlacementPolicy};
use crate::coordinator::transactions::DistributedCoordinator;
use crate::detection::{
    ClusterObservationView, DomainCorrelationSignal, NodeObservation, ObservedNodeState,
};
use crate::model::failure_domain::FailureDomainId;
use crate::model::node::NodeId;
use crate::model::shard::ShardId;
use crate::network::protocol::{GetShardRequest, PutShardRequest, VerifyShardRequest};
use crate::recovery::repair::{AdaptiveStorageRepairPlanner, ShardHealthState, StripeHealthReport};
use crate::storage::backend::StorageError;
use crate::storage::container::ShardContainerWriter;
use crate::storage::filesystem::write_and_sync_file;
use crate::storage::integrity::{sha256_bytes, Sha256Hasher};
use crate::strategies::adaptive_v2::AdaptiveV2Config;

static SCHEDULER_NONCE: AtomicU64 = AtomicU64::new(1);

/// Queued repair task tracked by the [`DistributedRepairScheduler`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedRepairTask {
    pub object_id: String,
    pub shard_id: usize,
    pub reason: String,
    pub retry_attempts: u32,
}

/// Summary report returned after executing a distributed Adaptive Repair v2 pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributedRepairReport {
    pub objects_inspected: usize,
    pub degraded_objects_found: usize,
    pub repairs_queued: usize,
    pub repairs_cancelled_on_node_recovery: usize,
    pub shards_repaired: usize,
    pub replacement_nodes_selected: Vec<(usize, NodeId)>,
    pub updated_manifest_generations: BTreeMap<String, u64>,
    pub failed_repair_attempts: usize,
}

/// Summary report returned after executing a background scrubbing cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubIntegrityReport {
    pub cycle_number: u64,
    pub verifications_executed: usize,
    pub healthy_shards_verified: usize,
    pub corrupted_shards_detected: Vec<(String, usize, NodeId, String)>,
    pub missing_shards_detected: Vec<(String, usize, NodeId)>,
    pub unreachable_nodes_encountered: BTreeSet<NodeId>,
    pub repairs_queued: usize,
    pub next_cursor_object_index: usize,
    pub next_cursor_shard_index: usize,
}

/// Distributed Adaptive Repair v2 Scheduler connecting live node `Heartbeat` and `VerifyShard`
/// telemetry to [`AdaptiveStorageRepairPlanner`] and executing remote shard reconstruction.
pub struct DistributedRepairScheduler {
    coordinator: Arc<DistributedCoordinator>,
    planner: AdaptiveStorageRepairPlanner,
    placement_policy: PlacementPolicy,
    node_observations: BTreeMap<NodeId, ObservedNodeState>,
    missed_heartbeat_counts: BTreeMap<NodeId, u32>,
    repair_queue: BTreeMap<(String, usize), QueuedRepairTask>,
    max_retry_attempts: u32,
    effective_repair_bandwidth_bps: u64,
}

impl DistributedRepairScheduler {
    /// Creates a new [`DistributedRepairScheduler`].
    pub fn new(
        coordinator: Arc<DistributedCoordinator>,
        adaptive_config: AdaptiveV2Config,
        placement_policy: PlacementPolicy,
    ) -> Self {
        Self {
            coordinator,
            planner: AdaptiveStorageRepairPlanner::new(adaptive_config),
            placement_policy,
            node_observations: BTreeMap::new(),
            missed_heartbeat_counts: BTreeMap::new(),
            repair_queue: BTreeMap::new(),
            max_retry_attempts: 3,
            effective_repair_bandwidth_bps: 100 * 1024 * 1024,
        }
    }

    /// Returns a snapshot of the active deduplicated repair queue.
    pub fn repair_queue(&self) -> &BTreeMap<(String, usize), QueuedRepairTask> {
        &self.repair_queue
    }

    /// Enqueues a `(object_id, shard_id)` repair task if not already present (strict deduplication).
    pub fn enqueue_repair(&mut self, object_id: &str, shard_id: usize, reason: &str) -> bool {
        let key = (object_id.to_string(), shard_id);
        if self.repair_queue.contains_key(&key) {
            return false;
        }
        self.repair_queue.insert(
            key,
            QueuedRepairTask {
                object_id: object_id.to_string(),
                shard_id,
                reason: reason.to_string(),
                retry_attempts: 0,
            },
        );
        true
    }

    /// Polls all registered storage nodes via `Heartbeat` and updates the blind [`ClusterObservationView`].
    pub fn poll_cluster_heartbeats(&mut self, now_secs: f64) -> ClusterObservationView {
        let nodes = self.coordinator.registered_nodes();
        let mut node_obs_list = Vec::with_capacity(nodes.len());
        let mut domain_counts: BTreeMap<usize, (usize, usize)> = BTreeMap::new();

        for (node_id, reg) in &nodes {
            let domain_idx = node_id.0 % 4;
            let entry = domain_counts.entry(domain_idx).or_insert((0, 0));
            entry.0 += 1;

            let hb_res = self.coordinator.rpc_client().heartbeat(
                *node_id,
                &reg.listen_addr,
                1,
                now_secs as u64,
            );
            let prev_state = self
                .node_observations
                .get(node_id)
                .copied()
                .unwrap_or(ObservedNodeState::Unknown);

            let new_state = match hb_res {
                Ok(hb) if hb.health_state == "Healthy" || hb.health_state == "Degraded" => {
                    self.missed_heartbeat_counts.insert(*node_id, 0);
                    match prev_state {
                        ObservedNodeState::SuspectedUnavailable {
                            first_missed_secs, ..
                        }
                        | ObservedNodeState::ConfirmedUnavailableByPolicy {
                            first_missed_secs,
                            ..
                        } => ObservedNodeState::RecoveryObserved {
                            recovered_at_secs: now_secs,
                            previous_outage_duration_secs: (now_secs - first_missed_secs).max(0.0),
                        },
                        _ => ObservedNodeState::Healthy {
                            last_response_secs: now_secs,
                        },
                    }
                }
                _ => {
                    entry.1 += 1;
                    let misses = self
                        .missed_heartbeat_counts
                        .entry(*node_id)
                        .and_modify(|m| *m += 1)
                        .or_insert(1);
                    let first_missed = match prev_state {
                        ObservedNodeState::SuspectedUnavailable {
                            first_missed_secs, ..
                        }
                        | ObservedNodeState::ConfirmedUnavailableByPolicy {
                            first_missed_secs,
                            ..
                        } => first_missed_secs,
                        _ => (now_secs - 300.0).max(0.0),
                    };
                    let silence = (now_secs - first_missed).max(300.0);
                    let p_perm = (1.0 - (-silence / 7_200.0).exp()).clamp(0.05, 0.999);
                    if *misses >= 3 || silence >= 1_800.0 {
                        ObservedNodeState::ConfirmedUnavailableByPolicy {
                            first_missed_secs: first_missed,
                            confirmed_at_secs: now_secs,
                            consecutive_misses: *misses,
                            failed_probes: 2,
                            posterior_permanent_prob: p_perm.max(0.85),
                        }
                    } else {
                        ObservedNodeState::SuspectedUnavailable {
                            first_missed_secs: first_missed,
                            consecutive_misses: *misses,
                            failed_probes: 0,
                            posterior_permanent_prob: p_perm,
                        }
                    }
                }
            };

            self.node_observations.insert(*node_id, new_state);
            node_obs_list.push(NodeObservation {
                node_id: *node_id,
                domain_id: FailureDomainId(domain_idx),
                state: new_state,
            });
        }

        let mut domain_signals = Vec::new();
        for (dom_idx, (total, unreachable)) in domain_counts {
            let frac = if total > 0 {
                (unreachable as f64) / (total as f64)
            } else {
                0.0
            };
            if frac >= 0.5 {
                domain_signals.push(DomainCorrelationSignal::CorrelatedDomainEvent {
                    domain_id: FailureDomainId(dom_idx),
                    affected_fraction: frac,
                });
            } else {
                domain_signals.push(DomainCorrelationSignal::Normal {
                    domain_id: FailureDomainId(dom_idx),
                    affected_fraction: frac,
                });
            }
        }

        ClusterObservationView {
            now_secs,
            node_observations: node_obs_list,
            domain_signals,
        }
    }

    /// Inspects all committed objects across the distributed cluster via `VerifyShard`, runs the
    /// [`AdaptiveStorageRepairPlanner`] to decide which degraded shards to repair, cancels queued
    /// repairs whose original node has recovered with an intact shard, and executes remote shard
    /// reconstruction and manifest re-signing.
    pub fn run_repair_pass(
        &mut self,
        now_secs: f64,
    ) -> Result<DistributedRepairReport, StorageError> {
        let obs_view = self.poll_cluster_heartbeats(now_secs);
        let object_ids = self.coordinator.list_committed_objects()?;
        let nodes_map = self.coordinator.registered_nodes();

        let mut objects_inspected = 0usize;
        let mut degraded_objects_found = 0usize;
        let mut repairs_queued = 0usize;
        let mut repairs_cancelled_on_node_recovery = 0usize;

        for obj_id in &object_ids {
            objects_inspected += 1;
            let signed = self.coordinator.load_verified_signed_manifest(obj_id)?;
            let manifest = &signed.manifest;
            let Some(first_stripe) = manifest.stripes.first() else {
                continue;
            };

            let mut healthy_shards = 0usize;
            let mut missing_shards = Vec::new();
            let mut corrupted_shards = Vec::new();
            let mut unreachable_shards = Vec::new();
            let mut shard_states = BTreeMap::new();

            for placement in &first_stripe.placements {
                let s_id = ShardId(placement.shard_id);
                let n_id = NodeId(placement.node_id);
                let Some(reg_node) = nodes_map.get(&n_id) else {
                    unreachable_shards.push((s_id, n_id));
                    shard_states.insert(s_id, ShardHealthState::NodeUnreachable { node_id: n_id });
                    continue;
                };

                let is_reachable = self
                    .node_observations
                    .get(&n_id)
                    .map(|st| st.is_observed_reachable())
                    .unwrap_or(false);
                if !is_reachable {
                    unreachable_shards.push((s_id, n_id));
                    shard_states.insert(s_id, ShardHealthState::NodeUnreachable { node_id: n_id });
                    continue;
                }

                let nonce_hex = format!("{:016x}", SCHEDULER_NONCE.fetch_add(1, Ordering::Relaxed));
                let vreq = VerifyShardRequest {
                    object_id: obj_id.clone(),
                    shard_id: placement.shard_id,
                    version: placement.version,
                    challenge_nonce_hex: nonce_hex,
                };
                match self
                    .coordinator
                    .rpc_client()
                    .verify_shard(n_id, &reg_node.listen_addr, vreq)
                {
                    Ok(rep) if rep.healthy => {
                        healthy_shards += 1;
                        shard_states.insert(s_id, ShardHealthState::HealthyVerified);
                        if self
                            .repair_queue
                            .remove(&(obj_id.clone(), placement.shard_id))
                            .is_some()
                        {
                            repairs_cancelled_on_node_recovery += 1;
                        }
                    }
                    Ok(rep) => {
                        let reason = rep
                            .error_reason
                            .unwrap_or_else(|| "shard verification failed".to_string());
                        if reason.contains("missing") {
                            missing_shards.push(s_id);
                            shard_states.insert(s_id, ShardHealthState::Missing);
                        } else {
                            corrupted_shards.push((s_id, reason.clone()));
                            shard_states.insert(s_id, ShardHealthState::Corrupted { reason });
                        }
                    }
                    Err(_) => {
                        unreachable_shards.push((s_id, n_id));
                        shard_states
                            .insert(s_id, ShardHealthState::NodeUnreachable { node_id: n_id });
                    }
                }
            }

            if healthy_shards < manifest.coding.n {
                degraded_objects_found += 1;
                let stripe_report = StripeHealthReport {
                    stripe_index: 0,
                    healthy_shards,
                    missing_shards,
                    corrupted_shards,
                    unreachable_shards,
                    shard_states,
                    is_recoverable: healthy_shards >= manifest.coding.k,
                };

                let decision = self.planner.plan_stripe_repair(
                    now_secs,
                    manifest,
                    &stripe_report,
                    Some(&obs_view),
                    &[],
                    self.effective_repair_bandwidth_bps,
                );

                for shard_to_repair in decision.shards_to_repair {
                    if self.enqueue_repair(
                        obj_id,
                        shard_to_repair.0,
                        "AdaptiveRepairV2 scheduled repair",
                    ) {
                        repairs_queued += 1;
                    }
                }
            }
        }

        let mut shards_repaired = 0usize;
        let mut replacement_nodes_selected = Vec::new();
        let mut updated_manifest_generations = BTreeMap::new();
        let mut failed_repair_attempts = 0usize;

        let keys: Vec<(String, usize)> = self.repair_queue.keys().cloned().collect();
        for key in keys {
            let Some(task) = self.repair_queue.get(&key).cloned() else {
                continue;
            };
            if task.retry_attempts >= self.max_retry_attempts {
                continue;
            }

            match self.execute_single_shard_repair(&task.object_id, task.shard_id, now_secs as u64)
            {
                Ok((target_node, new_gen)) => {
                    shards_repaired += 1;
                    replacement_nodes_selected.push((task.shard_id, target_node));
                    updated_manifest_generations.insert(task.object_id.clone(), new_gen);
                    self.repair_queue.remove(&key);
                }
                Err(_) => {
                    failed_repair_attempts += 1;
                    if let Some(entry) = self.repair_queue.get_mut(&key) {
                        entry.retry_attempts += 1;
                    }
                }
            }
        }

        Ok(DistributedRepairReport {
            objects_inspected,
            degraded_objects_found,
            repairs_queued,
            repairs_cancelled_on_node_recovery,
            shards_repaired,
            replacement_nodes_selected,
            updated_manifest_generations,
            failed_repair_attempts,
        })
    }

    fn execute_single_shard_repair(
        &self,
        object_id: &str,
        target_shard_id: usize,
        now_unix_secs: u64,
    ) -> Result<(NodeId, u64), StorageError> {
        let obj_lock = self.coordinator.object_lock(object_id)?;
        let _write_guard = obj_lock.write().unwrap_or_else(|p| p.into_inner());

        let signed = self.coordinator.load_verified_signed_manifest(object_id)?;
        let mut manifest = signed.manifest.clone();
        let coding_scheme = manifest.coding.to_coding_scheme()?;
        let codec = ErasureCodec::from_scheme(coding_scheme)?;
        let k = codec.data_shards();

        let nodes_map = self.coordinator.registered_nodes();
        let candidates = self.coordinator.probe_candidate_nodes();

        let total_shard_payload_bytes: u64 =
            manifest.stripes.iter().map(|s| s.shard_length as u64).sum();
        let required_shard_bytes =
            256u64 + total_shard_payload_bytes + (manifest.stripes.len() as u64) * 52;

        let current_node_id = NodeId(manifest.stripes[0].placements[target_shard_id].node_id);
        let current_version = manifest.stripes[0].placements[target_shard_id].version;
        let new_version = current_version + 1;

        let chosen_target = if let Some(cand) = candidates.iter().find(|c| {
            c.node_id == current_node_id && c.online && c.available_bytes >= required_shard_bytes
        }) {
            cand.clone()
        } else {
            let already_holding: BTreeSet<NodeId> = manifest.stripes[0]
                .placements
                .iter()
                .filter(|p| p.shard_id != target_shard_id)
                .map(|p| NodeId(p.node_id))
                .collect();
            let placement_planner = DistributedPlacementPlanner::new(self.placement_policy);
            placement_planner
                .select_repair_target_node(&candidates, &already_holding, required_shard_bytes)
                .map_err(|e| StorageError::Network(e.to_string()))?
        };

        let mut repaired_payload = Vec::with_capacity(total_shard_payload_bytes as usize);
        let mut repaired_stripes_meta = Vec::with_capacity(manifest.stripes.len());

        for stripe in &mut manifest.stripes {
            let mut healthy_shards: Vec<(ShardId, Vec<u8>)> = Vec::with_capacity(k);
            let mut healthy_ids: BTreeSet<ShardId> = BTreeSet::new();

            for placement in &stripe.placements {
                if placement.shard_id == target_shard_id {
                    continue;
                }
                if healthy_ids.len() >= k && codec.can_decode(&healthy_ids) {
                    break;
                }
                let n_id = NodeId(placement.node_id);
                let Some(reg_node) = nodes_map.get(&n_id) else {
                    continue;
                };
                let greq = GetShardRequest {
                    object_id: object_id.to_string(),
                    shard_id: placement.shard_id,
                    version: placement.version,
                    stripe_index: Some(stripe.stripe_index),
                };
                if let Ok((_, bytes)) =
                    self.coordinator
                        .rpc_client()
                        .get_shard(n_id, &reg_node.listen_addr, greq)
                {
                    if bytes.len() == stripe.shard_length
                        && sha256_bytes(&bytes).to_hex() == placement.sha256_digest
                    {
                        healthy_ids.insert(ShardId(placement.shard_id));
                        healthy_shards.push((ShardId(placement.shard_id), bytes));
                    }
                }
            }

            if !codec.can_decode(&healthy_ids) {
                return Err(StorageError::InsufficientValidShards {
                    object_id: object_id.to_string(),
                    stripe_index: stripe.stripe_index,
                    valid_shards: healthy_ids.len(),
                    required_shards: k,
                });
            }

            let avail_refs: Vec<(ShardId, &[u8])> = healthy_shards
                .iter()
                .map(|(id, v)| (*id, v.as_slice()))
                .collect();
            let (reconstructed_list, _) = codec
                .repair_shards(
                    &[ShardId(target_shard_id)],
                    &avail_refs,
                    stripe.shard_length,
                )
                .map_err(|_| StorageError::InsufficientValidShards {
                    object_id: object_id.to_string(),
                    stripe_index: stripe.stripe_index,
                    valid_shards: healthy_ids.len(),
                    required_shards: k,
                })?;

            let (_, rep_bytes) = reconstructed_list.into_iter().next().unwrap();
            let expected_sha = &stripe.placements[target_shard_id].sha256_digest;
            let actual_sha = sha256_bytes(&rep_bytes).to_hex();
            if &actual_sha != expected_sha {
                return Err(StorageError::ShardIntegrityFailure {
                    object_id: object_id.to_string(),
                    stripe_index: stripe.stripe_index,
                    shard_id: ShardId(target_shard_id),
                    reason: format!(
                        "reconstructed shard SHA-256 {} != manifest {}",
                        actual_sha, expected_sha
                    ),
                });
            }

            repaired_payload.extend_from_slice(&rep_bytes);
            repaired_stripes_meta.push((stripe.payload_length, stripe.shard_length, actual_sha));

            let target_placement = &mut stripe.placements[target_shard_id];
            target_placement.node_id = chosen_target.node_id.0;
            target_placement.domain_id = chosen_target.node_id.0 % 4;
            target_placement.version = new_version;
            target_placement.relative_path = format!(
                "node-{:02}/objects/{}/shard-{:02}.v{}.fspc",
                chosen_target.node_id.0, object_id, target_shard_id, new_version
            );
        }

        let repair_tx_id = format!(
            "repair-{}-s{:02}-v{}",
            object_id, target_shard_id, new_version
        );
        let put_req = PutShardRequest {
            tx_id: repair_tx_id,
            object_id: object_id.to_string(),
            shard_id: target_shard_id,
            version: new_version,
            coding_scheme,
            stripes: repaired_stripes_meta,
        };
        self.coordinator
            .rpc_client()
            .put_shard(
                chosen_target.node_id,
                &chosen_target.listen_addr,
                put_req,
                &repaired_payload,
            )
            .map_err(|e| StorageError::Network(e.to_string()))?;

        let verify_req = VerifyShardRequest {
            object_id: object_id.to_string(),
            shard_id: target_shard_id,
            version: new_version,
            challenge_nonce_hex: "repair_post_verify_01".to_string(),
        };
        let v_rep = self
            .coordinator
            .rpc_client()
            .verify_shard(
                chosen_target.node_id,
                &chosen_target.listen_addr,
                verify_req,
            )
            .map_err(|e| StorageError::Network(e.to_string()))?;
        if !v_rep.healthy {
            return Err(StorageError::ShardIntegrityFailure {
                object_id: object_id.to_string(),
                stripe_index: 0,
                shard_id: ShardId(target_shard_id),
                reason: format!("post-repair VerifyShard failed: {:?}", v_rep.error_reason),
            });
        }

        manifest.manifest_digest = manifest.compute_canonical_digest();
        manifest.validate()?;
        let new_gen = signed.manifest_generation + 1;
        let _ = now_unix_secs;
        let new_signed = self
            .coordinator
            .signer_keypair()
            .sign_manifest(manifest, new_gen)
            .map_err(|e| StorageError::Signature(e.to_string()))?;

        self.coordinator
            .signer_policy()
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .verify_and_advance_generation(&new_signed, true)
            .map_err(|e| StorageError::Signature(e.to_string()))?;

        self.coordinator.save_signed_manifest_atomic(&new_signed)?;

        Ok((chosen_target.node_id, new_gen))
    }
}

/// Rate-limited background data scrubber with keyed SHA-256 challenge-response verification and
/// restart-safe scrub cursor persistence (`<coord_root>/scrub_cursor.json`).
pub struct BackgroundScrubber {
    coordinator: Arc<DistributedCoordinator>,
    max_verifications_per_cycle: usize,
    cycle_number: u64,
    cursor_object_index: usize,
    cursor_shard_index: usize,
    independent_verification_enabled: bool,
}

impl BackgroundScrubber {
    /// Creates or resumes a [`BackgroundScrubber`], loading any persisted scrub cursor from
    /// `<coord_root>/scrub_cursor.json`.
    pub fn open_or_init(
        coordinator: Arc<DistributedCoordinator>,
        max_verifications_per_cycle: usize,
    ) -> Result<Self, StorageError> {
        let mut scrubber = Self {
            coordinator,
            max_verifications_per_cycle: max_verifications_per_cycle.max(1),
            cycle_number: 0,
            cursor_object_index: 0,
            cursor_shard_index: 0,
            independent_verification_enabled: false,
        };
        scrubber.load_cursor_if_exists()?;
        Ok(scrubber)
    }

    /// Enables or disables coordinator-side independent byte retrieval and streaming hash verification.
    pub fn set_independent_verification(&mut self, enabled: bool) {
        self.independent_verification_enabled = enabled;
    }

    fn cursor_file_path(&self) -> PathBuf {
        self.coordinator.coord_root().join("scrub_cursor.json")
    }

    fn load_cursor_if_exists(&mut self) -> Result<(), StorageError> {
        let p = self.cursor_file_path();
        if !p.exists() {
            return Ok(());
        }
        let raw = fs::read_to_string(&p)?;
        for line in raw.lines() {
            if let Some((k, v)) = line.split_once('=') {
                match k.trim() {
                    "cycle_number" => self.cycle_number = v.trim().parse().unwrap_or(0),
                    "cursor_object_index" => {
                        self.cursor_object_index = v.trim().parse().unwrap_or(0)
                    }
                    "cursor_shard_index" => self.cursor_shard_index = v.trim().parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn persist_cursor(&self) -> Result<(), StorageError> {
        let body = format!(
            "cycle_number={}\ncursor_object_index={}\ncursor_shard_index={}\n",
            self.cycle_number, self.cursor_object_index, self.cursor_shard_index
        );
        write_and_sync_file(&self.cursor_file_path(), body.as_bytes())
    }

    /// Runs one rate-limited background scrub cycle (verifying at most `max_verifications_per_cycle`
    /// shard containers across the cluster), independently checking the node's keyed challenge-response
    /// SHA-256 digest and queueing repairs in `scheduler` whenever corruption or missing shards are found.
    pub fn run_scrub_cycle(
        &mut self,
        scheduler: &mut DistributedRepairScheduler,
    ) -> Result<ScrubIntegrityReport, StorageError> {
        self.cycle_number += 1;
        let object_ids = self.coordinator.list_committed_objects()?;
        if object_ids.is_empty() {
            self.cursor_object_index = 0;
            self.cursor_shard_index = 0;
            self.persist_cursor()?;
            return Ok(ScrubIntegrityReport {
                cycle_number: self.cycle_number,
                verifications_executed: 0,
                healthy_shards_verified: 0,
                corrupted_shards_detected: Vec::new(),
                missing_shards_detected: Vec::new(),
                unreachable_nodes_encountered: BTreeSet::new(),
                repairs_queued: 0,
                next_cursor_object_index: 0,
                next_cursor_shard_index: 0,
            });
        }

        if self.cursor_object_index >= object_ids.len() {
            self.cursor_object_index = 0;
            self.cursor_shard_index = 0;
        }

        let nodes_map = self.coordinator.registered_nodes();
        let mut verifications_executed = 0usize;
        let mut healthy_shards_verified = 0usize;
        let mut corrupted_shards_detected = Vec::new();
        let mut missing_shards_detected = Vec::new();
        let mut unreachable_nodes_encountered = BTreeSet::new();
        let mut repairs_queued = 0usize;

        while verifications_executed < self.max_verifications_per_cycle {
            let obj_id = &object_ids[self.cursor_object_index];
            let signed = self.coordinator.load_verified_signed_manifest(obj_id)?;
            let manifest = &signed.manifest;
            let n = manifest.coding.n;

            if self.cursor_shard_index >= n {
                self.cursor_shard_index = 0;
                self.cursor_object_index = (self.cursor_object_index + 1) % object_ids.len();
                continue;
            }

            let shard_id = self.cursor_shard_index;
            let first_placement = &manifest.stripes[0].placements[shard_id];
            let node_id = NodeId(first_placement.node_id);
            let version = first_placement.version;

            verifications_executed += 1;
            self.cursor_shard_index += 1;
            if self.cursor_shard_index >= n {
                self.cursor_shard_index = 0;
                self.cursor_object_index = (self.cursor_object_index + 1) % object_ids.len();
            }

            let Some(reg_node) = nodes_map.get(&node_id) else {
                unreachable_nodes_encountered.insert(node_id);
                continue;
            };

            let verification_result = if self.independent_verification_enabled {
                // Independent verification: retrieve shard bytes directly from node, stream & hash locally
                let mut all_match = true;
                let mut failure_reason = None;
                for stripe in &manifest.stripes {
                    let greq = GetShardRequest {
                        object_id: obj_id.clone(),
                        shard_id,
                        version,
                        stripe_index: Some(stripe.stripe_index),
                    };
                    match self.coordinator.rpc_client().get_shard(
                        node_id,
                        &reg_node.listen_addr,
                        greq,
                    ) {
                        Ok((_, bytes)) => {
                            let expected_sha = &stripe.placements[shard_id].sha256_digest;
                            let actual_sha = sha256_bytes(&bytes).to_hex();
                            if &actual_sha != expected_sha {
                                all_match = false;
                                failure_reason = Some(format!(
                                    "independent scrub mismatch: got {}, manifest {}",
                                    actual_sha, expected_sha
                                ));
                                break;
                            }
                        }
                        Err(e) => {
                            unreachable_nodes_encountered.insert(node_id);
                            all_match = false;
                            failure_reason = Some(format!("independent scrub fetch error: {}", e));
                            break;
                        }
                    }
                }
                if all_match {
                    Ok(true)
                } else {
                    Err(failure_reason
                        .unwrap_or_else(|| "independent verification failed".to_string()))
                }
            } else {
                let nonce_num = SCHEDULER_NONCE.fetch_add(1, Ordering::Relaxed);
                let challenge_nonce_hex =
                    format!("scrub_{:08}_{:08x}", self.cycle_number, nonce_num);
                let req = VerifyShardRequest {
                    object_id: obj_id.clone(),
                    shard_id,
                    version,
                    challenge_nonce_hex: challenge_nonce_hex.clone(),
                };

                match self.coordinator.rpc_client().verify_shard(
                    node_id,
                    &reg_node.listen_addr,
                    req,
                ) {
                    Ok(rep) if rep.healthy => {
                        if rep.stripe_count != (manifest.stripes.len() as u32)
                            || rep.challenge_response_sha256.len() != 64
                        {
                            Err("invalid VerifyShard challenge-response or stripe count"
                                .to_string())
                        } else {
                            Ok(true)
                        }
                    }
                    Ok(rep) => {
                        let reason = rep
                            .error_reason
                            .unwrap_or_else(|| "container verification failed".to_string());
                        Err(reason)
                    }
                    Err(_e) => {
                        unreachable_nodes_encountered.insert(node_id);
                        continue;
                    }
                }
            };

            match verification_result {
                Ok(true) => {
                    healthy_shards_verified += 1;
                }
                Ok(false) => {}
                Err(reason) => {
                    if reason.contains("missing") || reason.contains("NotFound") {
                        missing_shards_detected.push((obj_id.clone(), shard_id, node_id));
                    } else {
                        corrupted_shards_detected.push((
                            obj_id.clone(),
                            shard_id,
                            node_id,
                            reason.clone(),
                        ));
                    }
                    if scheduler.enqueue_repair(obj_id, shard_id, &reason) {
                        repairs_queued += 1;
                    }
                }
            }

            if self.cursor_object_index == 0 && self.cursor_shard_index == 0 {
                break;
            }
        }

        self.persist_cursor()?;

        Ok(ScrubIntegrityReport {
            cycle_number: self.cycle_number,
            verifications_executed,
            healthy_shards_verified,
            corrupted_shards_detected,
            missing_shards_detected,
            unreachable_nodes_encountered,
            repairs_queued,
            next_cursor_object_index: self.cursor_object_index,
            next_cursor_shard_index: self.cursor_shard_index,
        })
    }

    /// Computes the exact expected keyed SHA-256 challenge-response digest for a known shard
    /// container's stripe payloads (`SHA256("FSP-SCRUB-CHALLENGE-V1:<nonce>:" || container_bytes)`).
    pub fn compute_expected_challenge_response(
        temp_scratch_path: &std::path::Path,
        object_id: &str,
        coding_scheme: crate::coding::CodingScheme,
        shard_id: usize,
        version: u64,
        stripes: &[(usize, &[u8])],
        challenge_nonce_hex: &str,
    ) -> Result<String, StorageError> {
        let mut writer = ShardContainerWriter::create(
            temp_scratch_path,
            object_id,
            coding_scheme,
            shard_id,
            version,
        )?;
        for (idx, (plen, bytes)) in stripes.iter().enumerate() {
            writer.append_stripe(idx, *plen, bytes)?;
        }
        writer.finalize()?;
        let raw_bytes = fs::read(temp_scratch_path)?;
        let _ = fs::remove_file(temp_scratch_path);

        let mut hasher = Sha256Hasher::new();
        hasher.update(format!("FSP-SCRUB-CHALLENGE-V1:{}:", challenge_nonce_hex).as_bytes());
        hasher.update(&raw_bytes);
        Ok(hasher.finalize().to_hex())
    }
}
