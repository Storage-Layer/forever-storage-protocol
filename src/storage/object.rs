//! Object ingestion options, upload receipts, and bounded-memory stripe reader utilities.

use std::io::{self, Read};

use crate::coding::stripe::DEFAULT_STRIPE_SIZE_BYTES;
use crate::coding::CodingScheme;
use crate::storage::backend::NodeStorageStatus;
use crate::storage::manifest::ObjectManifest;
use crate::storage::transaction::CrashTrigger;

/// Configuration options for storing an object (`PUT`) in the FSP storage engine.
#[derive(Debug, Clone)]
pub struct PutOptions {
    /// Optional explicit object ID. If `None`, a content-addressed ID (`obj-<sha256_prefix>`) is assigned.
    pub object_id: Option<String>,
    /// Maximum payload bytes per stripe (default `1,048,576` = `1 MiB`).
    pub stripe_size: usize,
    /// Erasure coding scheme (default `CodingScheme::ReedSolomon { n: 20, k: 12 }`).
    pub coding_scheme: CodingScheme,
    /// Optional deterministic crash trigger for transaction fault-injection testing.
    pub crash_trigger: Option<CrashTrigger>,
    /// Whether to consolidate all stripes of each shard into a Phase 4 v2 Multi-Stripe Container (`.fspc`).
    pub use_container_v2: bool,
}

impl Default for PutOptions {
    fn default() -> Self {
        Self {
            object_id: None,
            stripe_size: DEFAULT_STRIPE_SIZE_BYTES,
            coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
            crash_trigger: None,
            use_container_v2: false,
        }
    }
}

impl PutOptions {
    /// Returns default `PutOptions` configured to write Phase 4 v2 multi-stripe shard containers (`.fspc`).
    pub fn default_v2() -> Self {
        Self {
            use_container_v2: true,
            ..Self::default()
        }
    }

    /// Builder: enable or disable Phase 4 v2 multi-stripe shard containers.
    pub fn with_container_v2(mut self, enabled: bool) -> Self {
        self.use_container_v2 = enabled;
        self
    }
}

/// Receipt returned after an object upload transaction reaches `Committed`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutReceipt {
    /// Committed object identifier.
    pub object_id: String,
    /// Original unpadded object length in bytes.
    pub object_size: u64,
    /// Full object SHA-256 hex digest.
    pub object_digest: String,
    /// Number of stripes written.
    pub stripe_count: usize,
    /// Total shard files published across storage nodes.
    pub total_shards_written: usize,
    /// Total physical bytes written to shard files on disk.
    pub total_disk_bytes_written: u64,
    /// Transaction identifier that committed this object.
    pub tx_id: String,
    /// Validated committed object manifest.
    pub manifest: ObjectManifest,
}

/// Summary of the storage cluster's nodes, capacity usage, and stored objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterStorageStatus {
    pub root_path: String,
    pub num_nodes: usize,
    pub num_online_nodes: usize,
    pub num_domains: usize,
    pub committed_objects: Vec<String>,
    pub total_stored_shard_bytes: u64,
    pub total_stored_shard_files: usize,
    pub nodes: Vec<NodeStorageStatus>,
}

/// Reads up to `max_bytes` from `reader` into `buf`, looping until either `max_bytes` are read or EOF (`0`) is reached.
pub fn read_stripe_chunk<R: Read>(reader: &mut R, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut chunk = Vec::with_capacity(max_bytes.min(65_536));
    let mut temp = [0u8; 65_536];
    while chunk.len() < max_bytes {
        let want = (max_bytes - chunk.len()).min(temp.len());
        let n = reader.read(&mut temp[..want])?;
        if n == 0 {
            break;
        }
        chunk.extend_from_slice(&temp[..n]);
    }
    Ok(chunk)
}
