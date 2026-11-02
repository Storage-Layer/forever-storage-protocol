//! Verifying, bounded-memory streaming object reconstruction from surviving erasure-coded shards.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use crate::coding::codec::ErasureCodec;
use crate::model::node::NodeId;
use crate::model::shard::ShardId;
use crate::storage::backend::StorageError;
use crate::storage::container::ShardContainerReader;
use crate::storage::filesystem::{sync_directory, FilesystemBackend};
use crate::storage::integrity::{sha256_bytes, Sha256Hasher};
use crate::storage::manifest::{ObjectManifest, ShardPlacementRecord};

/// Detailed telemetry from an object reconstruction (`GET`) operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructionReport {
    /// Reconstructed object ID.
    pub object_id: String,
    /// Total object payload bytes written.
    pub object_size: u64,
    /// Verified SHA-256 hex digest of the reconstructed object.
    pub object_digest: String,
    /// Number of stripes processed.
    pub stripes_processed: usize,
    /// Total shard files verified and read from disk.
    pub shards_read: usize,
    /// Total physical disk bytes read across all stripes.
    pub disk_bytes_read: u64,
    /// Number of corrupt, truncated, or stale shard candidates rejected before decoding.
    pub corrupt_or_stale_shards_rejected: usize,
    /// Number of missing or offline shards encountered.
    pub missing_or_offline_shards: usize,
}

/// Loads and validates a single shard candidate from disk, returning `Ok(Some(bytes))` if and only if:
/// 1. The hosting node is online,
/// 2. The canonical versioned shard file exists, has exact byte length `placement.size_bytes`,
///    and its SHA-256 digest matches `placement.sha256_digest`.
///
/// Also inspects the parent directory for any stale or conflicting duplicate version files
/// (`stripe-XXXXXX.shard-YY.v*.fsp`) and counts rejected invalid candidates.
pub fn read_and_verify_shard_placement(
    backend: &FilesystemBackend,
    object_id: &str,
    stripe_index: usize,
    placement: &ShardPlacementRecord,
    corrupt_rejected_counter: &mut usize,
) -> Result<Option<Vec<u8>>, StorageError> {
    let node_id = NodeId(placement.node_id);
    if !backend.is_node_online(node_id) {
        return Ok(None);
    }

    // Phase 4 v2 Multi-Stripe Container format (`.fspc`)
    if placement.relative_path.ends_with(".fspc") {
        let prefix = format!("shard-{:02}.v", placement.shard_id);
        let canonical_filename = format!(
            "shard-{:02}.v{}.fspc",
            placement.shard_id, placement.version
        );
        if let Ok(obj_dir) = backend.node_object_dir(node_id, object_id) {
            if obj_dir.exists() {
                if let Ok(entries) = fs::read_dir(&obj_dir) {
                    for entry in entries.flatten() {
                        if let Some(fname) = entry.file_name().to_str() {
                            if fname.starts_with(&prefix)
                                && fname.ends_with(".fspc")
                                && fname != canonical_filename
                            {
                                *corrupt_rejected_counter += 1;
                            }
                        }
                    }
                }
            }
        }

        let full_path = backend.resolve_safe_relative_path(&placement.relative_path)?;
        if !full_path.exists() {
            return Ok(None);
        }
        let mut reader = match ShardContainerReader::open(&full_path) {
            Ok(r) => r,
            Err(_) => {
                *corrupt_rejected_counter += 1;
                return Ok(None);
            }
        };
        if reader.header().object_id != object_id
            || (reader.header().shard_id as usize) != placement.shard_id
            || reader.header().version != placement.version
        {
            *corrupt_rejected_counter += 1;
            return Ok(None);
        }
        match reader.read_stripe(stripe_index) {
            Ok(bytes) => {
                if (bytes.len() as u64) != placement.size_bytes {
                    *corrupt_rejected_counter += 1;
                    return Ok(None);
                }
                let actual_digest = sha256_bytes(&bytes).to_hex();
                if actual_digest != placement.sha256_digest {
                    *corrupt_rejected_counter += 1;
                    return Ok(None);
                }
                return Ok(Some(bytes));
            }
            Err(_) => {
                *corrupt_rejected_counter += 1;
                return Ok(None);
            }
        }
    }

    // Check if any conflicting/stale version files exist in the node's object directory for this (stripe, shard)
    let prefix = format!(
        "stripe-{:06}.shard-{:02}.v",
        stripe_index, placement.shard_id
    );
    let canonical_filename = format!(
        "stripe-{:06}.shard-{:02}.v{}.fsp",
        stripe_index, placement.shard_id, placement.version
    );

    if let Ok(obj_dir) = backend.node_object_dir(node_id, object_id) {
        if obj_dir.exists() {
            if let Ok(entries) = fs::read_dir(&obj_dir) {
                for entry in entries.flatten() {
                    if let Some(fname) = entry.file_name().to_str() {
                        if fname.starts_with(&prefix)
                            && fname.ends_with(".fsp")
                            && fname != canonical_filename
                        {
                            // Stale or conflicting shard version detected and rejected
                            *corrupt_rejected_counter += 1;
                        }
                    }
                }
            }
        }
    }

    let full_path = backend.resolve_safe_relative_path(&placement.relative_path)?;
    if !full_path.exists() {
        return Ok(None);
    }

    let meta = fs::metadata(&full_path)?;
    if meta.len() != placement.size_bytes {
        *corrupt_rejected_counter += 1;
        return Ok(None);
    }

    let bytes = fs::read(&full_path)?;
    if (bytes.len() as u64) != placement.size_bytes {
        *corrupt_rejected_counter += 1;
        return Ok(None);
    }

    let actual_digest = sha256_bytes(&bytes).to_hex();
    if actual_digest != placement.sha256_digest {
        *corrupt_rejected_counter += 1;
        return Ok(None);
    }

    Ok(Some(bytes))
}

