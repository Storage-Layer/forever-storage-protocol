//! Distributed Coordinator PUT/GET Transaction Protocol, Crash Recovery, and Signed Manifest
//! Persistence (`src/coordinator/transactions.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

use crate::coding::codec::ErasureCodec;
use crate::coding::stripe::{compute_stripe_shard_length, DEFAULT_STRIPE_SIZE_BYTES};
use crate::coding::CodingScheme;
use crate::coordinator::placement::{
    CandidateNodeInfo, DistributedPlacementPlanner, PlacementDistributionReport, PlacementPolicy,
};
use crate::model::node::NodeId;
use crate::model::shard::ShardId;
use crate::network::client::NodeRpcClient;
use crate::network::protocol::{GetShardRequest, PutShardAck, PutShardRequest};
use crate::node::identity::NodeFailureDomainTopology;
use crate::security::signatures::{
    ManifestSignerKeypair, MetadataBackupBundle, SignedObjectManifest, TrustedSignerPolicy,
};
use crate::storage::backend::StorageError;
use crate::storage::crash_point::{trigger_crash_point, CrashPoint};
use crate::storage::filesystem::{
    check_path_has_no_traversal, create_file_nofollow, ensure_not_symlink, sync_directory,
    write_and_sync_file,
};
use crate::storage::integrity::{sha256_bytes, Sha256Digest, Sha256Hasher};
use crate::storage::manifest::{
    validate_safe_identifier, ManifestCodingSpec, ObjectManifest, ShardPlacementRecord,
    StripeManifest, MANIFEST_FORMAT_VERSION,
};
use crate::storage::DurabilityLevel;

/// Sentinel marker file for a dedicated Coordinator metadata root directory.
pub const FSP_COORDINATOR_MARKER_FILE: &str = ".fsp-coordinator-marker";
pub const FSP_COORDINATOR_MARKER_MAGIC: &str = "FSP_COORDINATOR_METADATA_ROOT_V1";

static COORD_TX_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Policy controlling whether a distributed `PUT` requires all $n$ shards to be acknowledged or
/// allows an explicit degraded commit above a minimum quorum $\ge k$.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DegradedCommitPolicy {
    /// Default strict policy: all $n$ shards must be durably acknowledged before committing.
    StrictAllShards,
    /// Explicit degraded commit policy: requires at least `min_acknowledged_shards` ($\ge k$)
    /// durably acknowledged shards, and immediately marks unacknowledged shards for background repair.
    AllowDegradedCommit { min_acknowledged_shards: usize },
}

/// Options for [`DistributedCoordinator::distributed_put_bytes`] and [`DistributedCoordinator::distributed_put_reader`].
#[derive(Debug, Clone)]
pub struct DistributedPutOptions {
    pub coding_scheme: CodingScheme,
    pub stripe_size: usize,
    pub placement_policy: PlacementPolicy,
    pub commit_policy: DegradedCommitPolicy,
    /// Optional explicit `tx_id` (used to test idempotent transaction replay).
    pub tx_id_override: Option<String>,
}

impl Default for DistributedPutOptions {
    fn default() -> Self {
        Self {
            coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
            stripe_size: DEFAULT_STRIPE_SIZE_BYTES,
            placement_policy: PlacementPolicy::default(),
            commit_policy: DegradedCommitPolicy::StrictAllShards,
            tx_id_override: None,
        }
    }
}

/// Registered storage node endpoint known to the [`DistributedCoordinator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisteredStorageNode {
    pub node_id: NodeId,
    pub listen_addr: String,
    pub topology: NodeFailureDomainTopology,
}

/// Detailed persistence and fault-tolerance semantics of a committed PUT transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurabilityContractReport {
    /// Nominal base durability level (e.g. Recoverable).
    pub durability_level: DurabilityLevel,
    /// Whether minimum erasure-coded reconstruction threshold (>= k distinct shards) is locally durable.
    pub is_recoverable: bool,
    /// Count of distinct durable shard indices verified.
    pub distinct_durable_shards: usize,
    /// Total configured shards in stripe (e.g. 20).
    pub total_shards: usize,
    /// Minimum required shards to reconstruct original bytes (e.g. 12).
    pub min_shards: usize,
    /// Whether all configured shards (n of n) were confirmed locally durable on storage nodes.
    pub full_redundancy_achieved: bool,
    /// Whether the failure-domain placement policy (e.g. cross-zone diversity) is satisfied.
    pub failure_domain_policy_satisfied: bool,
    /// Whether the authoritative metadata manifest was committed to the monotonic WAL with disk barrier.
    pub metadata_quorum_committed: bool,
}

/// Result of a committed distributed `PUT` transaction.
#[derive(Debug, Clone)]
pub struct DistributedPutReport {
    pub object_id: String,
    pub tx_id: String,
    pub signed_manifest: SignedObjectManifest,
    pub placement_report: PlacementDistributionReport,
    pub acknowledged_nodes: BTreeSet<NodeId>,
    pub degraded_unacknowledged_shards: Vec<usize>,
    pub total_network_bytes_sent: u64,
    pub idempotent_replay_count: usize,
    pub durability_level: DurabilityLevel,
    pub durability_contract: DurabilityContractReport,
}

