//! Independent Storage Node Daemon engine (`src/node/daemon.rs`).
//!
//! Manages a dedicated storage root (`<node_root>/.fsp-node-marker`), persistent node identity,
//! capacity limit enforcement, local v2 shard container index, local transaction journal with
//! idempotent `tx_id` replay semantics, startup crash recovery, and handlers for all 7 RPCs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};

use crate::coordinator::fencing::NodeEpochFencingTracker;
use crate::model::node::NodeId;
use crate::network::protocol::{
    GetShardRequest, GetShardResponseMeta, HeartbeatResponse, NodeStatusResponse, PutShardAck,
    PutShardRequest, RpcError, RpcRequest, RpcResponse, StoredShardSummary, VerifyShardReport,
    VerifyShardRequest,
};
use crate::node::health::NodeHealthTelemetry;
use crate::node::idempotency::DurableIdempotencyStore;
use crate::node::identity::{NodeFailureDomainTopology, PersistentNodeIdentity};
use crate::storage::backend::StorageError;
use crate::storage::container::{ShardContainerReader, ShardContainerWriter};
use crate::storage::filesystem::{
    check_path_has_no_traversal, ensure_not_symlink, open_file_nofollow, sync_directory,
    write_and_sync_file,
};
use crate::storage::integrity::{sha256_bytes, Sha256Hasher};
use crate::storage::manifest::validate_safe_identifier;
use crate::storage::DurabilityLevel;

/// Sentinel marker file proving a directory is a dedicated FSP node daemon storage root.
pub const FSP_NODE_MARKER_FILE: &str = ".fsp-node-marker";
pub const FSP_NODE_MARKER_MAGIC: &str = "FSP_NODE_STORAGE_ROOT_V1";

/// Startup crash recovery report for an independent storage node daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCrashRecoveryReport {
    pub node_id: NodeId,
    pub uncommitted_journal_entries_aborted: usize,
    pub temp_container_files_cleaned: usize,
    pub valid_containers_rediscovered: usize,
    pub total_used_bytes_rediscovered: u64,
}

/// Configuration for initializing or starting a [`StorageNodeDaemon`].
#[derive(Debug, Clone)]
pub struct StorageNodeConfig {
    pub node_id: NodeId,
    pub listen_addr: String,
    pub storage_root: PathBuf,
    pub identity: PersistentNodeIdentity,
    pub capacity_limit_bytes: u64,
}

impl StorageNodeConfig {
    /// Creates a standard localhost node configuration for `node_id`.
    pub fn localhost(
        node_id: NodeId,
        listen_addr: &str,
        storage_root: &Path,
        cert_fingerprint_hex: &str,
        capacity_limit_bytes: u64,
    ) -> Self {
        let identity = PersistentNodeIdentity {
            node_id,
            identity_id: format!("node-{:02}", node_id.0),
            cert_fingerprint_hex: cert_fingerprint_hex.to_string(),
            topology: NodeFailureDomainTopology::single_host_local_daemon(node_id),
        };
        Self {
            node_id,
            listen_addr: listen_addr.to_string(),
            storage_root: storage_root.to_path_buf(),
            identity,
            capacity_limit_bytes,
        }
    }
}

/// Thread-safe storage node daemon state machine handling local v2 shard containers, local
/// transaction journaling, and RPC execution.
#[derive(Debug)]
pub struct StorageNodeDaemon {
    config: StorageNodeConfig,
    health: Arc<NodeHealthTelemetry>,
    shard_index: RwLock<BTreeMap<(String, usize), StoredShardSummary>>,
    idempotency_store: DurableIdempotencyStore,
    fencing_tracker: NodeEpochFencingTracker,
}

