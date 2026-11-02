//! Storage backend abstractions, node status descriptors, and unified [`StorageError`] type.

use std::fmt;
use std::io;

use crate::coding::reed_solomon::CodecError;
use crate::model::failure_domain::FailureDomainId;
use crate::model::node::NodeId;
use crate::model::shard::ShardId;
use crate::storage::container::ContainerError;
use crate::storage::manifest::ManifestValidationError;

/// Explicit durability guarantee levels for FSP write operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DurabilityLevel {
    /// In-memory: bytes reached process memory, not persisted to disk.
    Received,
    /// Written to OS page cache / file system buffer, but not fsync'd.
    Buffered,
    /// Shard payload and container verified and fsync'd on local storage node.
    LocallyDurable,
    /// Authoritative metadata manifest committed and synced to WAL/quorum.
    QuorumCommitted,
    /// Persisted across >= K independent failure domains, guaranteeing mathematical recoverability.
    Recoverable,
}

impl DurabilityLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Received => "Received",
            Self::Buffered => "Buffered",
            Self::LocallyDurable => "LocallyDurable",
            Self::QuorumCommitted => "QuorumCommitted",
            Self::Recoverable => "Recoverable",
        }
    }
}

impl std::str::FromStr for DurabilityLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Received" => Ok(Self::Received),
            "Buffered" => Ok(Self::Buffered),
            "LocallyDurable" => Ok(Self::LocallyDurable),
            "QuorumCommitted" => Ok(Self::QuorumCommitted),
            "Recoverable" => Ok(Self::Recoverable),
            other => Err(format!("Unknown DurabilityLevel: {}", other)),
        }
    }
}

impl fmt::Display for DurabilityLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Unified error type for the FSP Phase 3/4 persistent storage engine.
#[derive(Debug)]
pub enum StorageError {
    /// Underlying OS filesystem I/O error.
    Io(io::Error),
    /// Erasure coding / decoding error.
    Codec(CodecError),
    /// Manifest parse or validation error.
    Manifest(ManifestValidationError),
    /// Multi-stripe container v2 parse, validation, or integrity error.
    Container(ContainerError),
    /// Cryptographic manifest signature or signer policy error.
    Signature(String),
    /// Authentication, authorization, or TLS identity error.
    Authentication(String),
    /// Network transport or distributed coordinator error.
    Network(String),
    /// Storage root has not been initialized (missing `.fsp-marker`).
    UninitializedStorageRoot(String),
    /// Rejected unsafe path (path traversal `..`, absolute path, or symlink redirection).
    UnsafePathRejected(String),
    /// Requested object does not exist in the manifest store.
    ObjectNotFound(String),
    /// Target storage node is currently offline / unreachable.
    NodeUnavailable(NodeId),
    /// Storage node quota or disk capacity exceeded.
    QuotaExceeded {
        node_id: NodeId,
        used_bytes: u64,
        requested_bytes: u64,
        quota_bytes: u64,
    },
    /// Shard failed SHA-256 or byte-length verification.
    ShardIntegrityFailure {
        object_id: String,
        stripe_index: usize,
        shard_id: ShardId,
        reason: String,
    },
    /// Reconstructed full object failed end-to-end SHA-256 verification.
    ObjectIntegrityFailure {
        object_id: String,
        expected_digest: String,
        actual_digest: String,
    },
    /// Fewer than $k$ valid verified shards (or unrecoverable LRC pattern) available.
    InsufficientValidShards {
        object_id: String,
        stripe_index: usize,
        valid_shards: usize,
        required_shards: usize,
    },
    /// Deterministic fault/crash injection triggered during testing.
    SimulatedCrash(String),
    /// Mutating request rejected because coordinator's leadership epoch was superseded.
    FencingViolation {
        current_epoch: u64,
        attempted_epoch: u64,
    },
    /// Operation rejected because quorum majority is unavailable (fail-closed).
    QuorumUnavailable(String),
    /// Metadata file or container was corrupted on disk.
    CorruptMetadata(String),
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Codec(e) => write!(f, "Erasure codec error: {}", e),
            Self::Manifest(e) => write!(f, "Manifest error: {}", e),
            Self::Container(e) => write!(f, "Container v2 error: {}", e),
            Self::Signature(msg) => write!(f, "Signature verification error: {}", msg),
            Self::Authentication(msg) => write!(f, "Authentication/authorization error: {}", msg),
            Self::Network(msg) => write!(f, "Network error: {}", msg),
            Self::UninitializedStorageRoot(p) => {
                write!(
                    f,
                    "Uninitialized FSP storage root (missing .fsp-marker): {}",
                    p
                )
            }
            Self::UnsafePathRejected(msg) => {
                write!(f, "Security policy rejected unsafe path: {}", msg)
            }
            Self::ObjectNotFound(id) => write!(f, "Object not found: {}", id),
            Self::NodeUnavailable(id) => write!(f, "Storage node {} is unavailable", id.0),
            Self::QuotaExceeded {
                node_id,
                used_bytes,
                requested_bytes,
                quota_bytes,
            } => write!(
                f,
                "Quota exceeded on node-{:02}: used={} B, requested={} B, quota={} B",
                node_id.0, used_bytes, requested_bytes, quota_bytes
            ),
            Self::ShardIntegrityFailure {
                object_id,
                stripe_index,
                shard_id,
                reason,
            } => write!(
                f,
                "Shard integrity failure for object {} stripe {} shard {}: {}",
                object_id, stripe_index, shard_id.0, reason
            ),
            Self::ObjectIntegrityFailure {
                object_id,
                expected_digest,
                actual_digest,
            } => write!(
                f,
                "Object integrity failure for {}: expected SHA-256 {}, got {}",
                object_id, expected_digest, actual_digest
            ),
            Self::InsufficientValidShards {
                object_id,
                stripe_index,
                valid_shards,
                required_shards,
            } => write!(
                f,
                "Cannot reconstruct object {} stripe {}: only {} valid verified shards available (need >= {})",
                object_id, stripe_index, valid_shards, required_shards
            ),
            Self::SimulatedCrash(stage) => {
                write!(f, "Simulated crash/interruption at stage: {}", stage)
            }
            Self::FencingViolation {
                current_epoch,
                attempted_epoch,
            } => write!(
                f,
                "Fencing violation: current epoch {} > attempted epoch {}",
                current_epoch, attempted_epoch
            ),
            Self::QuorumUnavailable(msg) => write!(f, "Quorum unavailable (fail-closed): {}", msg),
            Self::CorruptMetadata(msg) => write!(f, "Corrupt metadata: {}", msg),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<io::Error> for StorageError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<CodecError> for StorageError {
    fn from(value: CodecError) -> Self {
        Self::Codec(value)
    }
}

impl From<ManifestValidationError> for StorageError {
    fn from(value: ManifestValidationError) -> Self {
        Self::Manifest(value)
    }
}

impl From<ContainerError> for StorageError {
    fn from(value: ContainerError) -> Self {
        Self::Container(value)
    }
}

/// Capacity, availability, and domain assignment for a storage node directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStorageStatus {
    /// Logical node identifier (`node-00`..`node-N`).
    pub node_id: NodeId,
    /// Assigned logical failure domain.
    pub domain_id: FailureDomainId,
    /// Whether the node directory is currently online and reachable.
    pub is_online: bool,
    /// Total bytes of shard files currently stored in this node directory.
    pub used_bytes: u64,
    /// Optional capacity quota in bytes (`None` = unlimited).
    pub quota_bytes: Option<u64>,
    /// Number of shard files stored on this node.
    pub shard_file_count: usize,
}
