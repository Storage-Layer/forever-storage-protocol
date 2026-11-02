//! Primary Phase 3 persistent [`StorageEngine`] orchestrating bounded-memory streaming ingestion,
//! crash-safe atomic transactions, verification, reconstruction, and adaptive repair.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::coding::codec::ErasureCodec;
use crate::coding::stripe::{MAX_STRIPE_SIZE_BYTES, MIN_STRIPE_SIZE_BYTES};
use crate::detection::ClusterObservationView;
use crate::model::node::NodeId;
use crate::model::shard::ShardId;
use crate::recovery::reconstruct::{
    reconstruct_object_to_file, reconstruct_object_to_writer, ReconstructionReport,
};
use crate::recovery::recovery_manager::{recover_storage_state, CrashRecoveryReport};
use crate::recovery::repair::{
    execute_object_repair, inspect_object_health, AdaptiveStorageRepairPlanner, ObjectHealthReport,
    RepairExecutionReport,
};
use crate::storage::backend::StorageError;
use crate::storage::container::ShardContainerWriter;
use crate::storage::filesystem::FilesystemBackend;
use crate::storage::integrity::{sha256_bytes, Sha256Hasher};
use crate::storage::manifest::{
    validate_safe_identifier, ManifestCodingSpec, ManifestValidationError, ObjectManifest,
    ShardPlacementRecord, StripeManifest, MANIFEST_FORMAT_VERSION,
};
use crate::storage::object::{read_stripe_chunk, ClusterStorageStatus, PutOptions, PutReceipt};
use crate::storage::transaction::WriteTransaction;
use crate::strategies::adaptive_v2::BlindRepairDecision;

static TX_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Persistent FSP storage engine backed by a sandboxed local filesystem directory hierarchy.
#[derive(Debug, Clone)]
pub struct StorageEngine {
    backend: FilesystemBackend,
}

impl StorageEngine {
    /// Initializes a new FSP storage root at `root` with `num_nodes` storage directories
    /// mapped across `num_domains` logical failure domains.
    pub fn init(root: &Path, num_nodes: usize, num_domains: usize) -> Result<Self, StorageError> {
        let backend = FilesystemBackend::init_root(root, num_nodes, num_domains)?;
        Ok(Self { backend })
    }