impl StorageNodeDaemon {
    /// Initializes or opens the node storage root, verifies `.fsp-node-marker` and persistent
    /// identity, runs startup crash recovery, and rebuilds the local shard index from disk.
    pub fn init_or_open(
        config: StorageNodeConfig,
    ) -> Result<(Arc<Self>, NodeCrashRecoveryReport), StorageError> {
        check_path_has_no_traversal(&config.storage_root)?;
        if config.storage_root.exists() {
            ensure_not_symlink(&config.storage_root)?;
        } else {
            fs::create_dir_all(&config.storage_root)?;
        }

        let marker = config.storage_root.join(FSP_NODE_MARKER_FILE);
        if marker.exists() {
            ensure_not_symlink(&marker)?;
            let content = fs::read_to_string(&marker)?;
            if !content.starts_with(FSP_NODE_MARKER_MAGIC) {
                return Err(StorageError::UninitializedStorageRoot(format!(
                    "invalid node marker in {}",
                    marker.display()
                )));
            }
        } else {
            let marker_text = format!("{}\nnode_id={}\n", FSP_NODE_MARKER_MAGIC, config.node_id.0);
            write_and_sync_file(&marker, marker_text.as_bytes())?;
        }

        config.identity.save_or_verify(&config.storage_root)?;

        let objects_dir = config.storage_root.join("objects");
        let journal_dir = config.storage_root.join("journal");
        let staging_dir = config.storage_root.join("staging");
        fs::create_dir_all(&objects_dir)?;
        fs::create_dir_all(&journal_dir)?;
        fs::create_dir_all(&staging_dir)?;
        ensure_not_symlink(&objects_dir)?;
        ensure_not_symlink(&journal_dir)?;
        ensure_not_symlink(&staging_dir)?;

        let idempotency_store = DurableIdempotencyStore::open_or_init(&config.storage_root)?;
        let fencing_tracker = NodeEpochFencingTracker::open_or_init(&config.storage_root)
            .map_err(|e| StorageError::Io(std::io::Error::other(e.to_string())))?;

        let daemon = Arc::new(Self {
            config,
            health: Arc::new(NodeHealthTelemetry::new()),
            shard_index: RwLock::new(BTreeMap::new()),
            idempotency_store,
            fencing_tracker,
        });

        let recovery_report = daemon.recover_and_rebuild_index()?;
        Ok((daemon, recovery_report))
    }

    /// Returns the node's [`NodeId`].
    pub const fn node_id(&self) -> NodeId {
        self.config.node_id
    }

    /// Returns the node's configured listen address.
    pub fn listen_addr(&self) -> &str {
        &self.config.listen_addr
    }

    /// Updates the listen address after binding to an ephemeral port (`127.0.0.1:0`).
    pub fn set_bound_listen_addr(&mut self, addr: String) {
        self.config.listen_addr = addr;
    }

    /// Returns the node's dedicated storage root path.
    pub fn storage_root(&self) -> &Path {
        &self.config.storage_root
    }

    /// Returns the node's [`PersistentNodeIdentity`].
    pub const fn identity(&self) -> &PersistentNodeIdentity {
        &self.config.identity
    }

    /// Returns the node's [`NodeHealthTelemetry`].
    pub fn health(&self) -> &Arc<NodeHealthTelemetry> {
        &self.health
    }

    /// Returns the highest seen leadership epoch recorded by this storage node.
    pub fn highest_seen_epoch(&self) -> u64 {
        self.fencing_tracker.highest_seen_epoch()
    }

    /// Returns the node's [`NodeEpochFencingTracker`].
    pub fn fencing_tracker(&self) -> &NodeEpochFencingTracker {
        &self.fencing_tracker
    }

    /// Enables or disables strict cryptographic quorum certificate enforcement.
    pub fn set_strict_fencing(&self, enabled: bool) {
        self.fencing_tracker.set_strict_mode(enabled);
    }

    /// Verifies that `<storage_root>/.fsp-node-marker` is intact before any filesystem operation.
    pub fn verify_node_root(&self) -> Result<(), RpcError> {
        ensure_not_symlink(&self.config.storage_root)
            .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        let marker = self.config.storage_root.join(FSP_NODE_MARKER_FILE);
        if !marker.exists() {
            return Err(RpcError::InternalNodeError(
                "missing .fsp-node-marker".into(),
            ));
        }
        ensure_not_symlink(&marker).map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        Ok(())
    }

