//! Real shard health inspection, Phase 2 `FspAdaptiveV2Policy` storage adapter, and atomic
//! post-verified shard repair execution.

use std::collections::BTreeMap;
use std::fs;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::coding::codec::ErasureCodec;
use crate::coding::RepairMethod;
use crate::detection::{ClusterObservationView, DomainCorrelationSignal, ObservedNodeState};
use crate::model::node::NodeId;
use crate::model::shard::ShardId;
use crate::recovery::reconstruct::read_and_verify_shard_placement;
use crate::storage::backend::StorageError;
use crate::storage::container::{repair_or_create_container_stripe_atomic, ShardContainerReader};
use crate::storage::filesystem::{sync_directory, write_and_sync_file, FilesystemBackend};
use crate::storage::integrity::{sha256_bytes, sha256_file};
use crate::storage::manifest::ObjectManifest;
use crate::strategies::adaptive_v2::{AdaptiveV2Config, BlindRepairDecision, FspAdaptiveV2Policy};

static REPAIR_NONCE: AtomicU64 = AtomicU64::new(1);

/// Observed physical/integrity health of a single shard placement on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShardHealthState {
    /// Node is online and the shard file's byte length and SHA-256 digest match the manifest.
    HealthyVerified,
    /// Node is online, but the shard file does not exist on disk.
    Missing,
    /// Node is online, but the shard file is truncated, corrupted, or has stale/conflicting versions.
    Corrupted { reason: String },
    /// Hosting storage node is currently unreachable/offline.
    NodeUnreachable { node_id: NodeId },
}

/// Per-stripe health inspection summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeHealthReport {
    pub stripe_index: usize,
    pub healthy_shards: usize,
    pub missing_shards: Vec<ShardId>,
    pub corrupted_shards: Vec<(ShardId, String)>,
    pub unreachable_shards: Vec<(ShardId, NodeId)>,
    pub shard_states: BTreeMap<ShardId, ShardHealthState>,
    pub is_recoverable: bool,
}

/// High-level per-object operational resilience state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectHealthState {
    /// All data and parity shards intact across distinct failure domains.
    FullyProtected,
    /// Shard losses have occurred (< n - k), but >= k shards intact, reconstructable immediately.
    DegradedButRecoverable,
    /// Fewer than k shards currently reachable/online (e.g. transient host reboot or network partition).
    TemporarilyUnavailable,
    /// Redundancy reconstruction is needed, but cluster does not have sufficient eligible capacity/nodes.
    RepairBlockedByCapacity,
    /// Confirmed permanent loss of > n - k shards (< k survive permanently); object cannot be reconstructed.
    IrrecoverablyLost,
}

impl std::fmt::Display for ObjectHealthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FullyProtected => write!(f, "FullyProtected"),
            Self::DegradedButRecoverable => write!(f, "DegradedButRecoverable"),
            Self::TemporarilyUnavailable => write!(f, "TemporarilyUnavailable"),
            Self::RepairBlockedByCapacity => write!(f, "RepairBlockedByCapacity"),
            Self::IrrecoverablyLost => write!(f, "IrrecoverablyLost"),
        }
    }
}

/// Complete integrity and recoverability audit for a stored object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectHealthReport {
    pub object_id: String,
    pub object_size: u64,
    pub total_shards_per_stripe: usize,
    pub required_shards_per_stripe: usize,
    pub stripes: Vec<StripeHealthReport>,
    pub all_shards_healthy: bool,
    pub all_stripes_recoverable: bool,
}

impl ObjectHealthReport {
    /// Classifies the overall object health state based on per-stripe shard audits and cluster capacity.
    pub fn classify_state(&self, has_sufficient_repair_capacity: bool) -> ObjectHealthState {
        if self.all_shards_healthy {
            return ObjectHealthState::FullyProtected;
        }

        for stripe in &self.stripes {
            let total = self.total_shards_per_stripe;
            let k = self.required_shards_per_stripe;
            let permanent_losses = stripe.missing_shards.len() + stripe.corrupted_shards.len();
            let reachable_healthy = stripe.healthy_shards;

            // If even with all unreachable nodes returning healthy, surviving shards < k: IrrecoverablyLost
            if total.saturating_sub(permanent_losses) < k {
                return ObjectHealthState::IrrecoverablyLost;
            }

            // If currently reachable healthy < k:
            if reachable_healthy < k {
                return ObjectHealthState::TemporarilyUnavailable;
            }
        }

        // Shards are recoverable (every stripe has >= k healthy shards)
        if !has_sufficient_repair_capacity {
            return ObjectHealthState::RepairBlockedByCapacity;
        }

        ObjectHealthState::DegradedButRecoverable
    }
}

