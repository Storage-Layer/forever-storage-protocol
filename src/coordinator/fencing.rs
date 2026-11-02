//! Distributed Fencing Tokens and Storage Node Epoch Enforcement (`src/coordinator/fencing.rs`).
//!
//! Prevents stale leaders from modifying storage node state under split-brain or network partitions.
//! Storage nodes reject mutating requests (`PutShard`, `DeleteShard`, etc.) whose epoch is lower
//! than the node's durably recorded high-water mark epoch.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::coordinator::incarnation::{ClusterAuthorityRecord, ClusterIncarnationId};
use crate::coordinator::leadership::{CoordinatorId, LeadershipCertificate};
use crate::model::node::NodeId;
use crate::network::protocol::RpcError;
use crate::security::signatures::{decode_hex_exact, encode_hex};
use crate::storage::backend::StorageError;
use crate::storage::filesystem::{sync_directory, write_and_sync_file};

/// Fencing token issued by an authoritative coordinator holding a committed quorum epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FencingToken {
    pub epoch: u64,
    pub leader_id: CoordinatorId,
    pub timestamp_unix_secs: u64,
}

impl FencingToken {
    pub fn new(epoch: u64, leader_id: CoordinatorId, timestamp_unix_secs: u64) -> Self {
        Self {
            epoch,
            leader_id,
            timestamp_unix_secs,
        }
    }

    /// Serializes token into wire format `TOKEN:epoch:leader_id:timestamp`.
    pub fn to_wire_string(&self) -> String {
        format!(
            "TOKEN:{}:{}:{}",
            self.epoch,
            self.leader_id.as_str(),
            self.timestamp_unix_secs
        )
    }

    /// Parses wire string.
    pub fn from_wire_string(s: &str) -> Result<Self, StorageError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 4 || parts[0] != "TOKEN" {
            return Err(StorageError::FencingViolation {
                current_epoch: 0,
                attempted_epoch: 0,
            });
        }
        let epoch = parts[1]
            .parse::<u64>()
            .map_err(|_| StorageError::FencingViolation {
                current_epoch: 0,
                attempted_epoch: 0,
            })?;
        let leader_id = CoordinatorId::new(parts[2]);
        let ts = parts[3]
            .parse::<u64>()
            .map_err(|_| StorageError::FencingViolation {
                current_epoch: 0,
                attempted_epoch: 0,
            })?;
        Ok(Self::new(epoch, leader_id, ts))
    }
}

/// Cryptographically authenticated fencing token issued by a certified leader.
/// Bound to the specific operation, object, shard, payload hash, and epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FencedOperationToken {
    pub epoch: u64,
    pub leader_id: CoordinatorId,
    pub request_id: String,
    pub object_id: String,
    pub shard_id: u32,
    pub version: u64,
    pub payload_sha256_hex: String,
    pub timestamp_unix_secs: u64,
    pub signature_hex: String,
    pub cluster_incarnation: Option<ClusterIncarnationId>,
}

impl FencedOperationToken {
    pub const DOMAIN_TAG: &'static str = "FSP-FENCED-OP-V1";

    pub fn canonical_message(
        epoch: u64,
        leader_id: &CoordinatorId,
        request_id: &str,
        object_id: &str,
        shard_id: u32,
        version: u64,
        payload_sha256_hex: &str,
    ) -> Vec<u8> {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}",
            Self::DOMAIN_TAG,
            epoch,
            leader_id.as_str(),
            request_id,
            object_id,
            shard_id,
            version,
            payload_sha256_hex
        )
        .into_bytes()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        epoch: u64,
        leader_id: CoordinatorId,
        request_id: impl Into<String>,
        object_id: impl Into<String>,
        shard_id: u32,
        version: u64,
        payload_sha256_hex: impl Into<String>,
        timestamp_unix_secs: u64,
        signing_key: &SigningKey,
    ) -> Self {
        let req_id = request_id.into();
        let obj_id = object_id.into();
        let hash_hex = payload_sha256_hex.into();
        let msg = Self::canonical_message(
            epoch, &leader_id, &req_id, &obj_id, shard_id, version, &hash_hex,
        );
        let sig = signing_key.sign(&msg);
        let signature_hex = encode_hex(&sig.to_bytes());
        Self {
            epoch,
            leader_id,
            request_id: req_id,
            object_id: obj_id,
            shard_id,
            version,
            payload_sha256_hex: hash_hex,
            timestamp_unix_secs,
            signature_hex,
            cluster_incarnation: None,
        }
    }

    pub fn with_incarnation(mut self, inc: ClusterIncarnationId) -> Self {
        self.cluster_incarnation = Some(inc);
        self
    }

    pub fn verify(&self, leader_key: &VerifyingKey) -> Result<(), RpcError> {
        let msg = Self::canonical_message(
            self.epoch,
            &self.leader_id,
            &self.request_id,
            &self.object_id,
            self.shard_id,
            self.version,
            &self.payload_sha256_hex,
        );
        let sig_bytes = decode_hex_exact::<64>(&self.signature_hex)
            .map_err(|e| RpcError::Unauthorized(format!("bad op token signature hex: {}", e)))?;
        let sig = Signature::from_bytes(&sig_bytes);
        leader_key
            .verify_strict(&msg, &sig)
            .map_err(|e| RpcError::Unauthorized(format!("invalid op token signature: {}", e)))
    }

    pub fn to_wire_string(&self) -> String {
        format!(
            "OPTOKEN:{}:{}:{}:{}:{}:{}:{}:{}:{}",
            self.epoch,
            self.leader_id.as_str(),
            self.request_id,
            self.object_id,
            self.shard_id,
            self.version,
            self.payload_sha256_hex,
            self.timestamp_unix_secs,
            self.signature_hex
        )
    }

    pub fn from_wire_string(s: &str) -> Result<Self, RpcError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 10 || parts[0] != "OPTOKEN" {
            return Err(RpcError::SerializationError(
                "invalid OPTOKEN format".into(),
            ));
        }
        let epoch = parts[1]
            .parse::<u64>()
            .map_err(|e| RpcError::SerializationError(e.to_string()))?;
        let leader_id = CoordinatorId::new(parts[2]);
        let request_id = parts[3].to_string();
        let object_id = parts[4].to_string();
        let shard_id = parts[5]
            .parse::<u32>()
            .map_err(|e| RpcError::SerializationError(e.to_string()))?;
        let version = parts[6]
            .parse::<u64>()
            .map_err(|e| RpcError::SerializationError(e.to_string()))?;
        let payload_sha256_hex = parts[7].to_string();
        let timestamp_unix_secs = parts[8]
            .parse::<u64>()
            .map_err(|e| RpcError::SerializationError(e.to_string()))?;
        let signature_hex = parts[9].to_string();
        Ok(Self {
            epoch,
            leader_id,
            request_id,
            object_id,
            shard_id,
            version,
            payload_sha256_hex,
            timestamp_unix_secs,
            signature_hex,
            cluster_incarnation: None,
        })
    }
}