    /// Resolves a strictly constrained container path `<storage_root>/objects/<object_id>/shard-YY.vZ.fspc`.
    pub fn resolve_container_path(
        &self,
        object_id: &str,
        shard_id: usize,
        version: u64,
    ) -> Result<PathBuf, RpcError> {
        self.verify_node_root()?;
        if !validate_safe_identifier(object_id) {
            return Err(RpcError::SerializationError(format!(
                "unsafe object_id {:?}",
                object_id
            )));
        }
        let obj_dir = self.config.storage_root.join("objects").join(object_id);
        if obj_dir.exists() {
            ensure_not_symlink(&obj_dir).map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        }
        let filename = format!("shard-{:02}.v{}.fspc", shard_id, version);
        let full = obj_dir.join(filename);
        if full.exists() {
            ensure_not_symlink(&full).map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        }
        Ok(full)
    }

    /// Runs startup crash recovery: cleans up uncommitted `.tmp` staging files and incomplete
    /// journal entries, then scans `<storage_root>/objects/` to rebuild and verify the shard index.
    pub fn recover_and_rebuild_index(&self) -> Result<NodeCrashRecoveryReport, StorageError> {
        let mut uncommitted_journal_entries_aborted = 0usize;
        let mut temp_container_files_cleaned = 0usize;

        let staging_dir = self.config.storage_root.join("staging");
        if staging_dir.exists() {
            ensure_not_symlink(&staging_dir)?;
            for entry in fs::read_dir(&staging_dir)? {
                let entry = entry?;
                let p = entry.path();
                ensure_not_symlink(&p)?;
                if p.is_dir() {
                    fs::remove_dir_all(&p)?;
                } else {
                    fs::remove_file(&p)?;
                }
                temp_container_files_cleaned += 1;
            }
        }

        let journal_dir = self.config.storage_root.join("journal");
        if journal_dir.exists() {
            ensure_not_symlink(&journal_dir)?;
            for entry in fs::read_dir(&journal_dir)? {
                let entry = entry?;
                let p = entry.path();
                ensure_not_symlink(&p)?;
                let Some(fname) = p.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if fname.ends_with(".tmp") {
                    fs::remove_file(&p)?;
                    temp_container_files_cleaned += 1;
                    continue;
                }
                if fname.ends_with(".node_tx") {
                    let content = fs::read_to_string(&p)?;
                    if content.contains("STATE=PREPARING") {
                        fs::remove_file(&p)?;
                        uncommitted_journal_entries_aborted += 1;
                    }
                }
            }
        }

        let objects_dir = self.config.storage_root.join("objects");
        let mut new_index = BTreeMap::new();
        let mut total_used_bytes_rediscovered = 0u64;

        if objects_dir.exists() {
            ensure_not_symlink(&objects_dir)?;
            for obj_entry in fs::read_dir(&objects_dir)? {
                let obj_entry = obj_entry?;
                let obj_path = obj_entry.path();
                ensure_not_symlink(&obj_path)?;
                if !obj_path.is_dir() {
                    continue;
                }
                for shard_entry in fs::read_dir(&obj_path)? {
                    let shard_entry = shard_entry?;
                    let sp = shard_entry.path();
                    ensure_not_symlink(&sp)?;
                    let Some(fname) = sp.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    if fname.ends_with(".tmp") {
                        fs::remove_file(&sp)?;
                        temp_container_files_cleaned += 1;
                        continue;
                    }
                    if fname.ends_with(".fspc") {
                        if let Ok(reader) = ShardContainerReader::open(&sp) {
                            let hdr = reader.header();
                            let summary = StoredShardSummary {
                                object_id: hdr.object_id.clone(),
                                shard_id: hdr.shard_id as usize,
                                version: hdr.version,
                                stripe_count: hdr.stripe_count,
                                container_size_bytes: hdr.total_file_size,
                                container_body_sha256: hdr.container_body_sha256.to_hex(),
                            };
                            total_used_bytes_rediscovered += hdr.total_file_size;
                            new_index
                                .insert((hdr.object_id.clone(), hdr.shard_id as usize), summary);
                        }
                    }
                }
            }
        }

        let valid_containers_rediscovered = new_index.len();
        *self.shard_index.write().unwrap_or_else(|p| p.into_inner()) = new_index;

        Ok(NodeCrashRecoveryReport {
            node_id: self.config.node_id,
            uncommitted_journal_entries_aborted,
            temp_container_files_cleaned,
            valid_containers_rediscovered,
            total_used_bytes_rediscovered,
        })
    }