/// Inspects every stripe and shard placement of `manifest` on disk, verifying SHA-256 digests.
pub fn inspect_object_health(
    backend: &FilesystemBackend,
    manifest: &ObjectManifest,
) -> Result<ObjectHealthReport, StorageError> {
    let scheme = manifest.validate()?;
    let codec = ErasureCodec::from_scheme(scheme)?;
    let n = scheme.total_shards();
    let k = scheme.data_shards();

    let mut stripe_reports = Vec::with_capacity(manifest.stripes.len());
    let mut all_shards_healthy = true;
    let mut all_stripes_recoverable = true;
    let mut container_cache: BTreeMap<String, Option<ShardContainerReader>> = BTreeMap::new();

    for stripe in &manifest.stripes {
        let mut healthy_set = std::collections::BTreeSet::new();
        let mut missing_shards = Vec::new();
        let mut corrupted_shards = Vec::new();
        let mut unreachable_shards = Vec::new();
        let mut shard_states = BTreeMap::new();

        for placement in &stripe.placements {
            let shard_id = ShardId(placement.shard_id);
            let node_id = NodeId(placement.node_id);

            if !backend.is_node_online(node_id) {
                all_shards_healthy = false;
                unreachable_shards.push((shard_id, node_id));
                shard_states.insert(shard_id, ShardHealthState::NodeUnreachable { node_id });
                continue;
            }

            if placement.relative_path.ends_with(".fspc") {
                if !container_cache.contains_key(&placement.relative_path) {
                    let full_path = backend.resolve_safe_relative_path(&placement.relative_path)?;
                    if !full_path.exists() {
                        container_cache.insert(placement.relative_path.clone(), None);
                    } else {
                        match ShardContainerReader::open(&full_path) {
                            Ok(r)
                                if r.header().object_id == manifest.object_id
                                    && (r.header().shard_id as usize) == placement.shard_id
                                    && r.header().version == placement.version =>
                            {
                                container_cache.insert(placement.relative_path.clone(), Some(r));
                            }
                            _ => {
                                container_cache.insert(placement.relative_path.clone(), None);
                            }
                        }
                    }
                }
                match container_cache.get_mut(&placement.relative_path) {
                    Some(Some(reader)) => match reader.read_stripe(stripe.stripe_index) {
                        Ok(bytes)
                            if (bytes.len() as u64) == placement.size_bytes
                                && sha256_bytes(&bytes).to_hex() == placement.sha256_digest =>
                        {
                            healthy_set.insert(shard_id);
                            shard_states.insert(shard_id, ShardHealthState::HealthyVerified);
                        }
                        _ => {
                            all_shards_healthy = false;
                            let reason = "Container v2 stripe SHA-256 mismatch".to_string();
                            corrupted_shards.push((shard_id, reason.clone()));
                            shard_states.insert(shard_id, ShardHealthState::Corrupted { reason });
                        }
                    },
                    _ => {
                        all_shards_healthy = false;
                        let full_path =
                            backend.resolve_safe_relative_path(&placement.relative_path)?;
                        if !full_path.exists() {
                            missing_shards.push(shard_id);
                            shard_states.insert(shard_id, ShardHealthState::Missing);
                        } else {
                            let reason =
                                "Container v2 header/index checksum mismatch or truncated file"
                                    .to_string();
                            corrupted_shards.push((shard_id, reason.clone()));
                            shard_states.insert(shard_id, ShardHealthState::Corrupted { reason });
                        }
                    }
                }
                continue;
            }

            let mut rejected_extra = 0usize;
            match read_and_verify_shard_placement(
                backend,
                &manifest.object_id,
                stripe.stripe_index,
                placement,
                &mut rejected_extra,
            )? {
                Some(_) if rejected_extra == 0 => {
                    healthy_set.insert(shard_id);
                    shard_states.insert(shard_id, ShardHealthState::HealthyVerified);
                }
                Some(_) => {
                    // Canonical shard exists and is valid, but stale/conflicting version files exist alongside it
                    all_shards_healthy = false;
                    let reason = format!(
                        "conflicting or stale duplicate version files ({}) in shard directory",
                        rejected_extra
                    );
                    corrupted_shards.push((shard_id, reason.clone()));
                    shard_states.insert(shard_id, ShardHealthState::Corrupted { reason });
                }
                None => {
                    all_shards_healthy = false;
                    let full_path = backend.resolve_safe_relative_path(&placement.relative_path)?;
                    if !full_path.exists() {
                        missing_shards.push(shard_id);
                        shard_states.insert(shard_id, ShardHealthState::Missing);
                    } else {
                        let reason =
                            "SHA-256 digest mismatch or truncated shard file on disk".to_string();
                        corrupted_shards.push((shard_id, reason.clone()));
                        shard_states.insert(shard_id, ShardHealthState::Corrupted { reason });
                    }
                }
            }
        }

        let is_recoverable = scheme.is_decodable(&healthy_set) && codec.can_decode(&healthy_set);
        if !is_recoverable {
            all_stripes_recoverable = false;
        }

        stripe_reports.push(StripeHealthReport {
            stripe_index: stripe.stripe_index,
            healthy_shards: healthy_set.len(),
            missing_shards,
            corrupted_shards,
            unreachable_shards,
            shard_states,
            is_recoverable,
        });
    }

    Ok(ObjectHealthReport {
        object_id: manifest.object_id.clone(),
        object_size: manifest.object_size,
        total_shards_per_stripe: n,
        required_shards_per_stripe: k,
        stripes: stripe_reports,
        all_shards_healthy,
        all_stripes_recoverable,
    })
}