/// Reconstructs an object stripe-by-stripe into `writer`, verifying every shard's SHA-256 prior to
/// decoding and verifying the full reconstructed object SHA-256 at completion.
pub fn reconstruct_object_to_writer<W: Write>(
    backend: &FilesystemBackend,
    manifest: &ObjectManifest,
    mut writer: W,
) -> Result<ReconstructionReport, StorageError> {
    let _read_guard_holder = backend.acquire_object_read_lock(&manifest.object_id);
    let _read_guard = _read_guard_holder.read();

    let scheme = manifest.validate()?;
    let codec = ErasureCodec::from_scheme(scheme)?;
    let k = codec.data_shards();

    let mut object_hasher = Sha256Hasher::new();
    let mut total_written = 0u64;
    let mut shards_read = 0usize;
    let mut disk_bytes_read = 0u64;
    let mut corrupt_or_stale_shards_rejected = 0usize;
    let mut missing_or_offline_shards = 0usize;

    // Cache opened v2 container readers across stripes so each container is opened and header-validated once
    let mut container_cache: BTreeMap<String, Option<ShardContainerReader>> = BTreeMap::new();

    for stripe in &manifest.stripes {
        let mut verified_shards: Vec<(ShardId, Vec<u8>)> =
            Vec::with_capacity(stripe.placements.len());
        let mut verified_ids: BTreeSet<ShardId> = BTreeSet::new();

        for placement in &stripe.placements {
            let is_v2 = placement.relative_path.ends_with(".fspc");
            if is_v2 && verified_shards.len() >= k && codec.can_decode(&verified_ids) {
                break;
            }

            let prev_corrupt = corrupt_or_stale_shards_rejected;
            let shard_opt = if is_v2 {
                let node_id = NodeId(placement.node_id);
                if !backend.is_node_online(node_id) {
                    None
                } else {
                    if !container_cache.contains_key(&placement.relative_path) {
                        let full_path =
                            backend.resolve_safe_relative_path(&placement.relative_path)?;
                        if !full_path.exists() {
                            container_cache.insert(placement.relative_path.clone(), None);
                        } else {
                            match ShardContainerReader::open(&full_path) {
                                Ok(r)
                                    if r.header().object_id == manifest.object_id
                                        && (r.header().shard_id as usize) == placement.shard_id
                                        && r.header().version == placement.version =>
                                {
                                    container_cache
                                        .insert(placement.relative_path.clone(), Some(r));
                                }
                                _ => {
                                    corrupt_or_stale_shards_rejected += 1;
                                    container_cache.insert(placement.relative_path.clone(), None);
                                }
                            }
                        }
                    }
                    match container_cache.get_mut(&placement.relative_path) {
                        Some(Some(reader)) => match reader.read_stripe(stripe.stripe_index) {
                            Ok(bytes) => {
                                if (bytes.len() as u64) != placement.size_bytes
                                    || sha256_bytes(&bytes).to_hex() != placement.sha256_digest
                                {
                                    corrupt_or_stale_shards_rejected += 1;
                                    None
                                } else {
                                    Some(bytes)
                                }
                            }
                            Err(_) => {
                                corrupt_or_stale_shards_rejected += 1;
                                None
                            }
                        },
                        _ => None,
                    }
                }
            } else {
                read_and_verify_shard_placement(
                    backend,
                    &manifest.object_id,
                    stripe.stripe_index,
                    placement,
                    &mut corrupt_or_stale_shards_rejected,
                )?
            };

            match shard_opt {
                Some(bytes) => {
                    shards_read += 1;
                    disk_bytes_read += bytes.len() as u64;
                    verified_ids.insert(ShardId(placement.shard_id));
                    verified_shards.push((ShardId(placement.shard_id), bytes));
                }
                None => {
                    if corrupt_or_stale_shards_rejected == prev_corrupt {
                        missing_or_offline_shards += 1;
                    }
                }
            }
        }

        if verified_shards.len() < k {
            return Err(StorageError::InsufficientValidShards {
                object_id: manifest.object_id.clone(),
                stripe_index: stripe.stripe_index,
                valid_shards: verified_shards.len(),
                required_shards: k,
            });
        }

        let avail_refs: Vec<(ShardId, &[u8])> = verified_shards
            .iter()
            .map(|(id, v)| (*id, v.as_slice()))
            .collect();

        let stripe_payload = codec
            .decode_stripe_payload(&avail_refs, stripe.shard_length, stripe.payload_length)
            .map_err(|_| StorageError::InsufficientValidShards {
                object_id: manifest.object_id.clone(),
                stripe_index: stripe.stripe_index,
                valid_shards: verified_shards.len(),
                required_shards: k,
            })?;

        writer.write_all(&stripe_payload)?;
        object_hasher.update(&stripe_payload);
        total_written += stripe_payload.len() as u64;
    }

    writer.flush()?;

    let actual_object_digest = object_hasher.finalize().to_hex();
    if total_written != manifest.object_size || actual_object_digest != manifest.object_digest {
        return Err(StorageError::ObjectIntegrityFailure {
            object_id: manifest.object_id.clone(),
            expected_digest: manifest.object_digest.clone(),
            actual_digest: actual_object_digest,
        });
    }

    Ok(ReconstructionReport {
        object_id: manifest.object_id.clone(),
        object_size: total_written,
        object_digest: actual_object_digest,
        stripes_processed: manifest.stripes.len(),
        shards_read,
        disk_bytes_read,
        corrupt_or_stale_shards_rejected,
        missing_or_offline_shards,
    })
}

/// Reconstructs an object to `output_path` atomically via a temporary file verified before rename.
pub fn reconstruct_object_to_file(
    backend: &FilesystemBackend,
    manifest: &ObjectManifest,
    output_path: &Path,
) -> Result<ReconstructionReport, StorageError> {
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp_path = output_path.with_extension("fsp_get.tmp");
    let result = {
        let file = File::create(&tmp_path)?;
        let mut buf_writer = BufWriter::with_capacity(256 * 1024, file);
        let report = reconstruct_object_to_writer(backend, manifest, &mut buf_writer)?;
        let inner = buf_writer.into_inner().map_err(|e| e.into_error())?;
        inner.sync_all()?;
        report
    };

    fs::rename(&tmp_path, output_path)?;
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = sync_directory(parent);
        }
    }
    Ok(result)
}