    /// Opens an existing initialized FSP storage root and runs idempotent startup crash recovery.
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        let backend = FilesystemBackend::open_existing(root)?;
        let _ = recover_storage_state(&backend)?;
        Ok(Self { backend })
    }

    /// Opens an existing initialized FSP storage root without automatically running crash recovery
    /// (useful for inspecting raw interrupted transaction state before calling `recover()`).
    pub fn open_without_recovery(root: &Path) -> Result<Self, StorageError> {
        let backend = FilesystemBackend::open_existing(root)?;
        Ok(Self { backend })
    }

    /// Runs idempotent crash recovery on the storage root and returns a detailed [`CrashRecoveryReport`].
    pub fn recover(&self) -> Result<CrashRecoveryReport, StorageError> {
        recover_storage_state(&self.backend)
    }

    /// Returns an immutable reference to the underlying [`FilesystemBackend`].
    pub const fn backend(&self) -> &FilesystemBackend {
        &self.backend
    }

    /// Returns a mutable reference to the underlying [`FilesystemBackend`].
    pub fn backend_mut(&mut self) -> &mut FilesystemBackend {
        &mut self.backend
    }

    /// Streams an arbitrary binary object from `reader` in bounded-memory stripes (`options.stripe_size`),
    /// erasure-codes each stripe, stages and verifies all shard files, and atomically commits the manifest.
    pub fn put_reader<R: Read>(
        &mut self,
        mut reader: R,
        options: &PutOptions,
    ) -> Result<PutReceipt, StorageError> {
        if options.stripe_size < MIN_STRIPE_SIZE_BYTES
            || options.stripe_size > MAX_STRIPE_SIZE_BYTES
        {
            return Err(StorageError::Manifest(
                ManifestValidationError::InvalidStripeSize(options.stripe_size),
            ));
        }

        if let Some(ref explicit_id) = options.object_id {
            if !validate_safe_identifier(explicit_id) {
                return Err(StorageError::Manifest(
                    ManifestValidationError::InvalidObjectId(explicit_id.clone()),
                ));
            }
        }

        let codec = ErasureCodec::from_scheme(options.coding_scheme)?;
        let n = codec.total_shards();
        if n > self.backend.num_nodes() {
            return Err(StorageError::Manifest(
                ManifestValidationError::InvalidCodingSpec(format!(
                    "coding total_shards n={} exceeds storage cluster num_nodes={}",
                    n,
                    self.backend.num_nodes()
                )),
            ));
        }

        let seq = TX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let tx_id = format!("tx-{}-{:08}", pid, seq);
        let initial_obj_id = options
            .object_id
            .clone()
            .unwrap_or_else(|| format!("pending-{}", tx_id));

        let mut tx = WriteTransaction::begin(
            &self.backend,
            &tx_id,
            &initial_obj_id,
            options.crash_trigger,
        )?;
        tx.enter_writing()?;

        let mut object_hasher = Sha256Hasher::new();
        let mut object_size = 0u64;
        let mut stripe_manifests = Vec::new();
        let mut total_shards_written = 0usize;
        let mut total_disk_bytes_written = 0u64;
        let mut per_node_requested_bytes: BTreeMap<NodeId, u64> = BTreeMap::new();

        let mut container_writers: Vec<ShardContainerWriter> = if options.use_container_v2 {
            let mut writers = Vec::with_capacity(n);
            for shard_id in 0..n {
                let staged_path = tx
                    .staging_dir_path()
                    .join(format!("shard-{:02}.v1.fspc.tmp", shard_id));
                let w = ShardContainerWriter::create(
                    &staged_path,
                    &initial_obj_id,
                    options.coding_scheme,
                    shard_id,
                    1,
                )?;
                writers.push(w);
            }
            writers
        } else {
            Vec::new()
        };

        let mut stripe_index = 0usize;
        loop {
            let chunk = read_stripe_chunk(&mut reader, options.stripe_size)?;
            if chunk.is_empty() && stripe_index > 0 {
                break;
            }

            let payload_offset = object_size;
            let payload_length = chunk.len();
            object_hasher.update(&chunk);
            object_size += payload_length as u64;

            let (shard_length, encoded_shards) = codec.encode_stripe_payload(&chunk)?;
            let mut placements = Vec::with_capacity(n);

            for (shard_id, shard_bytes) in encoded_shards.into_iter().enumerate() {
                let node_id = NodeId(shard_id % self.backend.num_nodes());
                let domain_id = self.backend.domain_for_node(node_id);
                let size_bytes = shard_bytes.len() as u64;

                let entry = per_node_requested_bytes.entry(node_id).or_insert(0);
                *entry += size_bytes;
                if let Err(e) = self.backend.check_node_write_admission(node_id, *entry) {
                    let _ = tx.abort();
                    return Err(e);
                }

                let sha256_hex = sha256_bytes(&shard_bytes).to_hex();
                let rel_path = if options.use_container_v2 {
                    if let Err(e) = container_writers[shard_id].append_stripe(
                        stripe_index,
                        payload_length,
                        &shard_bytes,
                    ) {
                        let _ = tx.abort();
                        return Err(StorageError::Container(e));
                    }
                    self.backend.canonical_container_relative_path(
                        node_id,
                        &initial_obj_id,
                        ShardId(shard_id),
                        1,
                    )?
                } else {
                    let rp = self.backend.canonical_shard_relative_path(
                        node_id,
                        &initial_obj_id,
                        stripe_index,
                        ShardId(shard_id),
                        1,
                    )?;
                    if let Err(e) = tx.stage_shard(
                        stripe_index,
                        shard_id,
                        &shard_bytes,
                        sha256_hex.clone(),
                        rp.clone(),
                    ) {
                        let _ = tx.abort();
                        return Err(e);
                    }
                    total_shards_written += 1;
                    total_disk_bytes_written += size_bytes;
                    rp
                };

                placements.push(ShardPlacementRecord {
                    shard_id,
                    node_id: node_id.0,
                    domain_id: domain_id.0,
                    version: 1,
                    size_bytes,
                    sha256_digest: sha256_hex,
                    relative_path: rel_path,
                });
            }

            stripe_manifests.push(StripeManifest {
                stripe_index,
                payload_offset,
                payload_length,
                shard_length,
                placements,
            });

            stripe_index += 1;
            if payload_length < options.stripe_size {
                break;
            }
        }

        let object_digest = object_hasher.finalize().to_hex();
        let final_object_id = match &options.object_id {
            Some(id) => id.clone(),
            None => format!("obj-{}", &object_digest[..32]),
        };

        if options.use_container_v2 {
            for (shard_id, mut writer) in container_writers.into_iter().enumerate() {
                if let Err(e) = writer.set_final_object_id(&final_object_id) {
                    let _ = tx.abort();
                    return Err(StorageError::Container(e));
                }
                let (staged_path, hdr, _) = match writer.finalize() {
                    Ok(res) => res,
                    Err(e) => {
                        let _ = tx.abort();
                        return Err(StorageError::Container(e));
                    }
                };
                let node_id = NodeId(shard_id % self.backend.num_nodes());
                let target_rel = self.backend.canonical_container_relative_path(
                    node_id,
                    &final_object_id,
                    ShardId(shard_id),
                    1,
                )?;
                tx.register_staged_container(
                    shard_id,
                    staged_path,
                    target_rel,
                    hdr.total_file_size,
                    hdr.container_body_sha256.to_hex(),
                );
                total_shards_written += 1;
                total_disk_bytes_written += hdr.total_file_size;
            }
        }

        if final_object_id != initial_obj_id {
            tx.set_final_object_id(&final_object_id)?;
            for s in &mut stripe_manifests {
                for p in &mut s.placements {
                    p.relative_path = if options.use_container_v2 {
                        self.backend.canonical_container_relative_path(
                            NodeId(p.node_id),
                            &final_object_id,
                            ShardId(p.shard_id),
                            p.version,
                        )?
                    } else {
                        self.backend.canonical_shard_relative_path(
                            NodeId(p.node_id),
                            &final_object_id,
                            s.stripe_index,
                            ShardId(p.shard_id),
                            p.version,
                        )?
                    };
                }
            }
        }

        tx.verify_staged_shards()?;

        let mut manifest = ObjectManifest {
            format_version: MANIFEST_FORMAT_VERSION,
            object_id: final_object_id.clone(),
            object_size,
            stripe_size: options.stripe_size,
            coding: ManifestCodingSpec::from_coding_scheme(options.coding_scheme),
            integrity_algorithm: "sha256".to_string(),
            object_digest: object_digest.clone(),
            stripes: stripe_manifests,
            manifest_digest: String::new(),
        };
        manifest.seal();

        {
            let lock_holder = self.backend.acquire_object_write_lock(&final_object_id);
            let _write_guard = lock_holder.write();
            tx.commit(&manifest)?;
        }

        Ok(PutReceipt {
            object_id: final_object_id,
            object_size,
            object_digest,
            stripe_count: manifest.stripes.len(),
            total_shards_written,
            total_disk_bytes_written,
            tx_id,
            manifest,
        })
    }

    /// Migrates an existing Phase 3 v1 object (`.fsp` per-stripe shard files) into Phase 4 v2
    /// multi-stripe shard containers (`.fspc`) atomically, preserving `object_id` and verifying
    /// every stripe before replacing the manifest and removing old `.fsp` files.
    pub fn migrate_object_v1_to_v2(&mut self, object_id: &str) -> Result<PutReceipt, StorageError> {
        let old_manifest = self.load_manifest(object_id)?;
        let scheme = old_manifest.validate()?;
        let (reconstructed_bytes, _) = self.get_to_bytes(object_id)?;
        let v2_opts = PutOptions {
            object_id: Some(object_id.to_string()),
            stripe_size: old_manifest.stripe_size,
            coding_scheme: scheme,
            crash_trigger: None,
            use_container_v2: true,
        };
        let receipt = self.put_bytes(&reconstructed_bytes, &v2_opts)?;

        // Clean up old v1 `.fsp` files after the v2 manifest and containers are durably committed
        for stripe in &old_manifest.stripes {
            for p in &stripe.placements {
                if p.relative_path.ends_with(".fsp") {
                    if let Ok(full) = self.backend.resolve_safe_relative_path(&p.relative_path) {
                        if full.exists() {
                            let _ = fs::remove_file(&full);
                        }
                    }
                }
            }
        }
        Ok(receipt)
    }

    /// Streams a file from `input_path` into the FSP storage engine.
    pub fn put_file(
        &mut self,
        input_path: &Path,
        options: &PutOptions,
    ) -> Result<PutReceipt, StorageError> {
        let file = File::open(input_path)?;
        let reader = BufReader::with_capacity(256 * 1024, file);
        self.put_reader(reader, options)
    }

    /// Stores an in-memory byte slice into the FSP storage engine.
    pub fn put_bytes(
        &mut self,
        bytes: &[u8],
        options: &PutOptions,
    ) -> Result<PutReceipt, StorageError> {
        self.put_reader(Cursor::new(bytes), options)
    }

    /// Loads and validates the committed [`ObjectManifest`] for `object_id`.
    pub fn load_manifest(&self, object_id: &str) -> Result<ObjectManifest, StorageError> {
        let path = self.backend.manifest_path(object_id)?;
        if !path.exists() {
            return Err(StorageError::ObjectNotFound(object_id.to_string()));
        }
        let json = fs::read_to_string(&path)?;
        let manifest = ObjectManifest::from_json(&json)?;
        Ok(manifest)
    }

    /// Reconstructs `object_id` and streams its verified original payload into `writer`.
    pub fn get_to_writer<W: Write>(
        &self,
        object_id: &str,
        writer: W,
    ) -> Result<ReconstructionReport, StorageError> {
        let manifest = self.load_manifest(object_id)?;
        reconstruct_object_to_writer(&self.backend, &manifest, writer)
    }

    /// Reconstructs `object_id` and writes its verified original payload to `output_path` atomically.
    pub fn get_to_file(
        &self,
        object_id: &str,
        output_path: &Path,
    ) -> Result<ReconstructionReport, StorageError> {
        let manifest = self.load_manifest(object_id)?;
        reconstruct_object_to_file(&self.backend, &manifest, output_path)
    }

    /// Reconstructs `object_id` into an in-memory byte vector.
    pub fn get_to_bytes(
        &self,
        object_id: &str,
    ) -> Result<(Vec<u8>, ReconstructionReport), StorageError> {
        let manifest = self.load_manifest(object_id)?;
        let mut out = Vec::with_capacity(manifest.object_size as usize);
        let report = reconstruct_object_to_writer(&self.backend, &manifest, &mut out)?;
        Ok((out, report))
    }

    /// Audits the integrity and recoverability of every shard of `object_id` on disk.
    pub fn verify_object(&self, object_id: &str) -> Result<ObjectHealthReport, StorageError> {
        let manifest = self.load_manifest(object_id)?;
        inspect_object_health(&self.backend, &manifest)
    }

    /// Loads the validated manifest and full health report for `object_id`.
    pub fn inspect_object(
        &self,
        object_id: &str,
    ) -> Result<(ObjectManifest, ObjectHealthReport), StorageError> {
        let manifest = self.load_manifest(object_id)?;
        let health = inspect_object_health(&self.backend, &manifest)?;
        Ok((manifest, health))
    }

    /// Repairs all missing or corrupted shards of `object_id`, verifying every reconstructed shard
    /// before and after atomic publication.
    pub fn repair_object(&self, object_id: &str) -> Result<RepairExecutionReport, StorageError> {
        let manifest = self.load_manifest(object_id)?;
        execute_object_repair(&self.backend, &manifest, None, false)
    }

    /// Plans and executes adaptive repair of `object_id` using the Phase 2 [`AdaptiveStorageRepairPlanner`]
    /// and blind [`ClusterObservationView`].
    pub fn repair_object_adaptive(
        &self,
        object_id: &str,
        planner: &mut AdaptiveStorageRepairPlanner,
        now_secs: f64,
        observation_view: Option<&ClusterObservationView>,
        in_flight_per_stripe: Option<&BTreeMap<usize, Vec<(ShardId, bool)>>>,
        effective_bandwidth_bps: u64,
    ) -> Result<(Vec<BlindRepairDecision>, RepairExecutionReport), StorageError> {
        let manifest = self.load_manifest(object_id)?;
        let health = inspect_object_health(&self.backend, &manifest)?;

        let mut decisions = Vec::with_capacity(health.stripes.len());
        let mut selected_map: BTreeMap<usize, Vec<ShardId>> = BTreeMap::new();

        for stripe_rep in &health.stripes {
            let in_flight = in_flight_per_stripe
                .and_then(|m| m.get(&stripe_rep.stripe_index))
                .map(|v| v.as_slice())
                .unwrap_or(&[]);

            let decision = planner.plan_stripe_repair(
                now_secs,
                &manifest,
                stripe_rep,
                observation_view,
                in_flight,
                effective_bandwidth_bps,
            );
            selected_map.insert(stripe_rep.stripe_index, decision.shards_to_repair.clone());
            decisions.push(decision);
        }

        let exec_report =
            execute_object_repair(&self.backend, &manifest, Some(&selected_map), false)?;
        Ok((decisions, exec_report))
    }

    /// Returns a summary of cluster node statuses, capacities, and committed objects.
    pub fn status(&self) -> Result<ClusterStorageStatus, StorageError> {
        let nodes = self.backend.all_node_statuses()?;
        let num_online_nodes = nodes.iter().filter(|n| n.is_online).count();
        let total_stored_shard_bytes = nodes.iter().map(|n| n.used_bytes).sum();
        let total_stored_shard_files = nodes.iter().map(|n| n.shard_file_count).sum();
        let committed_objects = self.backend.list_objects()?;

        Ok(ClusterStorageStatus {
            root_path: self.backend.root().display().to_string(),
            num_nodes: self.backend.num_nodes(),
            num_online_nodes,
            num_domains: self.backend.num_domains(),
            committed_objects,
            total_stored_shard_bytes,
            total_stored_shard_files,
            nodes,
        })
    }
}