/// Result of a verified distributed `GET` reconstruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DistributedGetReport {
    pub object_id: String,
    pub object_size: u64,
    pub object_sha256: Sha256Digest,
    pub manifest_generation: u64,
    pub nodes_queried: BTreeSet<NodeId>,
    pub nodes_failed_or_corrupt: BTreeSet<NodeId>,
    pub stripes_reconstructed: usize,
}

/// Startup crash recovery report for the [`DistributedCoordinator`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoordinatorCrashRecoveryReport {
    pub uncommitted_txs_aborted: usize,
    pub remote_orphan_shards_deleted: usize,
    pub verified_signed_manifests: usize,
}

#[derive(Debug, Clone)]
struct StripeLayoutInfo {
    stripe_index: usize,
    payload_offset: u64,
    payload_length: usize,
    shard_length: usize,
}

fn plan_stripe_layouts(object_size: u64, stripe_size: usize, k: usize) -> Vec<StripeLayoutInfo> {
    if object_size == 0 {
        let shard_length = compute_stripe_shard_length(0, k);
        return vec![StripeLayoutInfo {
            stripe_index: 0,
            payload_offset: 0,
            payload_length: 0,
            shard_length,
        }];
    }
    let mut layouts = Vec::new();
    let mut offset = 0u64;
    let mut idx = 0usize;
    while offset < object_size {
        let rem = object_size - offset;
        let plen = (rem.min(stripe_size as u64)) as usize;
        let slen = compute_stripe_shard_length(plen, k);
        layouts.push(StripeLayoutInfo {
            stripe_index: idx,
            payload_offset: offset,
            payload_length: plen,
            shard_length: slen,
        });
        offset += plen as u64;
        idx += 1;
    }
    layouts
}

/// Thread-safe Distributed Coordinator managing node membership, Ed25519 signed object manifests,
/// distributed PUT/GET transactions, per-object concurrency locks, and crash recovery.
pub struct DistributedCoordinator {
    coord_root: PathBuf,
    rpc_client: NodeRpcClient,
    signer_keypair: ManifestSignerKeypair,
    signer_policy: Arc<RwLock<TrustedSignerPolicy>>,
    registered_nodes: RwLock<BTreeMap<NodeId, RegisteredStorageNode>>,
    object_locks: Mutex<BTreeMap<String, Arc<RwLock<()>>>>,
}

impl DistributedCoordinator {
    /// Initializes or opens a coordinator metadata directory (`<coord_root>`), verifies
    /// `.fsp-coordinator-marker`, loads and verifies existing signed manifests, and runs
    /// coordinator transaction crash recovery.
    pub fn init_or_open(
        coord_root: &Path,
        rpc_client: NodeRpcClient,
        signer_keypair: ManifestSignerKeypair,
        signer_policy: TrustedSignerPolicy,
        nodes: Vec<RegisteredStorageNode>,
    ) -> Result<(Arc<Self>, CoordinatorCrashRecoveryReport), StorageError> {
        check_path_has_no_traversal(coord_root)?;
        if coord_root.exists() {
            ensure_not_symlink(coord_root)?;
        } else {
            fs::create_dir_all(coord_root)?;
        }

        let marker = coord_root.join(FSP_COORDINATOR_MARKER_FILE);
        if marker.exists() {
            ensure_not_symlink(&marker)?;
            let txt = fs::read_to_string(&marker)?;
            if !txt.starts_with(FSP_COORDINATOR_MARKER_MAGIC) {
                return Err(StorageError::UninitializedStorageRoot(format!(
                    "invalid coordinator marker in {}",
                    marker.display()
                )));
            }
        } else {
            write_and_sync_file(
                &marker,
                format!("{}\n", FSP_COORDINATOR_MARKER_MAGIC).as_bytes(),
            )?;
        }

        let manifests_dir = coord_root.join("manifests");
        let journal_dir = coord_root.join("journal");
        let staging_dir = coord_root.join("staging");
        fs::create_dir_all(&manifests_dir)?;
        fs::create_dir_all(&journal_dir)?;
        fs::create_dir_all(&staging_dir)?;
        ensure_not_symlink(&manifests_dir)?;
        ensure_not_symlink(&journal_dir)?;
        ensure_not_symlink(&staging_dir)?;

        let mut node_map = BTreeMap::new();
        for n in nodes {
            node_map.insert(n.node_id, n);
        }

        let coord = Arc::new(Self {
            coord_root: coord_root.to_path_buf(),
            rpc_client,
            signer_keypair,
            signer_policy: Arc::new(RwLock::new(signer_policy)),
            registered_nodes: RwLock::new(node_map),
            object_locks: Mutex::new(BTreeMap::new()),
        });

        let report = coord.recover_coordinator_transactions()?;
        Ok((coord, report))
    }

    /// Returns the coordinator metadata root directory.
    pub fn coord_root(&self) -> &Path {
        &self.coord_root
    }

    /// Returns a reference to the [`NodeRpcClient`].
    pub const fn rpc_client(&self) -> &NodeRpcClient {
        &self.rpc_client
    }

    /// Returns a reference to the coordinator's [`ManifestSignerKeypair`].
    pub const fn signer_keypair(&self) -> &ManifestSignerKeypair {
        &self.signer_keypair
    }

    /// Returns a shared handle to the coordinator's [`TrustedSignerPolicy`].
    pub fn signer_policy(&self) -> &Arc<RwLock<TrustedSignerPolicy>> {
        &self.signer_policy
    }