    /// Computes total stored bytes and shard container count from the active index.
    pub fn current_usage(&self) -> (u64, usize) {
        let idx = self.shard_index.read().unwrap_or_else(|p| p.into_inner());
        let bytes: u64 = idx.values().map(|s| s.container_size_bytes).sum();
        (bytes, idx.len())
    }

    /// Executes a validated [`RpcRequest`] and optional binary `payload`, returning `(RpcResponse, response_payload)`.
    pub fn handle_rpc(
        &self,
        request: RpcRequest,
        payload: &[u8],
    ) -> Result<(RpcResponse, Vec<u8>), RpcError> {
        self.health
            .requests_processed
            .fetch_add(1, Ordering::Relaxed);
        match request {
            RpcRequest::PutShard(req) => {
                let ack = self.handle_put_shard(req, payload)?;
                Ok((RpcResponse::PutShard(ack), Vec::new()))
            }
            RpcRequest::PutShardFenced {
                request: req,
                epoch,
            } => {
                self.fencing_tracker.check_and_update(epoch)?;
                let ack = self.handle_put_shard(req, payload)?;
                Ok((RpcResponse::PutShard(ack), Vec::new()))
            }
            RpcRequest::GetShard(req) => {
                let (meta, bytes) = self.handle_get_shard(req)?;
                Ok((RpcResponse::GetShard(meta), bytes))
            }
            RpcRequest::DeleteShard {
                tx_id,
                object_id,
                shard_id,
                version,
            } => {
                let deleted = self.handle_delete_shard(&tx_id, &object_id, shard_id, version)?;
                Ok((RpcResponse::DeleteShard { deleted }, Vec::new()))
            }
            RpcRequest::DeleteShardFenced {
                tx_id,
                object_id,
                shard_id,
                version,
                epoch,
            } => {
                self.fencing_tracker.check_and_update(epoch)?;
                let deleted = self.handle_delete_shard(&tx_id, &object_id, shard_id, version)?;
                Ok((RpcResponse::DeleteShard { deleted }, Vec::new()))
            }
            RpcRequest::VerifyShard(req) => {
                let rep = self.handle_verify_shard(req)?;
                Ok((RpcResponse::VerifyShard(rep), Vec::new()))
            }
            RpcRequest::ListShards { object_id_filter } => {
                let list = self.handle_list_shards(object_id_filter.as_deref());
                Ok((RpcResponse::ListShards(list), Vec::new()))
            }
            RpcRequest::Heartbeat { sequence, .. } => {
                let (used_bytes, stored_shard_count) = self.current_usage();
                let hb = HeartbeatResponse {
                    node_id: self.config.node_id,
                    sequence,
                    health_state: self.health.health_state().as_str().to_string(),
                    used_bytes,
                    capacity_limit_bytes: self.config.capacity_limit_bytes,
                    stored_shard_count,
                    uptime_ms: self.health.uptime_ms(),
                };
                Ok((RpcResponse::Heartbeat(hb), Vec::new()))
            }
            RpcRequest::GetNodeStatus => {
                let (used_bytes, stored_shard_count) = self.current_usage();
                let st = NodeStatusResponse {
                    node_id: self.config.node_id,
                    listen_addr: self.config.listen_addr.clone(),
                    identity_id: self.config.identity.identity_id.clone(),
                    cert_fingerprint_hex: self.config.identity.cert_fingerprint_hex.clone(),
                    health_state: self.health.health_state().as_str().to_string(),
                    used_bytes,
                    capacity_limit_bytes: self.config.capacity_limit_bytes,
                    stored_shard_count,
                    requests_processed: self.health.requests_processed.load(Ordering::Relaxed),
                    auth_failures_rejected: self
                        .health
                        .auth_failures_rejected
                        .load(Ordering::Relaxed),
                    bytes_received: self.health.bytes_received.load(Ordering::Relaxed),
                    bytes_sent: self.health.bytes_sent.load(Ordering::Relaxed),
                };
                Ok((RpcResponse::GetNodeStatus(st), Vec::new()))
            }
        }
    }