/// Compatibility adapter connecting the Phase 2 [`FspAdaptiveV2Policy`] and [`ClusterObservationView`]
/// to persistent storage health observations without accessing simulator ground truth.
pub struct AdaptiveStorageRepairPlanner {
    policy: FspAdaptiveV2Policy,
}

impl Default for AdaptiveStorageRepairPlanner {
    fn default() -> Self {
        Self::new(AdaptiveV2Config::default())
    }
}

impl AdaptiveStorageRepairPlanner {
    pub fn new(config: AdaptiveV2Config) -> Self {
        Self {
            policy: FspAdaptiveV2Policy::new(config),
        }
    }

    /// Plans which shards of `stripe_report` should be repaired right now based on:
    /// - Verified missing or corrupt shard files on online nodes (treated as confirmed permanent loss, $\hat{p}_{\text{perm}} = 1.0$)
    /// - Blind heartbeat observations (`ClusterObservationView`) for unreachable nodes (`Suspect` vs `AssumedLost`)
    /// - In-flight repairs whose host node may have recovered
    /// - Available repair bandwidth budget.
    pub fn plan_stripe_repair(
        &mut self,
        now_secs: f64,
        manifest: &ObjectManifest,
        stripe_report: &StripeHealthReport,
        observation_view: Option<&ClusterObservationView>,
        in_flight_shards: &[(ShardId, bool)],
        effective_bandwidth_bps: u64,
    ) -> BlindRepairDecision {
        let n = manifest.coding.n;
        let k = manifest.coding.k;
        let shard_len = manifest
            .stripes
            .get(stripe_report.stripe_index)
            .map(|s| s.shard_length as u64)
            .unwrap_or(65_536);
        let per_shard_repair_bytes = ((k as u64) + 1) * shard_len;

        let mut confirmed_shards: Vec<(ShardId, f64, f64, bool)> = Vec::new();
        let mut suspected_shards: Vec<(ShardId, f64, f64)> = Vec::new();

        // 1. Missing or SHA-256 corrupted files on reachable nodes are verified physical shard losses:
        for &shard_id in &stripe_report.missing_shards {
            confirmed_shards.push((shard_id, 1.0, 3_600.0, true));
        }
        for &(shard_id, _) in &stripe_report.corrupted_shards {
            confirmed_shards.push((shard_id, 1.0, 3_600.0, true));
        }

        // 2. Unreachable nodes are classified strictly through blind heartbeat observations:
        for &(shard_id, node_id) in &stripe_report.unreachable_shards {
            if let Some(obs) = observation_view
                .and_then(|v| v.node_observations.iter().find(|n| n.node_id == node_id))
            {
                match obs.state {
                    ObservedNodeState::ConfirmedUnavailableByPolicy {
                        first_missed_secs,
                        posterior_permanent_prob,
                        ..
                    } => {
                        confirmed_shards.push((
                            shard_id,
                            posterior_permanent_prob,
                            (now_secs - first_missed_secs).max(0.0),
                            true,
                        ));
                    }
                    ObservedNodeState::SuspectedUnavailable {
                        first_missed_secs,
                        posterior_permanent_prob,
                        ..
                    } => {
                        suspected_shards.push((
                            shard_id,
                            posterior_permanent_prob,
                            (now_secs - first_missed_secs).max(0.0),
                        ));
                    }
                    ObservedNodeState::Healthy { .. }
                    | ObservedNodeState::RecoveryObserved { .. }
                    | ObservedNodeState::Unknown => {}
                }
            } else {
                // Without a heartbeat confirm, treat newly unreachable node as Suspect
                suspected_shards.push((shard_id, 0.25, 180.0));
            }
        }

        let correlated_alert = observation_view
            .map(|v| {
                v.domain_signals
                    .iter()
                    .any(|s| matches!(s, DomainCorrelationSignal::CorrelatedDomainEvent { .. }))
            })
            .unwrap_or(false);

        self.policy.evaluate_blind(
            now_secs,
            n,
            k,
            stripe_report.healthy_shards,
            &confirmed_shards,
            &suspected_shards,
            in_flight_shards,
            1,
            n,
            correlated_alert,
            0,
            per_shard_repair_bytes,
            effective_bandwidth_bps,
            3_600.0,
        )
    }
}