    /// Updates or registers a storage node endpoint.
    pub fn register_or_update_node(&self, node: RegisteredStorageNode) {
        self.registered_nodes
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(node.node_id, node);
    }

    /// Updates the listen address for an existing registered node.
    pub fn update_node_address(&self, node_id: NodeId, new_addr: &str) {
        let mut map = self
            .registered_nodes
            .write()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = map.get_mut(&node_id) {
            entry.listen_addr = new_addr.to_string();
        }
    }

    /// Returns a snapshot of all registered storage nodes.
    pub fn registered_nodes(&self) -> BTreeMap<NodeId, RegisteredStorageNode> {
        self.registered_nodes
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Returns a shared per-object `RwLock<()>` used to serialize concurrent PUT, repair, and migration
    /// operations on the same `object_id`.
    pub fn object_lock(&self, object_id: &str) -> Result<Arc<RwLock<()>>, StorageError> {
        if !validate_safe_identifier(object_id) {
            return Err(StorageError::Manifest(
                crate::storage::manifest::ManifestValidationError::InvalidObjectId(
                    object_id.to_string(),
                ),
            ));
        }
        Ok(self.object_rwlock(object_id))
    }

    fn object_rwlock(&self, object_id: &str) -> Arc<RwLock<()>> {
        let mut map = self.object_locks.lock().unwrap_or_else(|p| p.into_inner());
        map.entry(object_id.to_string())
            .or_insert_with(|| Arc::new(RwLock::new(())))
            .clone()
    }

    /// Polls all registered storage nodes concurrently via `Heartbeat` to build live
    /// [`CandidateNodeInfo`] descriptors for placement or repair planning.
    pub fn probe_candidate_nodes(&self) -> Vec<CandidateNodeInfo> {
        let nodes: Vec<RegisteredStorageNode> = self
            .registered_nodes
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect();

        let results: Mutex<Vec<CandidateNodeInfo>> = Mutex::new(Vec::with_capacity(nodes.len()));
        thread::scope(|s| {
            for node in &nodes {
                let client = &self.rpc_client;
                let res_ref = &results;
                s.spawn(move || {
                    let info =
                        match client.heartbeat(node.node_id, &node.listen_addr, 1, 1_700_000_000) {
                            Ok(hb) => {
                                let online =
                                    hb.health_state == "Healthy" || hb.health_state == "Degraded";
                                let avail = hb.capacity_limit_bytes.saturating_sub(hb.used_bytes);
                                CandidateNodeInfo {
                                    node_id: node.node_id,
                                    listen_addr: node.listen_addr.clone(),
                                    topology: node.topology.clone(),
                                    online,
                                    available_bytes: avail,
                                }
                            }
                            Err(_) => CandidateNodeInfo {
                                node_id: node.node_id,
                                listen_addr: node.listen_addr.clone(),
                                topology: node.topology.clone(),
                                online: false,
                                available_bytes: 0,
                            },
                        };
                    res_ref.lock().unwrap_or_else(|p| p.into_inner()).push(info);
                });
            }
        });

        let mut vec = results.into_inner().unwrap_or_else(|p| p.into_inner());
        vec.sort_by_key(|c| c.node_id);
        vec
    }

    /// Runs startup crash recovery on the coordinator metadata directory:
    /// 1. Cleans up any `.tmp` manifest staging files.
    /// 2. Scans `<coord_root>/journal/*.coord_tx` for uncommitted (`STATE=PREPARING`) transactions,
    ///    marks them `ABORTED`, and issues `DeleteShard` cleanup RPCs to remove orphan remote shards.
    /// 3. Loads and verifies all committed `<coord_root>/manifests/*.sig.json` signed manifests
    ///    against [`TrustedSignerPolicy`].
    pub fn recover_coordinator_transactions(
        &self,
    ) -> Result<CoordinatorCrashRecoveryReport, StorageError> {
        let mut uncommitted_txs_aborted = 0usize;
        let mut remote_orphan_shards_deleted = 0usize;
        let mut verified_signed_manifests = 0usize;

        let staging_dir = self.coord_root.join("staging");
        if staging_dir.exists() {
            ensure_not_symlink(&staging_dir)?;
            for entry in fs::read_dir(&staging_dir)? {
                let p = entry?.path();
                ensure_not_symlink(&p)?;
                let _ = fs::remove_file(&p);
            }
        }

        let journal_dir = self.coord_root.join("journal");
        if journal_dir.exists() {
            ensure_not_symlink(&journal_dir)?;
            for entry in fs::read_dir(&journal_dir)? {
                let p = entry?.path();
                ensure_not_symlink(&p)?;
                let Some(fname) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if fname.ends_with(".tmp") {
                    let _ = fs::remove_file(&p);
                    continue;
                }
                if !fname.ends_with(".coord_tx") {
                    continue;
                }
                let content = fs::read_to_string(&p)?;
                if content.contains("STATE=PREPARING") {
                    uncommitted_txs_aborted += 1;
                    remote_orphan_shards_deleted += self.cleanup_preparing_journal_entry(&content);
                    let aborted = content.replace("STATE=PREPARING", "STATE=ABORTED");
                    write_and_sync_file(&p, aborted.as_bytes())?;
                }
            }
        }

        let manifests_dir = self.coord_root.join("manifests");
        if manifests_dir.exists() {
            ensure_not_symlink(&manifests_dir)?;
            for entry in fs::read_dir(&manifests_dir)? {
                let p = entry?.path();
                ensure_not_symlink(&p)?;
                let Some(fname) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if fname.ends_with(".tmp") {
                    let _ = fs::remove_file(&p);
                    continue;
                }
                if fname.ends_with(".sig.json") {
                    let raw = fs::read_to_string(&p)?;
                    let signed = SignedObjectManifest::from_json(&raw)
                        .map_err(|e| StorageError::Signature(e.to_string()))?;
                    self.signer_policy
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .verify_and_advance_generation(&signed, false)
                        .map_err(|e| StorageError::Signature(e.to_string()))?;
                    verified_signed_manifests += 1;
                }
            }
        }

        Ok(CoordinatorCrashRecoveryReport {
            uncommitted_txs_aborted,
            remote_orphan_shards_deleted,
            verified_signed_manifests,
        })
    }

    fn cleanup_preparing_journal_entry(&self, content: &str) -> usize {
        let mut tx_id = String::new();
        let mut object_id = String::new();
        let mut version = 1u64;
        let mut targets: Vec<(usize, NodeId, String)> = Vec::new();

        for line in content.lines() {
            if let Some((k, v)) = line.split_once('=') {
                match k {
                    "tx_id" => tx_id = v.to_string(),
                    "object_id" => object_id = v.to_string(),
                    "version" => version = v.parse().unwrap_or(1),
                    "TARGET" => {
                        let sp: Vec<&str> = v.split('|').collect();
                        if sp.len() == 3 {
                            if let (Ok(s_id), Ok(n_id)) = (sp[0].parse(), sp[1].parse()) {
                                targets.push((s_id, NodeId(n_id), sp[2].to_string()));
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        let mut deleted_count = 0usize;
        for (shard_id, node_id, addr) in targets {
            if let Ok(true) = self
                .rpc_client
                .delete_shard(node_id, &addr, &tx_id, &object_id, shard_id, version)
            {
                deleted_count += 1;
            }
        }
        deleted_count
    }

    /// Executes the full 12-step Distributed `PUT` transaction protocol for an in-memory byte slice.
    pub fn distributed_put_bytes(
        &self,
        object_id: &str,
        data: &[u8],
        options: DistributedPutOptions,
    ) -> Result<DistributedPutReport, StorageError> {
        let mut cursor = std::io::Cursor::new(data);
        self.distributed_put_reader(object_id, &mut cursor, data.len() as u64, options)
    }

    /// Executes the full 12-step Distributed `PUT` transaction protocol for a streaming reader.
    pub fn distributed_put_reader<R: Read>(
        &self,
        object_id: &str,
        reader: &mut R,
        object_size: u64,
        options: DistributedPutOptions,
    ) -> Result<DistributedPutReport, StorageError> {
        if !validate_safe_identifier(object_id) {
            return Err(StorageError::Manifest(
                crate::storage::manifest::ManifestValidationError::InvalidObjectId(
                    object_id.to_string(),
                ),
            ));
        }

        let obj_lock = self.object_rwlock(object_id);
        let _write_guard = obj_lock.write().unwrap_or_else(|p| p.into_inner());

        let codec = ErasureCodec::from_scheme(options.coding_scheme)?;
        let n = codec.total_shards();
        let k = codec.data_shards();
        let stripe_layouts = plan_stripe_layouts(object_size, options.stripe_size, k);

        // Determine per-shard container size requirement
        let total_shard_payload_bytes: u64 =
            stripe_layouts.iter().map(|s| s.shard_length as u64).sum();
        let required_shard_bytes =
            256u64 + total_shard_payload_bytes + (stripe_layouts.len() as u64) * 52;

        // Probe candidate nodes and plan failure-domain-aware placement
        let candidates = self.probe_candidate_nodes();
        let planner = DistributedPlacementPlanner::new(options.placement_policy);
        let placement_plan = planner
            .plan_object_placement(&candidates, n, required_shard_bytes)
            .map_err(|e| StorageError::Network(e.to_string()))?;

        // Read stripes, compute full object SHA-256, and encode per-shard stripe buffers
        let mut object_hasher = Sha256Hasher::new();
        let mut per_shard_payloads: Vec<Vec<u8>> = (0..n)
            .map(|_| Vec::with_capacity(total_shard_payload_bytes as usize))
            .collect();
        let mut per_shard_stripes_meta: Vec<Vec<(usize, usize, String)>> = (0..n)
            .map(|_| Vec::with_capacity(stripe_layouts.len()))
            .collect();
        let mut stripe_manifests: Vec<StripeManifest> = Vec::with_capacity(stripe_layouts.len());
        let shard_version = 1u64;

        for layout in &stripe_layouts {
            let mut stripe_buf = vec![0u8; layout.payload_length];
            if layout.payload_length > 0 {
                reader.read_exact(&mut stripe_buf)?;
                object_hasher.update(&stripe_buf);
            }

            let (_, coded_shards) = codec.encode_stripe_payload(&stripe_buf)?;
            let mut placements = Vec::with_capacity(n);

            for (shard_id, shard_bytes) in coded_shards.iter().enumerate() {
                let sha_hex = sha256_bytes(shard_bytes).to_hex();
                per_shard_payloads[shard_id].extend_from_slice(shard_bytes);
                per_shard_stripes_meta[shard_id].push((
                    layout.payload_length,
                    layout.shard_length,
                    sha_hex.clone(),
                ));

                let assigned = &placement_plan.shard_assignments[&shard_id];
                let rel_path = format!(
                    "node-{:02}/objects/{}/shard-{:02}.v{}.fspc",
                    assigned.node_id.0, object_id, shard_id, shard_version
                );
                placements.push(ShardPlacementRecord {
                    shard_id,
                    node_id: assigned.node_id.0,
                    domain_id: assigned.node_id.0 % 4,
                    version: shard_version,
                    size_bytes: layout.shard_length as u64,
                    sha256_digest: sha_hex,
                    relative_path: rel_path,
                });
            }

            stripe_manifests.push(StripeManifest {
                stripe_index: layout.stripe_index,
                payload_offset: layout.payload_offset,
                payload_length: layout.payload_length,
                shard_length: layout.shard_length,
                placements,
            });
        }

        let object_digest = object_hasher.finalize().to_hex();
        let tx_id = options.tx_id_override.unwrap_or_else(|| {
            let seq = COORD_TX_COUNTER.fetch_add(1, Ordering::Relaxed);
            format!("ctx-{}-{:06}", object_id, seq)
        });

        // Step 6: Persist coordinator transaction journal entry in PREPARING state before sending any network RPCs
        let journal_path = self
            .coord_root
            .join("journal")
            .join(format!("{}.coord_tx", tx_id));
        let mut journal_body = format!(
            "STATE=PREPARING\ntx_id={}\nobject_id={}\nversion={}\n",
            tx_id, object_id, shard_version
        );
        for (shard_id, node_info) in &placement_plan.shard_assignments {
            journal_body.push_str(&format!(
                "TARGET={}|{}|{}\n",
                shard_id, node_info.node_id.0, node_info.listen_addr
            ));
        }
        write_and_sync_file(&journal_path, journal_body.as_bytes())?;

        // Step 7 & 8: Dispatch PutShard RPCs concurrently across all target storage nodes
        let acks_mutex: Mutex<BTreeMap<usize, Result<PutShardAck, String>>> =
            Mutex::new(BTreeMap::new());
        thread::scope(|s| {
            for shard_id in 0..n {
                let target = &placement_plan.shard_assignments[&shard_id];
                let payload_ref = &per_shard_payloads[shard_id];
                let stripes_meta = per_shard_stripes_meta[shard_id].clone();
                let client = &self.rpc_client;
                let tx_id_ref = &tx_id;
                let acks_ref = &acks_mutex;
                let cs = options.coding_scheme;

                s.spawn(move || {
                    let req = PutShardRequest {
                        tx_id: tx_id_ref.clone(),
                        object_id: object_id.to_string(),
                        shard_id,
                        version: shard_version,
                        coding_scheme: cs,
                        stripes: stripes_meta,
                    };
                    let res = client
                        .put_shard(target.node_id, &target.listen_addr, req, payload_ref)
                        .map_err(|e| e.to_string());
                    acks_ref
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(shard_id, res);
                });
            }
        });

        let acks_map = acks_mutex.into_inner().unwrap_or_else(|p| p.into_inner());
        let mut acknowledged_nodes = BTreeSet::new();
        let mut acknowledged_shards = BTreeSet::new();
        let mut degraded_unacknowledged_shards = Vec::new();
        let mut total_network_bytes_sent = 0u64;
        let mut idempotent_replay_count = 0usize;
        let mut first_error: Option<String> = None;

        for shard_id in 0..n {
            let target = &placement_plan.shard_assignments[&shard_id];
            match acks_map.get(&shard_id) {
                Some(Ok(ack)) => {
                    // P0 Security / Durability Contract Verification
                    if ack.node_id != target.node_id {
                        degraded_unacknowledged_shards.push(shard_id);
                        if first_error.is_none() {
                            first_error = Some(format!(
                                "Node identity mismatch for shard {}: expected {:?}, got {:?}",
                                shard_id, target.node_id, ack.node_id
                            ));
                        }
                        continue;
                    }
                    if ack.shard_id != shard_id {
                        degraded_unacknowledged_shards.push(shard_id);
                        if first_error.is_none() {
                            first_error = Some(format!(
                                "Shard index mismatch: expected {}, got {}",
                                shard_id, ack.shard_id
                            ));
                        }
                        continue;
                    }
                    if ack.object_id != object_id {
                        degraded_unacknowledged_shards.push(shard_id);
                        if first_error.is_none() {
                            first_error = Some(format!(
                                "Object ID mismatch for shard {}: expected {}, got {}",
                                shard_id, object_id, ack.object_id
                            ));
                        }
                        continue;
                    }
                    if ack.version != shard_version {
                        degraded_unacknowledged_shards.push(shard_id);
                        if first_error.is_none() {
                            first_error = Some(format!(
                                "Version mismatch for shard {}: expected {}, got {}",
                                shard_id, shard_version, ack.version
                            ));
                        }
                        continue;
                    }
                    if ack.durability_level < DurabilityLevel::LocallyDurable {
                        degraded_unacknowledged_shards.push(shard_id);
                        if first_error.is_none() {
                            first_error = Some(format!(
                                "Insufficient durability for shard {}: expected >= LocallyDurable, got {:?}",
                                shard_id, ack.durability_level
                            ));
                        }
                        continue;
                    }

                    acknowledged_shards.insert(shard_id);
                    acknowledged_nodes.insert(ack.node_id);
                    total_network_bytes_sent += ack.container_size_bytes;
                    if ack.idempotent_replay {
                        idempotent_replay_count += 1;
                    }
                }
                Some(Err(err_msg)) => {
                    degraded_unacknowledged_shards.push(shard_id);
                    if first_error.is_none() {
                        first_error = Some(err_msg.clone());
                    }
                }
                None => {
                    degraded_unacknowledged_shards.push(shard_id);
                }
            }
        }

        // Step 9: Check commit policy (StrictAllShards vs AllowDegradedCommit)
        let required_acks = match options.commit_policy {
            DegradedCommitPolicy::StrictAllShards => n,
            DegradedCommitPolicy::AllowDegradedCommit {
                min_acknowledged_shards,
            } => min_acknowledged_shards.max(k),
        };

        if acknowledged_shards.len() < required_acks {
            // Step 12: Abort transaction and clean up any remote shards already written!
            let _ = self.cleanup_preparing_journal_entry(&journal_body);
            let aborted = journal_body.replace("STATE=PREPARING", "STATE=ABORTED");
            let _ = write_and_sync_file(&journal_path, aborted.as_bytes());
            return Err(StorageError::Network(format!(
                "Distributed PUT transaction {} aborted: acknowledged {}/{} required distinct shards (first error: {:?})",
                tx_id,
                acknowledged_shards.len(),
                required_acks,
                first_error
            )));
        }

        // Step 10: Construct ObjectManifest, sign with Ed25519 (monotonic generation), and self-verify
        let mut manifest = ObjectManifest {
            format_version: MANIFEST_FORMAT_VERSION,
            object_id: object_id.to_string(),
            object_size,
            stripe_size: options.stripe_size,
            coding: ManifestCodingSpec::from_coding_scheme(options.coding_scheme),
            integrity_algorithm: "sha256".to_string(),
            object_digest,
            stripes: stripe_manifests,
            manifest_digest: String::new(),
        };
        manifest.manifest_digest = manifest.compute_canonical_digest();
        manifest.validate()?;

        let next_gen = {
            let pol = self.signer_policy.read().unwrap_or_else(|p| p.into_inner());
            pol.recorded_generation(object_id).unwrap_or(0) + 1
        };
        let signed_manifest = self
            .signer_keypair
            .sign_manifest(manifest, next_gen)
            .map_err(|e| StorageError::Signature(e.to_string()))?;

        // Verify signature and register monotonic generation in TrustedSignerPolicy
        self.signer_policy
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .verify_and_advance_generation(&signed_manifest, true)
            .map_err(|e| StorageError::Signature(e.to_string()))?;

        // Step 11: Atomically commit signed manifest to `<coord_root>/manifests/<object_id>.sig.json`
        trigger_crash_point(CrashPoint::AfterWalSyncBeforeQuorumCommit)?;
        self.save_signed_manifest_atomic(&signed_manifest)?;

        let committed_journal = journal_body.replace("STATE=PREPARING", "STATE=COMMITTED");
        write_and_sync_file(&journal_path, committed_journal.as_bytes())?;

        trigger_crash_point(CrashPoint::AfterManifestCommitBeforeClientAck)?;

        let distinct_durable_shards = acknowledged_shards.len();
        let is_recoverable = distinct_durable_shards >= k;
        let full_redundancy_achieved = distinct_durable_shards == n;

        let durability_level = if is_recoverable {
            DurabilityLevel::Recoverable
        } else if !acknowledged_shards.is_empty() {
            DurabilityLevel::QuorumCommitted
        } else {
            DurabilityLevel::Buffered
        };

        let failure_domain_policy_satisfied = placement_plan.distribution_report.distinct_nodes
            == placement_plan.distribution_report.total_shards_placed;

        let durability_contract = DurabilityContractReport {
            durability_level,
            is_recoverable,
            distinct_durable_shards,
            total_shards: n,
            min_shards: k,
            full_redundancy_achieved,
            failure_domain_policy_satisfied,
            metadata_quorum_committed: true,
        };

        let report = DistributedPutReport {
            object_id: object_id.to_string(),
            tx_id,
            signed_manifest,
            placement_report: placement_plan.distribution_report,
            acknowledged_nodes,
            degraded_unacknowledged_shards,
            total_network_bytes_sent,
            idempotent_replay_count,
            durability_level,
            durability_contract,
        };

        trigger_crash_point(CrashPoint::ImmediatelyAfterClientAck)?;

        Ok(report)
    }

    /// Atomically persists an updated [`SignedObjectManifest`] (e.g. after PUT or distributed repair).
    pub fn save_signed_manifest_atomic(
        &self,
        signed: &SignedObjectManifest,
    ) -> Result<PathBuf, StorageError> {
        let manifests_dir = self.coord_root.join("manifests");
        ensure_not_symlink(&manifests_dir)?;
        let tmp_path = manifests_dir.join(format!(
            "{}.gen{}.sig.json.tmp",
            signed.manifest.object_id, signed.manifest_generation
        ));
        let final_path = manifests_dir.join(format!("{}.sig.json", signed.manifest.object_id));

        let json_bytes = signed.to_json().into_bytes();
        write_and_sync_file(&tmp_path, &json_bytes)?;
        fs::rename(&tmp_path, &final_path)?;
        sync_directory(&manifests_dir)?;
        Ok(final_path)
    }

    /// Loads and cryptographically verifies the committed [`SignedObjectManifest`] for `object_id`.
    pub fn load_verified_signed_manifest(
        &self,
        object_id: &str,
    ) -> Result<SignedObjectManifest, StorageError> {
        if !validate_safe_identifier(object_id) {
            return Err(StorageError::Manifest(
                crate::storage::manifest::ManifestValidationError::InvalidObjectId(
                    object_id.to_string(),
                ),
            ));
        }
        let path = self
            .coord_root
            .join("manifests")
            .join(format!("{}.sig.json", object_id));
        if !path.exists() {
            return Err(StorageError::ObjectNotFound(object_id.to_string()));
        }
        ensure_not_symlink(&path)?;
        let raw = fs::read_to_string(&path)?;
        let signed = SignedObjectManifest::from_json(&raw)
            .map_err(|e| StorageError::Signature(e.to_string()))?;
        self.signer_policy
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .verify_and_advance_generation(&signed, false)
            .map_err(|e| StorageError::Signature(e.to_string()))?;
        Ok(signed)
    }

    /// Lists all object IDs with committed manifests in `<coord_root>/manifests/`.
    pub fn list_committed_objects(&self) -> Result<Vec<String>, StorageError> {
        let manifests_dir = self.coord_root.join("manifests");
        let mut ids = Vec::new();
        if manifests_dir.exists() {
            ensure_not_symlink(&manifests_dir)?;
            for entry in fs::read_dir(&manifests_dir)? {
                let p = entry?.path();
                if let Some(fname) = p.file_name().and_then(|n| n.to_str()) {
                    if let Some(obj_id) = fname.strip_suffix(".sig.json") {
                        ids.push(obj_id.to_string());
                    }
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Reconstructs an object across the distributed storage nodes into an in-memory `Vec<u8>`,
    /// verifying the Ed25519 manifest signature, per-shard SHA-256 digests, and full object SHA-256.
    pub fn distributed_get_to_bytes(
        &self,
        object_id: &str,
    ) -> Result<(Vec<u8>, DistributedGetReport), StorageError> {
        let signed = self.load_verified_signed_manifest(object_id)?;
        let mut out = Vec::with_capacity(signed.manifest.object_size as usize);
        let report = self.reconstruct_into_writer(&signed, &mut out)?;
        Ok((out, report))
    }

    /// Reconstructs an object across the distributed storage nodes into `output_path` using
    /// bounded stripe-by-stripe streaming and atomic file publication.
    pub fn distributed_get_to_file(
        &self,
        object_id: &str,
        output_path: &Path,
    ) -> Result<DistributedGetReport, StorageError> {
        check_path_has_no_traversal(output_path)?;
        let signed = self.load_verified_signed_manifest(object_id)?;

        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() && !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let tmp_name = format!(
            "{}.fsp-dist-get.tmp",
            output_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("output")
        );
        let tmp_path = output_path.with_file_name(tmp_name);

        let report_res = (|| -> Result<DistributedGetReport, StorageError> {
            let file = create_file_nofollow(&tmp_path)?;
            let mut writer = BufWriter::with_capacity(256 * 1024, file);
            let rep = self.reconstruct_into_writer(&signed, &mut writer)?;
            writer.flush()?;
            let inner = writer.into_inner().map_err(|e| e.into_error())?;
            inner.sync_all()?;
            Ok(rep)
        })();

        match report_res {
            Ok(rep) => {
                fs::rename(&tmp_path, output_path)?;
                if let Some(parent) = output_path.parent() {
                    if !parent.as_os_str().is_empty() {
                        sync_directory(parent)?;
                    }
                }
                Ok(rep)
            }
            Err(err) => {
                let _ = fs::remove_file(&tmp_path);
                Err(err)
            }
        }
    }

    fn reconstruct_into_writer<W: Write>(
        &self,
        signed: &SignedObjectManifest,
        writer: &mut W,
    ) -> Result<DistributedGetReport, StorageError> {
        let obj_lock = self.object_rwlock(&signed.manifest.object_id);
        let _read_guard = obj_lock.read().unwrap_or_else(|p| p.into_inner());

        let manifest = &signed.manifest;
        let coding_scheme = manifest.coding.to_coding_scheme()?;
        let codec = ErasureCodec::from_scheme(coding_scheme)?;
        let k = codec.data_shards();

        let nodes_map = self.registered_nodes();
        let mut nodes_queried = BTreeSet::new();
        let mut nodes_failed_or_corrupt = BTreeSet::new();
        let mut object_hasher = Sha256Hasher::new();

        for stripe in &manifest.stripes {
            let mut available_shards: Vec<(ShardId, Vec<u8>)> = Vec::with_capacity(k);
            let mut surviving_ids: BTreeSet<ShardId> = BTreeSet::new();

            // First attempt systematic fast-path (data shards 0..k), then fallback to parity shards k..n
            // as needed until `codec.can_decode(&surviving_ids)` is satisfied!
            for placement in &stripe.placements {
                if surviving_ids.len() >= k && codec.can_decode(&surviving_ids) {
                    break;
                }
                let node_id = NodeId(placement.node_id);
                if nodes_failed_or_corrupt.contains(&node_id) {
                    continue;
                }
                let Some(reg_node) = nodes_map.get(&node_id) else {
                    nodes_failed_or_corrupt.insert(node_id);
                    continue;
                };

                nodes_queried.insert(node_id);
                let req = GetShardRequest {
                    object_id: manifest.object_id.clone(),
                    shard_id: placement.shard_id,
                    version: placement.version,
                    stripe_index: Some(stripe.stripe_index),
                };

                match self
                    .rpc_client
                    .get_shard(node_id, &reg_node.listen_addr, req)
                {
                    Ok((_, shard_bytes)) => {
                        if shard_bytes.len() != stripe.shard_length {
                            nodes_failed_or_corrupt.insert(node_id);
                            continue;
                        }
                        let actual_sha = sha256_bytes(&shard_bytes).to_hex();
                        if actual_sha != placement.sha256_digest {
                            nodes_failed_or_corrupt.insert(node_id);
                            continue;
                        }
                        surviving_ids.insert(ShardId(placement.shard_id));
                        available_shards.push((ShardId(placement.shard_id), shard_bytes));
                    }
                    Err(_) => {
                        nodes_failed_or_corrupt.insert(node_id);
                    }
                }
            }

            // If we still don't have enough shards, try any untried placements in this stripe
            if !codec.can_decode(&surviving_ids) {
                for placement in &stripe.placements {
                    if surviving_ids.contains(&ShardId(placement.shard_id)) {
                        continue;
                    }
                    let node_id = NodeId(placement.node_id);
                    let Some(reg_node) = nodes_map.get(&node_id) else {
                        continue;
                    };
                    nodes_queried.insert(node_id);
                    let req = GetShardRequest {
                        object_id: manifest.object_id.clone(),
                        shard_id: placement.shard_id,
                        version: placement.version,
                        stripe_index: Some(stripe.stripe_index),
                    };
                    if let Ok((_, shard_bytes)) =
                        self.rpc_client
                            .get_shard(node_id, &reg_node.listen_addr, req)
                    {
                        if shard_bytes.len() == stripe.shard_length
                            && sha256_bytes(&shard_bytes).to_hex() == placement.sha256_digest
                        {
                            surviving_ids.insert(ShardId(placement.shard_id));
                            available_shards.push((ShardId(placement.shard_id), shard_bytes));
                            if codec.can_decode(&surviving_ids) {
                                break;
                            }
                        } else {
                            nodes_failed_or_corrupt.insert(node_id);
                        }
                    } else {
                        nodes_failed_or_corrupt.insert(node_id);
                    }
                }
            }

            if !codec.can_decode(&surviving_ids) {
                return Err(StorageError::InsufficientValidShards {
                    object_id: manifest.object_id.clone(),
                    stripe_index: stripe.stripe_index,
                    valid_shards: surviving_ids.len(),
                    required_shards: k,
                });
            }

            let avail_refs: Vec<(ShardId, &[u8])> = available_shards
                .iter()
                .map(|(id, v)| (*id, v.as_slice()))
                .collect();
            let reconstructed_stripe = codec.decode_stripe_payload(
                &avail_refs,
                stripe.shard_length,
                stripe.payload_length,
            )?;
            object_hasher.update(&reconstructed_stripe);
            writer.write_all(&reconstructed_stripe)?;
        }

        let final_digest = object_hasher.finalize();
        if final_digest.to_hex() != manifest.object_digest {
            return Err(StorageError::ObjectIntegrityFailure {
                object_id: manifest.object_id.clone(),
                expected_digest: manifest.object_digest.clone(),
                actual_digest: final_digest.to_hex(),
            });
        }

        Ok(DistributedGetReport {
            object_id: manifest.object_id.clone(),
            object_size: manifest.object_size,
            object_sha256: final_digest,
            manifest_generation: signed.manifest_generation,
            nodes_queried,
            nodes_failed_or_corrupt,
            stripes_reconstructed: manifest.stripes.len(),
        })
    }

    /// Exports all committed signed manifests into an Ed25519-signed [`MetadataBackupBundle`].
    pub fn export_metadata_backup(&self) -> Result<MetadataBackupBundle, StorageError> {
        let obj_ids = self.list_committed_objects()?;
        let mut manifests = Vec::with_capacity(obj_ids.len());
        for id in obj_ids {
            manifests.push(self.load_verified_signed_manifest(&id)?);
        }
        let bundle = MetadataBackupBundle::create(&self.signer_keypair, manifests)
            .map_err(|e| StorageError::Signature(e.to_string()))?;
        Ok(bundle)
    }

    /// Restores committed signed manifests from a verified [`MetadataBackupBundle`].
    pub fn restore_metadata_backup(
        &self,
        bundle: &MetadataBackupBundle,
    ) -> Result<usize, StorageError> {
        let restored = {
            let mut pol = self
                .signer_policy
                .write()
                .unwrap_or_else(|p| p.into_inner());
            bundle
                .verify_and_restore(&mut pol)
                .map_err(|e| StorageError::Signature(e.to_string()))?
        };

        for signed in &restored {
            self.save_signed_manifest_atomic(signed)?;
        }
        Ok(restored.len())
    }
}