/// Specific mutating operation authorized by a quorum permit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PermitOperationType {
    /// Stage a new shard chunk (immutable temporary write).
    StageShard,
    /// Commit an object manifest version (advances generation).
    CommitManifest,
    /// Delete an individual shard or tombstone.
    DeleteShard,
    /// Destructive garbage collection / reclamation of unreferenced shards.
    GarbageCollectShard,
}

/// Individual signature grant from a quorum voter authorizing a specific mutation permit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumPermitGrant {
    pub voter_id: CoordinatorId,
    pub epoch: u64,
    pub commit_index: u64,
    pub request_id: String,
    pub object_id: String,
    pub shard_id: u32,
    pub version: u64,
    pub payload_sha256_hex: String,
    pub signature_hex: String,
    pub target_node: Option<NodeId>,
    pub operation_type: Option<PermitOperationType>,
}

impl QuorumPermitGrant {
    pub const DOMAIN_TAG: &'static str = "FSP-PERMIT-GRANT-V1";
    pub const DOMAIN_TAG_V2: &'static str = "FSP-PERMIT-GRANT-V2";

    #[allow(clippy::too_many_arguments)]
    pub fn canonical_message(
        epoch: u64,
        commit_index: u64,
        voter_id: &CoordinatorId,
        request_id: &str,
        object_id: &str,
        shard_id: u32,
        version: u64,
        payload_sha256_hex: &str,
    ) -> Vec<u8> {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}",
            Self::DOMAIN_TAG,
            epoch,
            commit_index,
            voter_id.as_str(),
            request_id,
            object_id,
            shard_id,
            version,
            payload_sha256_hex
        )
        .into_bytes()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn canonical_message_v2(
        epoch: u64,
        commit_index: u64,
        voter_id: &CoordinatorId,
        request_id: &str,
        object_id: &str,
        shard_id: u32,
        version: u64,
        payload_sha256_hex: &str,
        target_node: NodeId,
        operation_type: PermitOperationType,
    ) -> Vec<u8> {
        format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{:?}",
            Self::DOMAIN_TAG_V2,
            epoch,
            commit_index,
            voter_id.as_str(),
            request_id,
            object_id,
            shard_id,
            version,
            payload_sha256_hex,
            target_node.0,
            operation_type,
        )
        .into_bytes()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        voter_id: CoordinatorId,
        epoch: u64,
        commit_index: u64,
        request_id: impl Into<String>,
        object_id: impl Into<String>,
        shard_id: u32,
        version: u64,
        payload_sha256_hex: impl Into<String>,
        signing_key: &SigningKey,
    ) -> Self {
        let req_id = request_id.into();
        let obj_id = object_id.into();
        let hash_hex = payload_sha256_hex.into();
        let msg = Self::canonical_message(
            epoch,
            commit_index,
            &voter_id,
            &req_id,
            &obj_id,
            shard_id,
            version,
            &hash_hex,
        );
        let sig = signing_key.sign(&msg);
        let signature_hex = encode_hex(&sig.to_bytes());
        Self {
            voter_id,
            epoch,
            commit_index,
            request_id: req_id,
            object_id: obj_id,
            shard_id,
            version,
            payload_sha256_hex: hash_hex,
            signature_hex,
            target_node: None,
            operation_type: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_v2(
        voter_id: CoordinatorId,
        epoch: u64,
        commit_index: u64,
        request_id: impl Into<String>,
        object_id: impl Into<String>,
        shard_id: u32,
        version: u64,
        payload_sha256_hex: impl Into<String>,
        target_node: NodeId,
        operation_type: PermitOperationType,
        signing_key: &SigningKey,
    ) -> Self {
        let req_id = request_id.into();
        let obj_id = object_id.into();
        let hash_hex = payload_sha256_hex.into();
        let msg = Self::canonical_message_v2(
            epoch,
            commit_index,
            &voter_id,
            &req_id,
            &obj_id,
            shard_id,
            version,
            &hash_hex,
            target_node,
            operation_type,
        );
        let sig = signing_key.sign(&msg);
        let signature_hex = encode_hex(&sig.to_bytes());
        Self {
            voter_id,
            epoch,
            commit_index,
            request_id: req_id,
            object_id: obj_id,
            shard_id,
            version,
            payload_sha256_hex: hash_hex,
            signature_hex,
            target_node: Some(target_node),
            operation_type: Some(operation_type),
        }
    }

    pub fn verify(&self, voter_key: &VerifyingKey) -> Result<(), RpcError> {
        let msg = match (self.target_node, self.operation_type) {
            (Some(tn), Some(op)) => Self::canonical_message_v2(
                self.epoch,
                self.commit_index,
                &self.voter_id,
                &self.request_id,
                &self.object_id,
                self.shard_id,
                self.version,
                &self.payload_sha256_hex,
                tn,
                op,
            ),
            _ => Self::canonical_message(
                self.epoch,
                self.commit_index,
                &self.voter_id,
                &self.request_id,
                &self.object_id,
                self.shard_id,
                self.version,
                &self.payload_sha256_hex,
            ),
        };
        let sig_bytes = decode_hex_exact::<64>(&self.signature_hex)
            .map_err(|e| RpcError::Unauthorized(format!("bad permit grant sig hex: {}", e)))?;
        let sig = Signature::from_bytes(&sig_bytes);
        voter_key
            .verify_strict(&msg, &sig)
            .map_err(|e| RpcError::Unauthorized(format!("invalid permit grant signature: {}", e)))
    }
}

/// Multi-stage deletion lifecycle state to eliminate TOCTOU race conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ShardDeletionState {
    Candidate,
    EligibilityChecked,
    DeletionReserved,
    Tombstoned,
    Quarantined,
    PhysicallyDeleted,
}