/// Telemetry from executing an object repair operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairExecutionReport {
    pub object_id: String,
    pub stripes_inspected: usize,
    pub shards_repaired: usize,
    pub local_group_repairs: usize,
    pub global_reconstruction_repairs: usize,
    pub disk_bytes_read: u64,
    pub disk_bytes_written: u64,
    pub post_repair_all_healthy: bool,
}

/// Executes verified, atomic repair of all missing or corrupted shards (or the subset selected by
/// `only_shards_per_stripe`) for `manifest`.
///
/// If `interrupt_mid_repair` is `true`, simulates an interrupted repair after writing the `.repair.tmp`
/// file before atomic rename (for Scenario H testing).
pub fn execute_object_repair(
    backend: &FilesystemBackend,
    manifest: &ObjectManifest,
    only_shards_per_stripe: Option<&BTreeMap<usize, Vec<ShardId>>>,
    interrupt_mid_repair: bool,
) -> Result<RepairExecutionReport, StorageError> {
    let _write_lock_holder = backend.acquire_object_write_lock(&manifest.object_id);
    let _write_guard = _write_lock_holder.write();

    let scheme = manifest.validate()?;
    let codec = ErasureCodec::from_scheme(scheme)?;
    let k = codec.data_shards();

    let mut shards_repaired = 0usize;
    let mut local_group_repairs = 0usize;
    let mut global_reconstruction_repairs = 0usize;
    let mut disk_bytes_read = 0u64;
    let mut disk_bytes_written = 0u64;
    let mut v2_container_stripes: BTreeMap<usize, Vec<(usize, Vec<u8>)>> = BTreeMap::new();
    let is_v2_object = manifest
        .stripes
        .first()
        .and_then(|s| s.placements.first())
        .map(|p| p.relative_path.ends_with(".fspc"))
        .unwrap_or(false);

    for stripe in &manifest.stripes {
        let mut healthy_shards: Vec<(ShardId, Vec<u8>)> = Vec::new();
        let mut damaged_placements = Vec::new();

        for placement in &stripe.placements {
            let shard_id = ShardId(placement.shard_id);
            let mut rejected_extra = 0usize;
            match read_and_verify_shard_placement(
                backend,
                &manifest.object_id,
                stripe.stripe_index,
                placement,
                &mut rejected_extra,
            )? {
                Some(bytes) if rejected_extra == 0 => {
                    healthy_shards.push((shard_id, bytes));
                }
                _ => {
                    if let Some(filter_map) = only_shards_per_stripe {
                        if let Some(allowed) = filter_map.get(&stripe.stripe_index) {
                            if !allowed.contains(&shard_id) {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }
                    damaged_placements.push(placement.clone());
                }
            }
        }

        if damaged_placements.is_empty() {
            continue;
        }

        let healthy_id_set: std::collections::BTreeSet<ShardId> =
            healthy_shards.iter().map(|&(id, _)| id).collect();
        if !scheme.is_decodable(&healthy_id_set) || !codec.can_decode(&healthy_id_set) {
            return Err(StorageError::InsufficientValidShards {
                object_id: manifest.object_id.clone(),
                stripe_index: stripe.stripe_index,
                valid_shards: healthy_shards.len(),
                required_shards: k,
            });
        }

        // Check write admission (node online & quota) for every target node before repairing
        for dp in &damaged_placements {
            backend.check_node_write_admission(NodeId(dp.node_id), dp.size_bytes)?;
        }

        let avail_refs: Vec<(ShardId, &[u8])> = healthy_shards
            .iter()
            .map(|(id, v)| (*id, v.as_slice()))
            .collect();
        let target_ids: Vec<ShardId> = damaged_placements
            .iter()
            .map(|p| ShardId(p.shard_id))
            .collect();

        let (reconstructed_list, method) = codec
            .repair_shards(&target_ids, &avail_refs, stripe.shard_length)
            .map_err(|_| StorageError::InsufficientValidShards {
                object_id: manifest.object_id.clone(),
                stripe_index: stripe.stripe_index,
                valid_shards: healthy_shards.len(),
                required_shards: k,
            })?;

        let shards_read_for_op = method.shards_to_read();
        disk_bytes_read += (shards_read_for_op as u64) * (stripe.shard_length as u64);
        match method {
            RepairMethod::LocalGroup { .. } => local_group_repairs += target_ids.len(),
            RepairMethod::GlobalReconstruction { .. } => {
                global_reconstruction_repairs += target_ids.len()
            }
        }

        for (reconstructed_id, bytes) in reconstructed_list {
            let placement = damaged_placements
                .iter()
                .find(|p| p.shard_id == reconstructed_id.0)
                .expect("target placement exists");

            // 1. Verify reconstructed shard digest in memory BEFORE writing to disk
            let mem_digest = sha256_bytes(&bytes).to_hex();
            if (bytes.len() as u64) != placement.size_bytes || mem_digest != placement.sha256_digest
            {
                return Err(StorageError::ShardIntegrityFailure {
                    object_id: manifest.object_id.clone(),
                    stripe_index: stripe.stripe_index,
                    shard_id: reconstructed_id,
                    reason: format!(
                        "reconstructed shard SHA-256 {} != manifest {}",
                        mem_digest, placement.sha256_digest
                    ),
                });
            }

            if is_v2_object {
                v2_container_stripes
                    .entry(reconstructed_id.0)
                    .or_default()
                    .push((stripe.stripe_index, bytes));
                shards_repaired += 1;
                continue;
            }

            let target_full = backend.resolve_safe_relative_path(&placement.relative_path)?;
            if let Some(parent) = target_full.parent() {
                fs::create_dir_all(parent)?;
            }

            // 2. Write to temporary `.repair.tmp` file in target node directory and fsync
            let nonce = REPAIR_NONCE.fetch_add(1, Ordering::Relaxed);
            let tmp_repair_path = if interrupt_mid_repair {
                target_full.with_extension("fsp.repair.tmp")
            } else {
                target_full.with_extension(format!("fsp.repair.{}.tmp", nonce))
            };
            write_and_sync_file(&tmp_repair_path, &bytes)?;
            disk_bytes_written += bytes.len() as u64;

            if interrupt_mid_repair {
                return Err(StorageError::SimulatedCrash(
                    "InterruptedRepairAfterTempWrite".to_string(),
                ));
            }

            // 3. Re-read and verify `.repair.tmp` on disk before atomic rename
            let (disk_len, disk_digest) = sha256_file(&tmp_repair_path)?;
            if disk_len != placement.size_bytes || disk_digest.to_hex() != placement.sha256_digest {
                let _ = fs::remove_file(&tmp_repair_path);
                return Err(StorageError::ShardIntegrityFailure {
                    object_id: manifest.object_id.clone(),
                    stripe_index: stripe.stripe_index,
                    shard_id: reconstructed_id,
                    reason: "repair temp file failed post-write SHA-256 verification".to_string(),
                });
            }

            // 4. Clean up any stale/conflicting version files for this (stripe, shard) slot
            let prefix = format!(
                "stripe-{:06}.shard-{:02}.v",
                stripe.stripe_index, placement.shard_id
            );
            if let Some(parent) = target_full.parent() {
                if let Ok(entries) = fs::read_dir(parent) {
                    for entry in entries.flatten() {
                        if let Some(fname) = entry.file_name().to_str() {
                            if fname.starts_with(&prefix) && fname.ends_with(".fsp") {
                                let _ = fs::remove_file(entry.path());
                            }
                        }
                    }
                }
            }

            // 5. Atomically publish `.repair.tmp` -> canonical shard path and sync directory
            fs::rename(&tmp_repair_path, &target_full)?;
            if let Some(parent) = target_full.parent() {
                sync_directory(parent)?;
            }

            // 6. Final post-publication verification on the canonical shard path
            let (final_len, final_digest) = sha256_file(&target_full)?;
            if final_len != placement.size_bytes || final_digest.to_hex() != placement.sha256_digest
            {
                return Err(StorageError::ShardIntegrityFailure {
                    object_id: manifest.object_id.clone(),
                    stripe_index: stripe.stripe_index,
                    shard_id: reconstructed_id,
                    reason: "published shard failed post-rename SHA-256 verification".to_string(),
                });
            }

            shards_repaired += 1;
        }
    }

    if is_v2_object && !v2_container_stripes.is_empty() {
        for (shard_id, repaired_stripes) in v2_container_stripes {
            let first_placement = &manifest.stripes[0].placements[shard_id];
            let target_full = backend.resolve_safe_relative_path(&first_placement.relative_path)?;
            let mut full_stripes_owned: Vec<(usize, Vec<u8>)> =
                Vec::with_capacity(manifest.stripes.len());

            for stripe in &manifest.stripes {
                if let Some((_, rep_bytes)) = repaired_stripes
                    .iter()
                    .find(|(s_idx, _)| *s_idx == stripe.stripe_index)
                {
                    full_stripes_owned.push((stripe.payload_length, rep_bytes.clone()));
                } else {
                    let placement = &stripe.placements[shard_id];
                    let mut dummy = 0usize;
                    let existing = read_and_verify_shard_placement(
                        backend,
                        &manifest.object_id,
                        stripe.stripe_index,
                        placement,
                        &mut dummy,
                    )?
                    .ok_or_else(|| StorageError::ShardIntegrityFailure {
                        object_id: manifest.object_id.clone(),
                        stripe_index: stripe.stripe_index,
                        shard_id: ShardId(shard_id),
                        reason: "unrepaired stripe missing during v2 container rebuild".into(),
                    })?;
                    full_stripes_owned.push((stripe.payload_length, existing));
                }
            }

            let refs: Vec<(usize, &[u8])> = full_stripes_owned
                .iter()
                .map(|(plen, v)| (*plen, v.as_slice()))
                .collect();
            let nonce = REPAIR_NONCE.fetch_add(1, Ordering::Relaxed);
            let (hdr, _) = repair_or_create_container_stripe_atomic(
                &target_full,
                &manifest.object_id,
                scheme,
                shard_id,
                first_placement.version,
                &refs,
                nonce,
            )?;
            disk_bytes_written += hdr.total_file_size;
        }
    }

    let post_health = inspect_object_health(backend, manifest)?;

    Ok(RepairExecutionReport {
        object_id: manifest.object_id.clone(),
        stripes_inspected: manifest.stripes.len(),
        shards_repaired,
        local_group_repairs,
        global_reconstruction_repairs,
        disk_bytes_read,
        disk_bytes_written,
        post_repair_all_healthy: post_health.all_shards_healthy,
    })
}