    fn handle_put_shard(
        &self,
        req: PutShardRequest,
        payload: &[u8],
    ) -> Result<PutShardAck, RpcError> {
        self.verify_node_root()?;

        // 1. Check idempotency: if this tx_id already committed this shard on this node, return replay ack!
        if let Some(record) = self.idempotency_store.check_idempotency(
            &req.tx_id,
            req.shard_id,
            &req.object_id,
            req.version,
            payload,
        )? {
            if let Some(prev_ack) = record.ack {
                let mut replay = prev_ack;
                replay.idempotent_replay = true;
                return Ok(replay);
            }
        }

        // 2. Validate payload length matches sum of stripe shard_lengths
        let mut expected_payload_bytes = 0usize;
        for &(_, slen, _) in &req.stripes {
            expected_payload_bytes = expected_payload_bytes
                .checked_add(slen)
                .ok_or_else(|| RpcError::SerializationError("payload length overflow".into()))?;
        }
        if payload.len() != expected_payload_bytes {
            return Err(RpcError::IntegrityFailure(format!(
                "PutShard payload length {} != expected {}",
                payload.len(),
                expected_payload_bytes
            )));
        }

        // 3. Check capacity limit admission (accounting for replacing an existing version of the same shard)
        let estimated_container_bytes =
            256u64 + (payload.len() as u64) + (req.stripes.len() as u64) * 52;
        let (used_bytes, _) = self.current_usage();
        let existing_shard_bytes = self
            .shard_index
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(&(req.object_id.clone(), req.shard_id))
            .map(|s| s.container_size_bytes)
            .unwrap_or(0);
        let projected = used_bytes
            .saturating_sub(existing_shard_bytes)
            .saturating_add(estimated_container_bytes);
        if projected > self.config.capacity_limit_bytes {
            return Err(RpcError::QuotaExceeded(format!(
                "node-{:02} capacity exceeded: used={} B, requested={} B, limit={} B",
                self.config.node_id.0,
                used_bytes,
                estimated_container_bytes,
                self.config.capacity_limit_bytes
            )));
        }

        // 4. Write local node transaction journal record in PREPARING state
        let journal_path = self
            .config
            .storage_root
            .join("journal")
            .join(format!("{}-{:02}.node_tx", req.tx_id, req.shard_id));
        let prep_record = format!(
            "STATE=PREPARING\ntx_id={}\nobject_id={}\nshard_id={}\nversion={}\n",
            req.tx_id, req.object_id, req.shard_id, req.version
        );
        write_and_sync_file(&journal_path, prep_record.as_bytes())
            .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        self.health.fsync_count.fetch_add(1, Ordering::Relaxed);

        // 5. Stream stripes into staged v2 container file and verify each stripe's SHA-256
        let staged_path = self.config.storage_root.join("staging").join(format!(
            "{}-{}-shard-{:02}.v{}.fspc.tmp",
            req.tx_id, req.object_id, req.shard_id, req.version
        ));
        let mut writer = ShardContainerWriter::create(
            &staged_path,
            &req.object_id,
            req.coding_scheme,
            req.shard_id,
            req.version,
        )
        .map_err(|e| RpcError::IntegrityFailure(e.to_string()))?;

        let mut offset = 0usize;
        for (s_idx, (plen, slen, expected_sha)) in req.stripes.iter().enumerate() {
            let slice = &payload[offset..offset + *slen];
            offset += *slen;
            let actual_sha = sha256_bytes(slice).to_hex();
            if &actual_sha != expected_sha {
                let _ = fs::remove_file(&staged_path);
                let _ = fs::remove_file(&journal_path);
                return Err(RpcError::IntegrityFailure(format!(
                    "PutShard stripe {} SHA-256 mismatch: expected {}, got {}",
                    s_idx, expected_sha, actual_sha
                )));
            }
            writer
                .append_stripe(s_idx, *plen, slice)
                .map_err(|e| RpcError::IntegrityFailure(e.to_string()))?;
        }

        let (_, hdr, _) = writer
            .finalize()
            .map_err(|e| RpcError::IntegrityFailure(e.to_string()))?;
        self.health.fsync_count.fetch_add(1, Ordering::Relaxed);

        // 6. Re-open staged container on disk and verify before atomic publication
        {
            let mut verifier = ShardContainerReader::open(&staged_path)
                .map_err(|e| RpcError::IntegrityFailure(e.to_string()))?;
            verifier
                .verify_all()
                .map_err(|e| RpcError::IntegrityFailure(e.to_string()))?;
        }

        // 7. Atomically publish staged container to `<node_root>/objects/<object_id>/shard-YY.vZ.fspc`
        let target_path = self.resolve_container_path(&req.object_id, req.shard_id, req.version)?;
        if let Some(parent) = target_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)
                    .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
                if let Some(grandparent) = parent.parent() {
                    sync_directory(grandparent)
                        .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
                }
            }
        }
        fs::rename(&staged_path, &target_path)
            .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        if let Some(parent) = target_path.parent() {
            sync_directory(parent).map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
            self.health.fsync_count.fetch_add(1, Ordering::Relaxed);
        }

        // 8. Mark journal COMMITTED and update in-memory shard index & idempotency cache
        let commit_record = format!(
            "STATE=COMMITTED\ntx_id={}\nobject_id={}\nshard_id={}\nversion={}\nsize={}\n",
            req.tx_id, req.object_id, req.shard_id, req.version, hdr.total_file_size
        );
        write_and_sync_file(&journal_path, commit_record.as_bytes())
            .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        self.health.fsync_count.fetch_add(1, Ordering::Relaxed);

        let summary = StoredShardSummary {
            object_id: req.object_id.clone(),
            shard_id: req.shard_id,
            version: req.version,
            stripe_count: hdr.stripe_count,
            container_size_bytes: hdr.total_file_size,
            container_body_sha256: hdr.container_body_sha256.to_hex(),
        };
        self.shard_index
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert((req.object_id.clone(), req.shard_id), summary);

        let ack = PutShardAck {
            node_id: self.config.node_id,
            tx_id: req.tx_id.clone(),
            object_id: req.object_id.clone(),
            shard_id: req.shard_id,
            version: req.version,
            stripe_count: hdr.stripe_count,
            container_size_bytes: hdr.total_file_size,
            container_body_sha256: hdr.container_body_sha256.to_hex(),
            header_and_index_sha256: hdr.header_and_index_sha256.to_hex(),
            idempotent_replay: false,
            durability_level: DurabilityLevel::LocallyDurable,
        };
        self.idempotency_store.record_put_shard(
            &req.tx_id,
            &req.object_id,
            req.shard_id,
            req.version,
            payload,
            ack.clone(),
            0,
        )?;

        Ok(ack)
    }

    fn handle_get_shard(
        &self,
        req: GetShardRequest,
    ) -> Result<(GetShardResponseMeta, Vec<u8>), RpcError> {
        let path = self.resolve_container_path(&req.object_id, req.shard_id, req.version)?;
        if !path.exists() {
            return Err(RpcError::ShardNotFound {
                object_id: req.object_id,
                shard_id: req.shard_id,
                version: req.version,
            });
        }
        let mut reader = ShardContainerReader::open(&path)
            .map_err(|e| RpcError::IntegrityFailure(e.to_string()))?;

        match req.stripe_index {
            Some(idx) => {
                let bytes = reader
                    .read_stripe(idx)
                    .map_err(|e| RpcError::IntegrityFailure(e.to_string()))?;
                let sha = sha256_bytes(&bytes).to_hex();
                let meta = GetShardResponseMeta {
                    node_id: self.config.node_id,
                    object_id: req.object_id,
                    shard_id: req.shard_id,
                    version: req.version,
                    stripes: vec![(idx, bytes.len(), sha)],
                };
                Ok((meta, bytes))
            }
            None => {
                let stripe_count = reader.entries().len();
                let mut stripes = Vec::with_capacity(stripe_count);
                let mut all_bytes = Vec::new();
                for idx in 0..stripe_count {
                    let bytes = reader
                        .read_stripe(idx)
                        .map_err(|e| RpcError::IntegrityFailure(e.to_string()))?;
                    let sha = sha256_bytes(&bytes).to_hex();
                    stripes.push((idx, bytes.len(), sha));
                    all_bytes.extend_from_slice(&bytes);
                }
                let meta = GetShardResponseMeta {
                    node_id: self.config.node_id,
                    object_id: req.object_id,
                    shard_id: req.shard_id,
                    version: req.version,
                    stripes,
                };
                Ok((meta, all_bytes))
            }
        }
    }

    fn handle_delete_shard(
        &self,
        tx_id: &str,
        object_id: &str,
        shard_id: usize,
        version: u64,
    ) -> Result<bool, RpcError> {
        if let Some(record) =
            self.idempotency_store
                .check_idempotency(tx_id, shard_id, object_id, version, &[])?
        {
            if let Some(prev_deleted) = record.deleted {
                return Ok(prev_deleted);
            }
        }

        let path = self.resolve_container_path(object_id, shard_id, version)?;
        let existed = if path.exists() {
            fs::remove_file(&path).map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
            if let Some(parent) = path.parent() {
                let _ = sync_directory(parent);
            }
            true
        } else {
            false
        };
        self.shard_index
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&(object_id.to_string(), shard_id));

        self.idempotency_store
            .record_delete_shard(tx_id, object_id, shard_id, version, existed, 0)?;

        Ok(existed)
    }

    fn handle_verify_shard(&self, req: VerifyShardRequest) -> Result<VerifyShardReport, RpcError> {
        let path = self.resolve_container_path(&req.object_id, req.shard_id, req.version)?;
        if !path.exists() {
            return Ok(VerifyShardReport {
                node_id: self.config.node_id,
                object_id: req.object_id,
                shard_id: req.shard_id,
                version: req.version,
                healthy: false,
                stripe_count: 0,
                container_size_bytes: 0,
                container_body_sha256: String::new(),
                challenge_response_sha256: String::new(),
                error_reason: Some("shard container missing on disk".to_string()),
            });
        }

        let mut reader = match ShardContainerReader::open(&path) {
            Ok(r) => r,
            Err(e) => {
                return Ok(VerifyShardReport {
                    node_id: self.config.node_id,
                    object_id: req.object_id,
                    shard_id: req.shard_id,
                    version: req.version,
                    healthy: false,
                    stripe_count: 0,
                    container_size_bytes: 0,
                    container_body_sha256: String::new(),
                    challenge_response_sha256: String::new(),
                    error_reason: Some(e.to_string()),
                });
            }
        };

        if let Err(e) = reader.verify_all() {
            return Ok(VerifyShardReport {
                node_id: self.config.node_id,
                object_id: req.object_id,
                shard_id: req.shard_id,
                version: req.version,
                healthy: false,
                stripe_count: reader.header().stripe_count,
                container_size_bytes: reader.header().total_file_size,
                container_body_sha256: reader.header().container_body_sha256.to_hex(),
                challenge_response_sha256: String::new(),
                error_reason: Some(e.to_string()),
            });
        }

        // Compute keyed SHA-256 challenge-response over the full on-disk container bytes
        let mut raw_file =
            open_file_nofollow(&path).map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        let mut challenge_hasher = Sha256Hasher::new();
        challenge_hasher
            .update(format!("FSP-SCRUB-CHALLENGE-V1:{}:", req.challenge_nonce_hex).as_bytes());
        let mut buf = [0u8; 65_536];
        loop {
            use std::io::Read;
            let n = raw_file
                .read(&mut buf)
                .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
            if n == 0 {
                break;
            }
            challenge_hasher.update(&buf[..n]);
        }

        Ok(VerifyShardReport {
            node_id: self.config.node_id,
            object_id: req.object_id,
            shard_id: req.shard_id,
            version: req.version,
            healthy: true,
            stripe_count: reader.header().stripe_count,
            container_size_bytes: reader.header().total_file_size,
            container_body_sha256: reader.header().container_body_sha256.to_hex(),
            challenge_response_sha256: challenge_hasher.finalize().to_hex(),
            error_reason: None,
        })
    }

    fn handle_list_shards(&self, object_id_filter: Option<&str>) -> Vec<StoredShardSummary> {
        let idx = self.shard_index.read().unwrap_or_else(|p| p.into_inner());
        idx.values()
            .filter(|s| match object_id_filter {
                Some(f) => s.object_id == f,
                None => true,
            })
            .cloned()
            .collect()
    }
}