/// Lifecycle tracking record for a shard marked for deletion or garbage collection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardDeletionLifecycleRecord {
    pub object_id: String,
    pub shard_id: u32,
    pub version: u64,
    pub state: ShardDeletionState,
    pub reservation_deadline_secs: u64,
    pub domain_id: Option<String>,
}

/// A consensus-authorized operation permit proving majority quorum agreement
/// for a specific mutating operation at an exact term, commit index, and object generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumMutationPermit {
    pub epoch: u64,
    pub commit_index: u64,
    pub request_id: String,
    pub object_id: String,
    pub shard_id: u32,
    pub version: u64,
    pub payload_sha256_hex: String,
    pub timestamp_unix_secs: u64,
    pub grants: Vec<QuorumPermitGrant>,
    pub target_node: Option<NodeId>,
    pub operation_type: Option<PermitOperationType>,
    pub cluster_incarnation: Option<ClusterIncarnationId>,
}

impl QuorumMutationPermit {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        epoch: u64,
        commit_index: u64,
        request_id: impl Into<String>,
        object_id: impl Into<String>,
        shard_id: u32,
        version: u64,
        payload_sha256_hex: impl Into<String>,
        timestamp_unix_secs: u64,
        grants: Vec<QuorumPermitGrant>,
    ) -> Self {
        Self {
            epoch,
            commit_index,
            request_id: request_id.into(),
            object_id: object_id.into(),
            shard_id,
            version,
            payload_sha256_hex: payload_sha256_hex.into(),
            timestamp_unix_secs,
            grants,
            target_node: None,
            operation_type: None,
            cluster_incarnation: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_v2(
        epoch: u64,
        commit_index: u64,
        request_id: impl Into<String>,
        object_id: impl Into<String>,
        shard_id: u32,
        version: u64,
        payload_sha256_hex: impl Into<String>,
        timestamp_unix_secs: u64,
        target_node: NodeId,
        operation_type: PermitOperationType,
        grants: Vec<QuorumPermitGrant>,
    ) -> Self {
        Self {
            epoch,
            commit_index,
            request_id: request_id.into(),
            object_id: object_id.into(),
            shard_id,
            version,
            payload_sha256_hex: payload_sha256_hex.into(),
            timestamp_unix_secs,
            grants,
            target_node: Some(target_node),
            operation_type: Some(operation_type),
            cluster_incarnation: None,
        }
    }

    pub fn with_incarnation(mut self, inc: ClusterIncarnationId) -> Self {
        self.cluster_incarnation = Some(inc);
        self
    }

    pub fn verify_quorum(
        &self,
        voter_keys: &BTreeMap<CoordinatorId, VerifyingKey>,
        threshold: usize,
    ) -> Result<(), RpcError> {
        if self.grants.len() < threshold {
            return Err(RpcError::Unauthorized(format!(
                "insufficient quorum permit grants: {} < required threshold {}",
                self.grants.len(),
                threshold
            )));
        }

        let mut seen_voters = BTreeSet::new();
        for grant in &self.grants {
            if grant.epoch != self.epoch
                || grant.commit_index != self.commit_index
                || grant.request_id != self.request_id
                || grant.object_id != self.object_id
                || grant.shard_id != self.shard_id
                || grant.version != self.version
                || grant.payload_sha256_hex != self.payload_sha256_hex
            {
                return Err(RpcError::Unauthorized(
                    "mutation permit grant parameters mismatch permit envelope".into(),
                ));
            }

            if self.target_node.is_some() && grant.target_node != self.target_node {
                return Err(RpcError::Unauthorized(format!(
                    "grant target node mismatch: grant {:?} != permit {:?}",
                    grant.target_node, self.target_node
                )));
            }

            if self.operation_type.is_some() && grant.operation_type != self.operation_type {
                return Err(RpcError::Unauthorized(format!(
                    "grant operation type mismatch: grant {:?} != permit {:?}",
                    grant.operation_type, self.operation_type
                )));
            }

            if !seen_voters.insert(grant.voter_id.clone()) {
                return Err(RpcError::Unauthorized(format!(
                    "duplicate grant from voter {}",
                    grant.voter_id
                )));
            }

            let voter_key = voter_keys.get(&grant.voter_id).ok_or_else(|| {
                RpcError::Unauthorized(format!("unknown quorum voter {}", grant.voter_id))
            })?;
            grant.verify(voter_key)?;
        }

        if seen_voters.len() < threshold {
            return Err(RpcError::Unauthorized(
                "distinct quorum voters below majority threshold".into(),
            ));
        }

        Ok(())
    }
}

/// Durable storage node-side epoch fencing tracker.
///
/// Persisted at `<node_root>/epoch.rec` to survive daemon restarts and SIGKILL termination.
#[derive(Debug, Clone)]
pub struct NodeEpochFencingTracker {
    file_path: PathBuf,
    highest_seen_epoch: Arc<AtomicU64>,
    highest_seen_commit_index: Arc<AtomicU64>,
    object_generations: Arc<RwLock<BTreeMap<String, u64>>>,
    executed_requests: Arc<RwLock<BTreeSet<String>>>,
    retention_watermarks: Arc<RwLock<BTreeMap<String, u64>>>,
    protected_checkpoints: Arc<RwLock<BTreeSet<String>>>,
    retained_historical_versions: Arc<RwLock<BTreeMap<String, BTreeSet<u64>>>>,
    active_legal_holds: Arc<RwLock<BTreeMap<String, String>>>,
    in_flight_transactions: Arc<RwLock<BTreeMap<String, u64>>>,
    strict_mode: Arc<AtomicBool>,
    current_incarnation: Arc<RwLock<ClusterIncarnationId>>,
    authority_record: Arc<RwLock<Option<ClusterAuthorityRecord>>>,
    coexistence_warning_active: Arc<AtomicBool>,
    active_repair_reservations: Arc<RwLock<BTreeMap<String, BTreeSet<u32>>>>,
    deletion_lifecycle: Arc<RwLock<BTreeMap<String, ShardDeletionLifecycleRecord>>>,
}

impl NodeEpochFencingTracker {
    pub const FILE_NAME: &'static str = "epoch.rec";

    /// Opens or initializes the fencing record in the storage node root.
    pub fn open_or_init(storage_root: &Path) -> Result<Self, RpcError> {
        let file_path = storage_root.join(Self::FILE_NAME);
        let current_val = if file_path.exists() {
            let mut file = File::open(&file_path)
                .map_err(|e| RpcError::Io(format!("open epoch.rec: {}", e)))?;
            let mut buf = String::new();
            file.read_to_string(&mut buf)
                .map_err(|e| RpcError::Io(format!("read epoch.rec: {}", e)))?;
            Self::parse_epoch(&buf)?
        } else {
            let init_val = 0u64;
            let text = format!("HIGHEST_EPOCH={}\n", init_val);
            write_and_sync_file(&file_path, text.as_bytes())
                .map_err(|e| RpcError::Io(format!("init epoch.rec: {}", e)))?;
            if let Some(parent) = file_path.parent() {
                let _ = sync_directory(parent);
            }
            init_val
        };

        let mut object_generations = BTreeMap::new();
        if let Ok(entries) = fs::read_dir(storage_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(fname) = p.file_name().and_then(|n| n.to_str()) {
                    if let Some(obj_id) = fname.strip_suffix(".gen") {
                        if let Ok(content) = fs::read_to_string(&p) {
                            for line in content.lines() {
                                if let Some(g) = line.strip_prefix("GENERATION=") {
                                    if let Ok(gen) = g.trim().parse::<u64>() {
                                        object_generations.insert(obj_id.to_string(), gen);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let ci_file = storage_root.join("commit_index.rec");
        let init_ci = if ci_file.exists() {
            fs::read_to_string(&ci_file)
                .ok()
                .and_then(|s| {
                    s.lines().find_map(|l| {
                        l.strip_prefix("COMMIT_INDEX=")
                            .and_then(|v| v.trim().parse::<u64>().ok())
                    })
                })
                .unwrap_or(0)
        } else {
            0
        };

        let req_file = storage_root.join("executed_requests.rec");
        let mut executed_requests = BTreeSet::new();
        if req_file.exists() {
            if let Ok(content) = fs::read_to_string(&req_file) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        executed_requests.insert(trimmed.to_string());
                    }
                }
            }
        }

        let inc_file = storage_root.join("incarnation.rec");
        let initial_incarnation = if inc_file.exists() {
            fs::read_to_string(&inc_file)
                .ok()
                .and_then(|s| ClusterIncarnationId::from_wire_string(s.trim()).ok())
                .unwrap_or_else(|| ClusterIncarnationId::genesis("fsp-cluster-default"))
        } else {
            ClusterIncarnationId::genesis("fsp-cluster-default")
        };

        let auth_file = storage_root.join("authority.rec");
        let initial_auth = if auth_file.exists() {
            fs::read_to_string(&auth_file)
                .ok()
                .and_then(|s| ClusterAuthorityRecord::decode(&s).ok())
        } else {
            None
        };

        Ok(Self {
            file_path,
            highest_seen_epoch: Arc::new(AtomicU64::new(current_val)),
            highest_seen_commit_index: Arc::new(AtomicU64::new(init_ci)),
            object_generations: Arc::new(RwLock::new(object_generations)),
            executed_requests: Arc::new(RwLock::new(executed_requests)),
            retention_watermarks: Arc::new(RwLock::new(BTreeMap::new())),
            protected_checkpoints: Arc::new(RwLock::new(BTreeSet::new())),
            retained_historical_versions: Arc::new(RwLock::new(BTreeMap::new())),
            active_legal_holds: Arc::new(RwLock::new(BTreeMap::new())),
            in_flight_transactions: Arc::new(RwLock::new(BTreeMap::new())),
            strict_mode: Arc::new(AtomicBool::new(false)),
            current_incarnation: Arc::new(RwLock::new(initial_incarnation)),
            authority_record: Arc::new(RwLock::new(initial_auth)),
            coexistence_warning_active: Arc::new(AtomicBool::new(false)),
            active_repair_reservations: Arc::new(RwLock::new(BTreeMap::new())),
            deletion_lifecycle: Arc::new(RwLock::new(BTreeMap::new())),
        })
    }

    pub fn set_strict_mode(&self, enabled: bool) {
        self.strict_mode.store(enabled, Ordering::SeqCst);
    }

    pub fn is_strict_mode(&self) -> bool {
        self.strict_mode.load(Ordering::SeqCst)
    }

    pub fn set_retention_watermark(&self, object_id: &str, min_retained_gen: u64) {
        self.retention_watermarks
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(object_id.to_string(), min_retained_gen);
    }

    pub fn get_retention_watermark(&self, object_id: &str) -> Option<u64> {
        self.retention_watermarks
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(object_id)
            .copied()
    }

    pub fn protect_checkpoint(&self, object_id: &str, generation: u64) {
        self.protected_checkpoints
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(format!("{}:{}", object_id, generation));
    }

    pub fn unprotect_checkpoint(&self, object_id: &str, generation: u64) {
        self.protected_checkpoints
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&format!("{}:{}", object_id, generation));
    }

    pub fn is_checkpoint_protected(&self, object_id: &str, generation: u64) -> bool {
        self.protected_checkpoints
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&format!("{}:{}", object_id, generation))
    }

    pub fn add_retained_historical_version(&self, object_id: &str, version: u64) {
        self.retained_historical_versions
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .entry(object_id.to_string())
            .or_default()
            .insert(version);
    }

    pub fn remove_retained_historical_version(&self, object_id: &str, version: u64) {
        let mut map = self
            .retained_historical_versions
            .write()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(set) = map.get_mut(object_id) {
            set.remove(&version);
        }
    }

    pub fn is_historical_version_retained(&self, object_id: &str, version: u64) -> bool {
        self.retained_historical_versions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(object_id)
            .map(|set| set.contains(&version))
            .unwrap_or(false)
    }

    pub fn set_legal_hold(&self, object_id: &str, reason: impl Into<String>) {
        self.active_legal_holds
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(object_id.to_string(), reason.into());
    }

    pub fn release_legal_hold(&self, object_id: &str) {
        self.active_legal_holds
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(object_id);
    }

    pub fn has_legal_hold(&self, object_id: &str) -> bool {
        self.active_legal_holds
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(object_id)
    }

    pub fn record_in_flight_tx(&self, object_id: &str, tx_id: u64) {
        self.in_flight_transactions
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(object_id.to_string(), tx_id);
    }

    pub fn clear_in_flight_tx(&self, object_id: &str) {
        self.in_flight_transactions
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(object_id);
    }

    pub fn has_in_flight_tx(&self, object_id: &str) -> bool {
        self.in_flight_transactions
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(object_id)
    }

    pub fn audit_gc_eligibility(
        &self,
        object_id: &str,
        version: u64,
        surviving_shards: Option<usize>,
    ) -> Result<(), RpcError> {
        // 1. Legal hold check
        if let Some(reason) = self
            .active_legal_holds
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(object_id)
        {
            return Err(RpcError::Unauthorized(format!(
                "GC or shard deletion rejected: active legal hold on object '{}' ({})",
                object_id, reason
            )));
        }

        // 2. In-flight transaction / repair check
        if self.has_in_flight_tx(object_id) {
            return Err(RpcError::Unauthorized(format!(
                "GC or shard deletion rejected: object '{}' has an in-flight transaction or repair",
                object_id
            )));
        }

        // 3. Retention watermark check
        if let Some(&min_ret) = self
            .retention_watermarks
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(object_id)
        {
            if version <= min_ret {
                return Err(RpcError::Unauthorized(format!(
                    "GC or shard deletion rejected: generation {} <= retention watermark {}",
                    version, min_ret
                )));
            }
        }

        // 4. Checkpoint protection check
        let key = format!("{}:{}", object_id, version);
        if self
            .protected_checkpoints
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains(&key)
        {
            return Err(RpcError::Unauthorized(format!(
                "GC or shard deletion rejected: generation {} of object {} is protected by active recovery checkpoint",
                version, object_id
            )));
        }

        // 5. Current active committed generation check
        let cur_gen = self
            .object_generations
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(object_id)
            .copied()
            .unwrap_or(0);
        if version >= cur_gen && cur_gen > 0 {
            return Err(RpcError::Unauthorized(format!(
                "GC or shard deletion rejected: cannot delete active committed generation {}",
                version
            )));
        }

        // 6. Retained historical version set check
        if self.is_historical_version_retained(object_id, version) {
            return Err(RpcError::Unauthorized(format!(
                "GC or shard deletion rejected: generation {} of object {} is in retained historical versions set",
                version, object_id
            )));
        }

        // 7. Surviving shards threshold check (cannot drop at or below k=12)
        if let Some(surviving) = surviving_shards {
            if surviving <= 12 {
                return Err(RpcError::Unauthorized(format!(
                    "GC or shard deletion rejected: surviving shards ({}) would drop at or below minimum threshold k=12",
                    surviving
                )));
            }
        }

        Ok(())
    }

    pub fn current_incarnation(&self) -> ClusterIncarnationId {
        self.current_incarnation
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn authority_record(&self) -> Option<ClusterAuthorityRecord> {
        self.authority_record
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn is_coexistence_warning_active(&self) -> bool {
        self.coexistence_warning_active.load(Ordering::SeqCst)
    }

    pub fn set_coexistence_warning(&self, active: bool) {
        self.coexistence_warning_active
            .store(active, Ordering::SeqCst);
    }

    pub fn set_incarnation_direct(&self, inc: ClusterIncarnationId) -> Result<(), RpcError> {
        if let Some(parent) = self.file_path.parent() {
            let inc_file = parent.join("incarnation.rec");
            let text = inc.to_wire_string();
            write_and_sync_file(&inc_file, text.as_bytes())
                .map_err(|e| RpcError::Io(format!("sync incarnation.rec: {}", e)))?;
            let _ = sync_directory(parent);
        }
        *self
            .current_incarnation
            .write()
            .unwrap_or_else(|p| p.into_inner()) = inc;
        Ok(())
    }

    pub fn adopt_cluster_incarnation(
        &self,
        record: &ClusterAuthorityRecord,
        recovery_auth_key: &VerifyingKey,
    ) -> Result<(), RpcError> {
        record.verify(recovery_auth_key).map_err(|e| {
            RpcError::Unauthorized(format!("invalid authority transition record: {}", e))
        })?;

        let cur = self.current_incarnation();

        // If cluster IDs match, sequence must be strictly greater than current
        if record.incarnation_id.cluster_id == cur.cluster_id {
            if record.incarnation_id.incarnation_seq < cur.incarnation_seq {
                return Err(RpcError::Fenced {
                    current_epoch: cur.incarnation_seq,
                    presented_epoch: record.incarnation_id.incarnation_seq,
                });
            }
            if record.incarnation_id.incarnation_seq == cur.incarnation_seq {
                if record.incarnation_id.incarnation_token != cur.incarnation_token {
                    return Err(RpcError::Unauthorized(format!(
                        "conflicting incarnation at sequence {}: token mismatch",
                        cur.incarnation_seq
                    )));
                }
                // Idempotent re-adoption of current incarnation
                return Ok(());
            }
        }

        // If operational isolation or old quorum fencing was NOT attested, set coexistence warning
        if !record.operational_isolation_attested || !record.old_quorum_fenced {
            self.set_coexistence_warning(true);
        } else {
            self.set_coexistence_warning(false);
        }

        if let Some(parent) = self.file_path.parent() {
            let auth_file = parent.join("authority.rec");
            let text = record.encode();
            write_and_sync_file(&auth_file, text.as_bytes())
                .map_err(|e| RpcError::Io(format!("sync authority.rec: {}", e)))?;

            let inc_file = parent.join("incarnation.rec");
            let inc_text = record.incarnation_id.to_wire_string();
            write_and_sync_file(&inc_file, inc_text.as_bytes())
                .map_err(|e| RpcError::Io(format!("sync incarnation.rec: {}", e)))?;
            let _ = sync_directory(parent);
        }

        *self
            .current_incarnation
            .write()
            .unwrap_or_else(|p| p.into_inner()) = record.incarnation_id.clone();
        *self
            .authority_record
            .write()
            .unwrap_or_else(|p| p.into_inner()) = Some(record.clone());

        Ok(())
    }

    pub fn verify_incarnation_binding(
        &self,
        presented: &ClusterIncarnationId,
    ) -> Result<(), RpcError> {
        let cur = self.current_incarnation();
        if presented.cluster_id != cur.cluster_id {
            return Err(RpcError::Unauthorized(format!(
                "cluster ID mismatch: presented '{}' != active '{}'",
                presented.cluster_id, cur.cluster_id
            )));
        }
        if presented.incarnation_seq < cur.incarnation_seq {
            return Err(RpcError::Fenced {
                current_epoch: cur.incarnation_seq,
                presented_epoch: presented.incarnation_seq,
            });
        }
        if presented.incarnation_seq > cur.incarnation_seq {
            return Err(RpcError::Unauthorized(format!(
                "storage node has not adopted higher incarnation sequence {} (active sequence is {})",
                presented.incarnation_seq, cur.incarnation_seq
            )));
        }
        if presented.incarnation_token != cur.incarnation_token {
            return Err(RpcError::Unauthorized(format!(
                "incarnation token mismatch: presented '{}' != active '{}'",
                presented.incarnation_token, cur.incarnation_token
            )));
        }
        Ok(())
    }

    pub fn register_active_repair(&self, object_id: &str, shard_id: u32) {
        self.active_repair_reservations
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .entry(object_id.to_string())
            .or_default()
            .insert(shard_id);
    }

    pub fn unregister_active_repair(&self, object_id: &str, shard_id: u32) {
        let mut map = self
            .active_repair_reservations
            .write()
            .unwrap_or_else(|p| p.into_inner());
        if let Some(set) = map.get_mut(object_id) {
            set.remove(&shard_id);
        }
    }

    pub fn is_shard_repairing(&self, object_id: &str, shard_id: u32) -> bool {
        self.active_repair_reservations
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(object_id)
            .map(|set| set.contains(&shard_id))
            .unwrap_or(false)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn reserve_shard_deletion(
        &self,
        object_id: &str,
        shard_id: u32,
        version: u64,
        cur_time_secs: u64,
        lease_secs: u64,
        surviving_shards: Option<usize>,
        domain_id: Option<&str>,
    ) -> Result<(), RpcError> {
        self.audit_gc_eligibility(object_id, version, surviving_shards)?;

        if self.is_shard_repairing(object_id, shard_id) {
            return Err(RpcError::Unauthorized(format!(
                "GC deletion reservation rejected: shard {}:{} is undergoing active repair",
                object_id, shard_id
            )));
        }

        let key = format!("{}:{}:{}", object_id, shard_id, version);
        let mut lifecycle = self
            .deletion_lifecycle
            .write()
            .unwrap_or_else(|p| p.into_inner());

        if let Some(rec) = lifecycle.get(&key) {
            if rec.state == ShardDeletionState::PhysicallyDeleted {
                return Err(RpcError::Unauthorized(format!(
                    "shard {}:{}:{} is already physically deleted",
                    object_id, shard_id, version
                )));
            }
        }

        lifecycle.insert(
            key,
            ShardDeletionLifecycleRecord {
                object_id: object_id.to_string(),
                shard_id,
                version,
                state: ShardDeletionState::DeletionReserved,
                reservation_deadline_secs: cur_time_secs.saturating_add(lease_secs),
                domain_id: domain_id.map(|s| s.to_string()),
            },
        );

        Ok(())
    }

    pub fn tombstone_shard_deletion(
        &self,
        object_id: &str,
        shard_id: u32,
        version: u64,
    ) -> Result<(), RpcError> {
        let key = format!("{}:{}:{}", object_id, shard_id, version);
        let mut lifecycle = self
            .deletion_lifecycle
            .write()
            .unwrap_or_else(|p| p.into_inner());

        let rec = lifecycle.get_mut(&key).ok_or_else(|| {
            RpcError::Unauthorized(format!(
                "shard {}:{}:{} must be reserved before tombstoning",
                object_id, shard_id, version
            ))
        })?;

        rec.state = ShardDeletionState::Tombstoned;
        Ok(())
    }

    pub fn commit_physical_deletion(
        &self,
        object_id: &str,
        shard_id: u32,
        version: u64,
    ) -> Result<(), RpcError> {
        if self.is_shard_repairing(object_id, shard_id) {
            return Err(RpcError::Unauthorized(format!(
                "physical deletion aborted: shard {}:{} has an active repair reservation",
                object_id, shard_id
            )));
        }

        let cur_gen = self
            .object_generations
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(object_id)
            .copied()
            .unwrap_or(0);
        if version >= cur_gen && cur_gen > 0 {
            return Err(RpcError::Unauthorized(format!(
                "physical deletion aborted: version {} became the current active generation",
                version
            )));
        }

        let key = format!("{}:{}:{}", object_id, shard_id, version);
        let mut lifecycle = self
            .deletion_lifecycle
            .write()
            .unwrap_or_else(|p| p.into_inner());

        if let Some(rec) = lifecycle.get_mut(&key) {
            rec.state = ShardDeletionState::PhysicallyDeleted;
        } else {
            lifecycle.insert(
                key,
                ShardDeletionLifecycleRecord {
                    object_id: object_id.to_string(),
                    shard_id,
                    version,
                    state: ShardDeletionState::PhysicallyDeleted,
                    reservation_deadline_secs: 0,
                    domain_id: None,
                },
            );
        }

        Ok(())
    }

    pub fn get_deletion_state(
        &self,
        object_id: &str,
        shard_id: u32,
        version: u64,
    ) -> Option<ShardDeletionState> {
        let key = format!("{}:{}:{}", object_id, shard_id, version);
        self.deletion_lifecycle
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            .map(|r| r.state)
    }

    fn parse_epoch(text: &str) -> Result<u64, RpcError> {
        for line in text.lines() {
            if let Some(val) = line.strip_prefix("HIGHEST_EPOCH=") {
                return val
                    .trim()
                    .parse::<u64>()
                    .map_err(|e| RpcError::SerializationError(format!("bad epoch: {}", e)));
            }
        }
        Err(RpcError::SerializationError(
            "missing HIGHEST_EPOCH=".into(),
        ))
    }

    pub fn highest_seen_epoch(&self) -> u64 {
        self.highest_seen_epoch.load(Ordering::SeqCst)
    }

    /// Verifies that a mutating request's presented epoch is valid ($\ge$ current epoch).
    /// If `presented_epoch > current`, durably updates disk and advances the in-memory watermark.
    /// If `presented_epoch < current`, rejects with `RpcError::Fenced`.
    pub fn check_and_update(&self, presented_epoch: u64) -> Result<(), RpcError> {
        let cur = self.highest_seen_epoch.load(Ordering::SeqCst);
        if presented_epoch < cur {
            return Err(RpcError::Fenced {
                current_epoch: cur,
                presented_epoch,
            });
        }

        if presented_epoch > cur {
            if self.strict_mode.load(Ordering::SeqCst) {
                return Err(RpcError::Unauthorized(
                    "storage node strict mode requires verified LeadershipCertificate to advance epoch".into(),
                ));
            }

            // Persist new high-water mark to disk
            let text = format!("HIGHEST_EPOCH={}\n", presented_epoch);
            write_and_sync_file(&self.file_path, text.as_bytes())
                .map_err(|e| RpcError::Io(format!("sync epoch.rec: {}", e)))?;
            if let Some(parent) = self.file_path.parent() {
                let _ = sync_directory(parent);
            }
            self.highest_seen_epoch
                .store(presented_epoch, Ordering::SeqCst);
        }

        Ok(())
    }

    /// Advances the storage node epoch high-water mark strictly when accompanied by a
    /// valid cryptographic [`LeadershipCertificate`] proving majority quorum election.
    pub fn check_and_update_with_certificate(
        &self,
        cert: &LeadershipCertificate,
        voter_keys: &BTreeMap<CoordinatorId, VerifyingKey>,
        threshold: usize,
    ) -> Result<(), RpcError> {
        cert.verify_quorum(voter_keys, threshold)
            .map_err(|e| RpcError::Unauthorized(format!("invalid quorum certificate: {}", e)))?;

        let cur = self.highest_seen_epoch.load(Ordering::SeqCst);
        if cert.epoch < cur {
            return Err(RpcError::Fenced {
                current_epoch: cur,
                presented_epoch: cert.epoch,
            });
        }

        if cert.epoch > cur {
            let text = format!("HIGHEST_EPOCH={}\n", cert.epoch);
            write_and_sync_file(&self.file_path, text.as_bytes())
                .map_err(|e| RpcError::Io(format!("sync epoch.rec: {}", e)))?;
            if let Some(parent) = self.file_path.parent() {
                let _ = sync_directory(parent);
            }
            self.highest_seen_epoch.store(cert.epoch, Ordering::SeqCst);
        }

        Ok(())
    }

    /// Verifies that a mutating request's [`FencedOperationToken`] is valid:
    /// 1. Token epoch is not fenced out by current node watermark.
    /// 2. Object ID, shard ID, and payload SHA-256 digest match expected parameters.
    /// 3. Token Ed25519 signature is strictly valid against the certified leader's public key.
    pub fn verify_fenced_operation(
        &self,
        token: &FencedOperationToken,
        expected_object_id: &str,
        expected_shard_id: u32,
        expected_payload_hash_hex: &str,
        leader_key: &VerifyingKey,
    ) -> Result<(), RpcError> {
        if let Some(ref inc) = token.cluster_incarnation {
            self.verify_incarnation_binding(inc)?;
        }

        let cur = self.highest_seen_epoch.load(Ordering::SeqCst);
        if token.epoch < cur {
            return Err(RpcError::Fenced {
                current_epoch: cur,
                presented_epoch: token.epoch,
            });
        }

        if token.object_id != expected_object_id {
            return Err(RpcError::Unauthorized(format!(
                "object_id mismatch in fenced op token: expected {}, got {}",
                expected_object_id, token.object_id
            )));
        }

        if token.shard_id != expected_shard_id {
            return Err(RpcError::Unauthorized(format!(
                "shard_id mismatch in fenced op token: expected {}, got {}",
                expected_shard_id, token.shard_id
            )));
        }

        if token.payload_sha256_hex != expected_payload_hash_hex {
            return Err(RpcError::Unauthorized(format!(
                "payload digest mismatch in fenced op token: expected {}, got {}",
                expected_payload_hash_hex, token.payload_sha256_hex
            )));
        }

        token.verify(leader_key)?;
        Ok(())
    }

    pub fn highest_seen_commit_index(&self) -> u64 {
        self.highest_seen_commit_index.load(Ordering::SeqCst)
    }

    pub fn recorded_generation(&self, object_id: &str) -> Option<u64> {
        self.object_generations
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(object_id)
            .copied()
    }

    pub fn is_request_executed(&self, request_id: &str) -> bool {
        self.executed_requests
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains(request_id)
    }

    /// Verifies a [`QuorumMutationPermit`] and applies the mutation authority to the node state:
    /// 1. Verifies that the permit carries majority quorum voter signatures.
    /// 2. Verifies that the permit epoch is not stale against the node's watermark.
    /// 3. Verifies that the permit commit index is monotonic (commit_index >= highest_seen_commit_index).
    /// 4. Verifies durable object generation to strictly reject conflicting or rollback generations.
    /// 5. Validates request idempotency and prevents replays.
    pub fn verify_and_apply_mutation_permit(
        &self,
        permit: &QuorumMutationPermit,
        expected_object_id: &str,
        expected_shard_id: u32,
        expected_payload_hash_hex: &str,
        voter_keys: &BTreeMap<CoordinatorId, VerifyingKey>,
        threshold: usize,
    ) -> Result<(), RpcError> {
        // 1. Quorum majority validation
        permit.verify_quorum(voter_keys, threshold)?;

        if self.is_coexistence_warning_active() {
            return Err(RpcError::Unauthorized(
                "mutation permit rejected: unverified old-quorum coexistence warning active - fail closed against mutations".into(),
            ));
        }

        if let Some(ref inc) = permit.cluster_incarnation {
            self.verify_incarnation_binding(inc)?;
        }

        // 2. Epoch fencing validation
        let cur_epoch = self.highest_seen_epoch.load(Ordering::SeqCst);
        if permit.epoch < cur_epoch {
            return Err(RpcError::Fenced {
                current_epoch: cur_epoch,
                presented_epoch: permit.epoch,
            });
        }
        if permit.epoch > cur_epoch {
            let text = format!("HIGHEST_EPOCH={}\n", permit.epoch);
            write_and_sync_file(&self.file_path, text.as_bytes())
                .map_err(|e| RpcError::Io(format!("sync epoch.rec: {}", e)))?;
            if let Some(parent) = self.file_path.parent() {
                let _ = sync_directory(parent);
            }
            self.highest_seen_epoch
                .store(permit.epoch, Ordering::SeqCst);
        }

        // 3. Commit index monotonicity validation
        let cur_ci = self.highest_seen_commit_index.load(Ordering::SeqCst);
        if permit.commit_index < cur_ci {
            return Err(RpcError::Unauthorized(format!(
                "stale commit index in mutation permit: {} < recorded {}",
                permit.commit_index, cur_ci
            )));
        }
        if permit.commit_index > cur_ci {
            if let Some(parent) = self.file_path.parent() {
                let ci_file = parent.join("commit_index.rec");
                let text = format!("COMMIT_INDEX={}\n", permit.commit_index);
                let _ = write_and_sync_file(&ci_file, text.as_bytes());
            }
            self.highest_seen_commit_index
                .store(permit.commit_index, Ordering::SeqCst);
        }

        // 4. Expected target parameter validation
        if permit.object_id != expected_object_id {
            return Err(RpcError::Unauthorized(format!(
                "object_id mismatch: expected {}, got {}",
                expected_object_id, permit.object_id
            )));
        }
        if permit.shard_id != expected_shard_id {
            return Err(RpcError::Unauthorized(format!(
                "shard_id mismatch: expected {}, got {}",
                expected_shard_id, permit.shard_id
            )));
        }
        if permit.payload_sha256_hex != expected_payload_hash_hex {
            return Err(RpcError::Unauthorized(format!(
                "payload digest mismatch: expected {}, got {}",
                expected_payload_hash_hex, permit.payload_sha256_hex
            )));
        }

        // 5. Durable object generation check & conflict detection
        let is_deletion = matches!(
            permit.operation_type,
            Some(PermitOperationType::DeleteShard) | Some(PermitOperationType::GarbageCollectShard)
        );

        if is_deletion {
            self.audit_gc_eligibility(&permit.object_id, permit.version, None)?;
        } else {
            let mut gens = self
                .object_generations
                .write()
                .unwrap_or_else(|p| p.into_inner());
            let cur_gen = gens.get(&permit.object_id).copied().unwrap_or(0);

            if permit.version < cur_gen {
                return Err(RpcError::Unauthorized(format!(
                    "generation rollback rejected: attempted version {} < recorded generation {}",
                    permit.version, cur_gen
                )));
            }
            if permit.version == cur_gen {
                let reqs = self
                    .executed_requests
                    .read()
                    .unwrap_or_else(|p| p.into_inner());
                if !reqs.contains(&permit.request_id) {
                    return Err(RpcError::Unauthorized(format!(
                        "conflicting mutation at existing generation {}: new request_id {}",
                        permit.version, permit.request_id
                    )));
                }
            } else {
                gens.insert(permit.object_id.clone(), permit.version);
                if let Some(parent) = self.file_path.parent() {
                    let gen_file = parent.join(format!("{}.gen", permit.object_id));
                    let text = format!("GENERATION={}\n", permit.version);
                    let _ = write_and_sync_file(&gen_file, text.as_bytes());
                }
            }
        }

        // 6. Record executed request ID
        let mut reqs = self
            .executed_requests
            .write()
            .unwrap_or_else(|p| p.into_inner());
        reqs.insert(permit.request_id.clone());
        if let Some(parent) = self.file_path.parent() {
            let req_file = parent.join("executed_requests.rec");
            let mut text = String::new();
            for r in reqs.iter() {
                text.push_str(r);
                text.push('\n');
            }
            let _ = write_and_sync_file(&req_file, text.as_bytes());
        }

        Ok(())
    }

    /// Verifies a [`QuorumMutationPermit`] with Phase 5 enhanced security:
    /// strictly enforces destination node identity binding (`NodeId`) and mutating operation type
    /// (`PermitOperationType`), preventing cross-node permit replays or re-purposing staging permits
    /// for destructive deletions or GC.
    #[allow(clippy::too_many_arguments)]
    pub fn verify_and_apply_mutation_permit_v2(
        &self,
        permit: &QuorumMutationPermit,
        expected_object_id: &str,
        expected_shard_id: u32,
        expected_payload_hash_hex: &str,
        expected_node_id: NodeId,
        expected_operation: PermitOperationType,
        voter_keys: &BTreeMap<CoordinatorId, VerifyingKey>,
        threshold: usize,
    ) -> Result<(), RpcError> {
        if permit.target_node != Some(expected_node_id) {
            return Err(RpcError::Unauthorized(format!(
                "destination node mismatch: expected NodeId({}), got {:?}",
                expected_node_id.0, permit.target_node
            )));
        }
        if permit.operation_type != Some(expected_operation) {
            return Err(RpcError::Unauthorized(format!(
                "operation type mismatch: expected {:?}, got {:?}",
                expected_operation, permit.operation_type
            )));
        }

        self.verify_and_apply_mutation_permit(
            permit,
            expected_object_id,
            expected_shard_id,
            expected_payload_hash_hex,
            voter_keys,
            threshold,
        )
    }

    /// Verifies a [`QuorumMutationPermit`] with Phase 5.2 full GC reachability and threshold safety:
    /// verifies node binding, operation type, consensus quorum, epoch/commit monotonicity,
    /// and surviving shard count ($k=12$ erasure-coding redundancy threshold).
    #[allow(clippy::too_many_arguments)]
    pub fn verify_and_apply_mutation_permit_v3(
        &self,
        permit: &QuorumMutationPermit,
        expected_object_id: &str,
        expected_shard_id: u32,
        expected_payload_hash_hex: &str,
        expected_node_id: NodeId,
        expected_operation: PermitOperationType,
        surviving_shards: Option<usize>,
        voter_keys: &BTreeMap<CoordinatorId, VerifyingKey>,
        threshold: usize,
    ) -> Result<(), RpcError> {
        if permit.target_node != Some(expected_node_id) {
            return Err(RpcError::Unauthorized(format!(
                "destination node mismatch: expected NodeId({}), got {:?}",
                expected_node_id.0, permit.target_node
            )));
        }
        if permit.operation_type != Some(expected_operation) {
            return Err(RpcError::Unauthorized(format!(
                "operation type mismatch: expected {:?}, got {:?}",
                expected_operation, permit.operation_type
            )));
        }

        let is_deletion = matches!(
            expected_operation,
            PermitOperationType::DeleteShard | PermitOperationType::GarbageCollectShard
        );
        if is_deletion {
            if self.is_shard_repairing(expected_object_id, expected_shard_id) {
                return Err(RpcError::Unauthorized(format!(
                    "GC deletion rejected: shard {}:{} is undergoing active repair",
                    expected_object_id, expected_shard_id
                )));
            }
            self.audit_gc_eligibility(expected_object_id, permit.version, surviving_shards)?;
        }

        self.verify_and_apply_mutation_permit(
            permit,
            expected_object_id,
            expected_shard_id,
            expected_payload_hash_hex,
            voter_keys,
            threshold,
        )
    }
}
