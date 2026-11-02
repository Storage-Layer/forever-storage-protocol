//! Phase 4 Per-Shard Multi-Stripe Container Format v2 (`container`).
//!
//! Consolidates all stripes for a single logical `(object_id, shard_id)` on a storage node into
//! one versioned binary container file (`shard-YY.vZ.fspc`), reducing a 100-stripe `RS(20,12)`
//! object from `2,000` individual shard files to `20` shard container files while supporting:
//! - Streaming single-pass writes (`ShardContainerWriter`) with bounded RAM
//! - Validated random-access stripe reads (`ShardContainerReader::read_stripe`)
//! - Strict overflow-checked offset/range validation prior to any buffer allocation
//! - Per-stripe SHA-256 digests, container data-region SHA-256, and header+index SHA-256

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::coding::stripe::{compute_stripe_shard_length, MAX_STRIPE_SIZE_BYTES};
use crate::coding::CodingScheme;
use crate::storage::crash_point::{trigger_crash_point, CrashPoint};
use crate::storage::filesystem::{ensure_not_symlink, open_file_nofollow, sync_directory};
use crate::storage::integrity::{sha256_bytes, Sha256Digest, Sha256Hasher};
use crate::storage::manifest::{
    validate_safe_identifier, ManifestCodingSpec, MAX_MANIFEST_STRIPES,
};

/// 8-byte magic header identifying an FSP Multi-Stripe Shard Container v2.
pub const CONTAINER_MAGIC: [u8; 8] = *b"FSPC\x00\x02\x00\x00";

/// Binary container format version (`2`).
pub const CONTAINER_FORMAT_VERSION: u32 = 2;

/// Fixed size in bytes of the v2 container header (`256` bytes).
pub const CONTAINER_HEADER_SIZE: u64 = 256;

/// Fixed size in bytes of each serialized [`ContainerStripeEntry`] in the index table (`52` bytes).
pub const CONTAINER_INDEX_ENTRY_SIZE: u64 = 52;

/// Coding scheme tag stored in the binary container header.
const SCHEME_TAG_REED_SOLOMON: u8 = 1;
const SCHEME_TAG_LRC: u8 = 2;

/// Errors returned when parsing, validating, reading, or writing a v2 shard container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerError {
    /// Underlying filesystem I/O error.
    Io(String),
    /// Container file is smaller than the mandatory 256-byte header or truncated.
    TruncatedContainer { actual_len: u64, minimum_len: u64 },
    /// Magic bytes do not match `FSPC\x00\x02\x00\x00`.
    InvalidMagic([u8; 8]),
    /// Unsupported binary container version.
    UnsupportedVersion(u32),
    /// Invalid or unsafe `object_id` in container header.
    InvalidObjectId(String),
    /// Invalid coding specification in container header.
    InvalidCodingSpec(String),
    /// Out-of-bounds `shard_id` or zero `version`.
    InvalidShardMetadata(String),
    /// `stripe_count` is zero or exceeds `MAX_MANIFEST_STRIPES` (`16,384`).
    InvalidStripeCount(u32),
    /// Integer arithmetic overflow or out-of-bounds/overlapping stripe offset range.
    InvalidOffsetLayout(String),
    /// Header + index table SHA-256 checksum mismatch.
    HeaderIndexChecksumMismatch { expected: String, actual: String },
    /// Container data body SHA-256 checksum mismatch.
    ContainerBodyChecksumMismatch { expected: String, actual: String },
    /// Per-stripe SHA-256 digest mismatch or unexpected stripe index.
    StripeIntegrityMismatch {
        stripe_index: usize,
        expected: String,
        actual: String,
    },
    /// Requested `stripe_index` is out of range `0..stripe_count`.
    StripeIndexOutOfBounds {
        stripe_index: usize,
        stripe_count: usize,
    },
}

impl fmt::Display for ContainerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "Container I/O error: {}", msg),
            Self::TruncatedContainer {
                actual_len,
                minimum_len,
            } => write!(
                f,
                "Truncated shard container: length {} < required {}",
                actual_len, minimum_len
            ),
            Self::InvalidMagic(m) => write!(f, "Invalid container magic bytes: {:?}", m),
            Self::UnsupportedVersion(v) => {
                write!(f, "Unsupported container format version: {}", v)
            }
            Self::InvalidObjectId(id) => write!(f, "Invalid container object_id: {:?}", id),
            Self::InvalidCodingSpec(msg) => write!(f, "Invalid container coding spec: {}", msg),
            Self::InvalidShardMetadata(msg) => {
                write!(f, "Invalid container shard metadata: {}", msg)
            }
            Self::InvalidStripeCount(c) => write!(f, "Invalid container stripe_count: {}", c),
            Self::InvalidOffsetLayout(msg) => {
                write!(f, "Invalid container offset/range layout: {}", msg)
            }
            Self::HeaderIndexChecksumMismatch { expected, actual } => write!(
                f,
                "Container header+index SHA-256 mismatch: expected {}, got {}",
                expected, actual
            ),
            Self::ContainerBodyChecksumMismatch { expected, actual } => write!(
                f,
                "Container body SHA-256 mismatch: expected {}, got {}",
                expected, actual
            ),
            Self::StripeIntegrityMismatch {
                stripe_index,
                expected,
                actual,
            } => write!(
                f,
                "Container stripe {} SHA-256 mismatch: expected {}, got {}",
                stripe_index, expected, actual
            ),
            Self::StripeIndexOutOfBounds {
                stripe_index,
                stripe_count,
            } => write!(
                f,
                "Stripe index {} out of bounds for container with {} stripes",
                stripe_index, stripe_count
            ),
        }
    }
}

impl std::error::Error for ContainerError {}

impl From<std::io::Error> for ContainerError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

/// Validated metadata for a single stripe stored inside a v2 shard container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerStripeEntry {
    /// 0-based stripe index.
    pub stripe_index: u32,
    /// Original unpadded stripe payload length in bytes (`0..=MAX_STRIPE_SIZE_BYTES`).
    pub payload_length: u32,
    /// Encoded shard byte length for this stripe (`1..=MAX_STRIPE_SIZE_BYTES`).
    pub shard_length: u32,
    /// Exact byte offset of this stripe's shard bytes inside the container file.
    pub shard_offset: u64,
    /// SHA-256 digest of this stripe's shard bytes.
    pub shard_sha256: Sha256Digest,
}

impl ContainerStripeEntry {
    /// Serializes this index entry into its canonical 52-byte little-endian binary representation.
    pub fn to_bytes(&self) -> [u8; 52] {
        let mut buf = [0u8; 52];
        buf[0..4].copy_from_slice(&self.stripe_index.to_le_bytes());
        buf[4..8].copy_from_slice(&self.payload_length.to_le_bytes());
        buf[8..12].copy_from_slice(&self.shard_length.to_le_bytes());
        buf[12..20].copy_from_slice(&self.shard_offset.to_le_bytes());
        buf[20..52].copy_from_slice(&self.shard_sha256.0);
        buf
    }

    /// Parses a 52-byte index entry without trusting its offset/length values until validated.
    pub fn from_bytes(buf: &[u8; 52]) -> Self {
        let stripe_index = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let payload_length = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let shard_length = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let shard_offset = u64::from_le_bytes(buf[12..20].try_into().unwrap());
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&buf[20..52]);
        Self {
            stripe_index,
            payload_length,
            shard_length,
            shard_offset,
            shard_sha256: Sha256Digest(digest),
        }
    }
}

/// Validated header of a v2 Per-Shard Multi-Stripe Container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerHeader {
    /// Format version (`2`).
    pub format_version: u32,
    /// Validated object identifier (`1..=128` safe ASCII bytes).
    pub object_id: String,
    /// Validated coding scheme (`ReedSolomon` or `Lrc`).
    pub coding_scheme: CodingScheme,
    /// Logical shard index (`0..n`).
    pub shard_id: u16,
    /// Monotonic shard generation version (`>= 1`).
    pub version: u64,
    /// Total number of stripes stored in this container (`1..=MAX_MANIFEST_STRIPES`).
    pub stripe_count: u32,
    /// Byte offset where contiguous stripe payloads begin (`== 256`).
    pub data_region_offset: u64,
    /// Byte offset where the Stripe Index Table begins (`== 256 + sum(shard_length)`).
    pub index_table_offset: u64,
    /// Exact total file size in bytes (`== index_table_offset + stripe_count * 52`).
    pub total_file_size: u64,
    /// SHA-256 digest over all concatenated stripe shard bytes in `[data_region_offset .. index_table_offset)`.
    pub container_body_sha256: Sha256Digest,
    /// SHA-256 digest over header bytes `0..224` + the entire Stripe Index Table.
    pub header_and_index_sha256: Sha256Digest,
}

impl ContainerHeader {
    /// Serializes the first `224` bytes of the 256-byte header (everything before `header_and_index_sha256`).
    fn to_prefix_224_bytes(&self) -> Result<[u8; 224], ContainerError> {
        if !validate_safe_identifier(&self.object_id) {
            return Err(ContainerError::InvalidObjectId(self.object_id.clone()));
        }
        let mut buf = [0u8; 224];
        buf[0..8].copy_from_slice(&CONTAINER_MAGIC);
        buf[8..12].copy_from_slice(&self.format_version.to_le_bytes());

        let (scheme_tag, n, k, l) = match self.coding_scheme {
            CodingScheme::ReedSolomon { n, k } => {
                (SCHEME_TAG_REED_SOLOMON, n as u16, k as u16, 0u16)
            }
            CodingScheme::Lrc(cfg) => (
                SCHEME_TAG_LRC,
                cfg.total_shards as u16,
                cfg.data_shards as u16,
                cfg.locality as u16,
            ),
        };
        buf[12] = scheme_tag;
        // buf[13..16] reserved = 0
        buf[16..18].copy_from_slice(&n.to_le_bytes());
        buf[18..20].copy_from_slice(&k.to_le_bytes());
        buf[20..22].copy_from_slice(&l.to_le_bytes());
        buf[22..24].copy_from_slice(&self.shard_id.to_le_bytes());
        buf[24..32].copy_from_slice(&self.version.to_le_bytes());
        buf[32..36].copy_from_slice(&self.stripe_count.to_le_bytes());
        let obj_bytes = self.object_id.as_bytes();
        buf[36..40].copy_from_slice(&(obj_bytes.len() as u32).to_le_bytes());
        buf[40..48].copy_from_slice(&self.data_region_offset.to_le_bytes());
        buf[48..56].copy_from_slice(&self.index_table_offset.to_le_bytes());
        buf[56..64].copy_from_slice(&self.total_file_size.to_le_bytes());
        buf[64..192][..obj_bytes.len()].copy_from_slice(obj_bytes);
        buf[192..224].copy_from_slice(&self.container_body_sha256.0);
        Ok(buf)
    }

    /// Serializes the full 256-byte header.
    pub fn to_bytes(&self) -> Result<[u8; 256], ContainerError> {
        let prefix = self.to_prefix_224_bytes()?;
        let mut full = [0u8; 256];
        full[0..224].copy_from_slice(&prefix);
        full[224..256].copy_from_slice(&self.header_and_index_sha256.0);
        Ok(full)
    }
}

/// Streaming writer that constructs a v2 shard container file in a single pass without buffering
/// all stripes in memory.
pub struct ShardContainerWriter {
    staged_path: PathBuf,
    writer: BufWriter<File>,
    object_id: String,
    coding_scheme: CodingScheme,
    shard_id: u16,
    version: u64,
    current_offset: u64,
    body_hasher: Sha256Hasher,
    entries: Vec<ContainerStripeEntry>,
}

impl ShardContainerWriter {
    /// Creates a new staged container file at `staged_path` and reserves the 256-byte header at offset `0`.
    pub fn create(
        staged_path: &Path,
        initial_object_id: &str,
        coding_scheme: CodingScheme,
        shard_id: usize,
        version: u64,
    ) -> Result<Self, ContainerError> {
        if !validate_safe_identifier(initial_object_id) {
            return Err(ContainerError::InvalidObjectId(
                initial_object_id.to_string(),
            ));
        }
        if shard_id >= coding_scheme.total_shards() || version == 0 {
            return Err(ContainerError::InvalidShardMetadata(format!(
                "shard_id={} (n={}), version={}",
                shard_id,
                coding_scheme.total_shards(),
                version
            )));
        }
        if let Some(parent) = staged_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        if staged_path.exists() {
            ensure_not_symlink(staged_path).map_err(|e| ContainerError::Io(e.to_string()))?;
        }
        let file = File::create(staged_path)?;
        let mut writer = BufWriter::with_capacity(64 * 1024, file);
        // Write 256-byte zero placeholder header so stripe 0 starts at offset 256
        writer.write_all(&[0u8; CONTAINER_HEADER_SIZE as usize])?;

        Ok(Self {
            staged_path: staged_path.to_path_buf(),
            writer,
            object_id: initial_object_id.to_string(),
            coding_scheme,
            shard_id: shard_id as u16,
            version,
            current_offset: CONTAINER_HEADER_SIZE,
            body_hasher: Sha256Hasher::new(),
            entries: Vec::new(),
        })
    }

    /// Appends the next stripe's shard bytes sequentially to the container.
    pub fn append_stripe(
        &mut self,
        stripe_index: usize,
        payload_length: usize,
        shard_bytes: &[u8],
    ) -> Result<ContainerStripeEntry, ContainerError> {
        let expected_idx = self.entries.len();
        if stripe_index != expected_idx {
            return Err(ContainerError::InvalidOffsetLayout(format!(
                "out-of-order stripe append: expected {}, got {}",
                expected_idx, stripe_index
            )));
        }
        if self.entries.len() >= MAX_MANIFEST_STRIPES {
            return Err(ContainerError::InvalidStripeCount(
                (self.entries.len() + 1) as u32,
            ));
        }
        if payload_length > MAX_STRIPE_SIZE_BYTES {
            return Err(ContainerError::InvalidOffsetLayout(format!(
                "payload_length {} exceeds MAX_STRIPE_SIZE_BYTES",
                payload_length
            )));
        }
        let expected_shard_len =
            compute_stripe_shard_length(payload_length, self.coding_scheme.data_shards());
        if shard_bytes.is_empty()
            || shard_bytes.len() > MAX_STRIPE_SIZE_BYTES
            || shard_bytes.len() != expected_shard_len
        {
            return Err(ContainerError::InvalidOffsetLayout(format!(
                "invalid shard_bytes.len() = {}, expected {}",
                shard_bytes.len(),
                expected_shard_len
            )));
        }

        let shard_offset = self.current_offset;
        let next_offset = shard_offset
            .checked_add(shard_bytes.len() as u64)
            .ok_or_else(|| ContainerError::InvalidOffsetLayout("offset overflow".into()))?;

        let shard_sha256 = sha256_bytes(shard_bytes);
        self.writer.write_all(shard_bytes)?;
        self.body_hasher.update(shard_bytes);
        self.current_offset = next_offset;

        let entry = ContainerStripeEntry {
            stripe_index: stripe_index as u32,
            payload_length: payload_length as u32,
            shard_length: shard_bytes.len() as u32,
            shard_offset,
            shard_sha256,
        };
        self.entries.push(entry.clone());
        Ok(entry)
    }

    /// Updates `object_id` before finalizing (used when content-addressed `object_id` is derived
    /// after streaming the full input payload).
    pub fn set_final_object_id(&mut self, final_object_id: &str) -> Result<(), ContainerError> {
        if !validate_safe_identifier(final_object_id) {
            return Err(ContainerError::InvalidObjectId(final_object_id.to_string()));
        }
        self.object_id = final_object_id.to_string();
        Ok(())
    }

    /// Writes the Stripe Index Table, seals the 256-byte header at offset `0`, flushes and calls
    /// `File::sync_all()`, and returns the finalized [`ContainerHeader`] and stripe entries.
    pub fn finalize(
        mut self,
    ) -> Result<(PathBuf, ContainerHeader, Vec<ContainerStripeEntry>), ContainerError> {
        if self.entries.is_empty() || self.entries.len() > MAX_MANIFEST_STRIPES {
            return Err(ContainerError::InvalidStripeCount(self.entries.len() as u32));
        }

        let index_table_offset = self.current_offset;
        let index_bytes_len = (self.entries.len() as u64)
            .checked_mul(CONTAINER_INDEX_ENTRY_SIZE)
            .ok_or_else(|| ContainerError::InvalidOffsetLayout("index table overflow".into()))?;
        let total_file_size = index_table_offset
            .checked_add(index_bytes_len)
            .ok_or_else(|| ContainerError::InvalidOffsetLayout("total size overflow".into()))?;

        let mut index_buf = Vec::with_capacity(index_bytes_len as usize);
        for entry in &self.entries {
            index_buf.extend_from_slice(&entry.to_bytes());
        }
        self.writer.write_all(&index_buf)?;
        self.writer.flush()?;

        let container_body_sha256 = self.body_hasher.finalize();
        let mut header = ContainerHeader {
            format_version: CONTAINER_FORMAT_VERSION,
            object_id: self.object_id.clone(),
            coding_scheme: self.coding_scheme,
            shard_id: self.shard_id,
            version: self.version,
            stripe_count: self.entries.len() as u32,
            data_region_offset: CONTAINER_HEADER_SIZE,
            index_table_offset,
            total_file_size,
            container_body_sha256,
            header_and_index_sha256: Sha256Digest([0u8; 32]),
        };

        let prefix_224 = header.to_prefix_224_bytes()?;
        let mut hdr_hasher = Sha256Hasher::new();
        hdr_hasher.update(&prefix_224);
        hdr_hasher.update(&index_buf);
        header.header_and_index_sha256 = hdr_hasher.finalize();

        let header_bytes = header.to_bytes()?;
        let mut file = self
            .writer
            .into_inner()
            .map_err(|e| ContainerError::Io(e.into_error().to_string()))?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header_bytes)?;
        file.flush()?;
        file.sync_all()?;

        Ok((self.staged_path, header, self.entries))
    }
}

/// Convenience helper to write a complete multi-stripe shard container atomically to `target_path`.
pub fn write_shard_container_atomic(
    target_path: &Path,
    object_id: &str,
    coding_scheme: CodingScheme,
    shard_id: usize,
    version: u64,
    stripes: &[(usize, &[u8])],
) -> Result<(ContainerHeader, Vec<ContainerStripeEntry>), ContainerError> {
    trigger_crash_point(CrashPoint::BeforeTempFileCreate)
        .map_err(|e| ContainerError::Io(e.to_string()))?;
    let tmp_path = target_path.with_extension("fspc.tmp");
    let mut writer =
        ShardContainerWriter::create(&tmp_path, object_id, coding_scheme, shard_id, version)?;
    for (idx, &(payload_len, shard_bytes)) in stripes.iter().enumerate() {
        trigger_crash_point(CrashPoint::DuringPartialShardWrite)
            .map_err(|e| ContainerError::Io(e.to_string()))?;
        writer.append_stripe(idx, payload_len, shard_bytes)?;
    }
    trigger_crash_point(CrashPoint::AfterBytesWrittenBeforeFsync)
        .map_err(|e| ContainerError::Io(e.to_string()))?;
    let (_, header, entries) = writer.finalize()?;
    trigger_crash_point(CrashPoint::AfterFsyncBeforeRename)
        .map_err(|e| ContainerError::Io(e.to_string()))?;
    fs::rename(&tmp_path, target_path)?;
    trigger_crash_point(CrashPoint::AfterRenameBeforeParentDirSync)
        .map_err(|e| ContainerError::Io(e.to_string()))?;
    if let Some(parent) = target_path.parent() {
        sync_directory(parent).map_err(|e| ContainerError::Io(e.to_string()))?;
        trigger_crash_point(CrashPoint::AfterParentDirSync)
            .map_err(|e| ContainerError::Io(e.to_string()))?;
    }
    Ok((header, entries))
}

/// Validated random-access reader for a v2 Per-Shard Multi-Stripe Container.
///
/// Validates magic bytes, format version, coding specification, header+index SHA-256, and every
/// stripe entry's offset/length invariants upon `open()` BEFORE allowing any stripe reads.
#[derive(Debug)]
pub struct ShardContainerReader {
    file: File,
    header: ContainerHeader,
    entries: Vec<ContainerStripeEntry>,
}

impl ShardContainerReader {
    /// Opens and strictly validates a v2 shard container at `path` (using `O_NOFOLLOW` on Unix).
    pub fn open(path: &Path) -> Result<Self, ContainerError> {
        ensure_not_symlink(path).map_err(|e| ContainerError::Io(e.to_string()))?;
        let mut file = open_file_nofollow(path).map_err(|e| ContainerError::Io(e.to_string()))?;
        let file_len = file.metadata()?.len();
        if file_len < CONTAINER_HEADER_SIZE {
            return Err(ContainerError::TruncatedContainer {
                actual_len: file_len,
                minimum_len: CONTAINER_HEADER_SIZE,
            });
        }

        let mut hdr_bytes = [0u8; CONTAINER_HEADER_SIZE as usize];
        file.read_exact(&mut hdr_bytes)?;

        let mut magic = [0u8; 8];
        magic.copy_from_slice(&hdr_bytes[0..8]);
        if magic != CONTAINER_MAGIC {
            return Err(ContainerError::InvalidMagic(magic));
        }

        let format_version = u32::from_le_bytes(hdr_bytes[8..12].try_into().unwrap());
        if format_version != CONTAINER_FORMAT_VERSION {
            return Err(ContainerError::UnsupportedVersion(format_version));
        }

        let scheme_tag = hdr_bytes[12];
        if hdr_bytes[13..16] != [0, 0, 0] {
            return Err(ContainerError::InvalidShardMetadata(
                "non-zero reserved flags in header".into(),
            ));
        }
        let n = u16::from_le_bytes(hdr_bytes[16..18].try_into().unwrap()) as usize;
        let k = u16::from_le_bytes(hdr_bytes[18..20].try_into().unwrap()) as usize;
        let l_raw = u16::from_le_bytes(hdr_bytes[20..22].try_into().unwrap()) as usize;
        let shard_id = u16::from_le_bytes(hdr_bytes[22..24].try_into().unwrap());
        let version = u64::from_le_bytes(hdr_bytes[24..32].try_into().unwrap());
        let stripe_count = u32::from_le_bytes(hdr_bytes[32..36].try_into().unwrap());
        let obj_id_len = u32::from_le_bytes(hdr_bytes[36..40].try_into().unwrap()) as usize;
        let data_region_offset = u64::from_le_bytes(hdr_bytes[40..48].try_into().unwrap());
        let index_table_offset = u64::from_le_bytes(hdr_bytes[48..56].try_into().unwrap());
        let total_file_size = u64::from_le_bytes(hdr_bytes[56..64].try_into().unwrap());

        if obj_id_len == 0 || obj_id_len > 128 {
            return Err(ContainerError::InvalidObjectId(format!(
                "invalid object_id_len={}",
                obj_id_len
            )));
        }
        if hdr_bytes[64 + obj_id_len..192].iter().any(|&b| b != 0) {
            return Err(ContainerError::InvalidObjectId(
                "non-zero padding in object_id slot".into(),
            ));
        }
        let object_id = std::str::from_utf8(&hdr_bytes[64..64 + obj_id_len])
            .map_err(|_| ContainerError::InvalidObjectId("non-UTF8 object_id".into()))?
            .to_string();
        if !validate_safe_identifier(&object_id) {
            return Err(ContainerError::InvalidObjectId(object_id));
        }

        let coding_spec = match scheme_tag {
            SCHEME_TAG_REED_SOLOMON => {
                if l_raw != 0 {
                    return Err(ContainerError::InvalidCodingSpec(
                        "RS container must have l=0".into(),
                    ));
                }
                ManifestCodingSpec {
                    scheme: "reed-solomon".to_string(),
                    n,
                    k,
                    l: None,
                }
            }
            SCHEME_TAG_LRC => ManifestCodingSpec {
                scheme: "lrc".to_string(),
                n,
                k,
                l: Some(l_raw),
            },
            other => {
                return Err(ContainerError::InvalidCodingSpec(format!(
                    "unknown scheme_tag={}",
                    other
                )));
            }
        };
        let coding_scheme = coding_spec
            .to_coding_scheme()
            .map_err(|e| ContainerError::InvalidCodingSpec(e.to_string()))?;

        if (shard_id as usize) >= n || version == 0 {
            return Err(ContainerError::InvalidShardMetadata(format!(
                "shard_id={} out of range for n={} or version={} == 0",
                shard_id, n, version
            )));
        }

        if stripe_count == 0 || (stripe_count as usize) > MAX_MANIFEST_STRIPES {
            return Err(ContainerError::InvalidStripeCount(stripe_count));
        }

        if data_region_offset != CONTAINER_HEADER_SIZE {
            return Err(ContainerError::InvalidOffsetLayout(format!(
                "data_region_offset {} != {}",
                data_region_offset, CONTAINER_HEADER_SIZE
            )));
        }
        if index_table_offset <= data_region_offset {
            return Err(ContainerError::InvalidOffsetLayout(format!(
                "index_table_offset {} <= data_region_offset {}",
                index_table_offset, data_region_offset
            )));
        }

        let index_bytes_len = (stripe_count as u64)
            .checked_mul(CONTAINER_INDEX_ENTRY_SIZE)
            .ok_or_else(|| ContainerError::InvalidOffsetLayout("index size overflow".into()))?;
        let expected_total_size = index_table_offset
            .checked_add(index_bytes_len)
            .ok_or_else(|| ContainerError::InvalidOffsetLayout("file size overflow".into()))?;

        if total_file_size != expected_total_size || file_len != expected_total_size {
            return Err(ContainerError::InvalidOffsetLayout(format!(
                "total_file_size={} (expected={}) vs on-disk file_len={}",
                total_file_size, expected_total_size, file_len
            )));
        }

        let mut body_sha = [0u8; 32];
        body_sha.copy_from_slice(&hdr_bytes[192..224]);
        let mut expected_hdr_idx_sha = [0u8; 32];
        expected_hdr_idx_sha.copy_from_slice(&hdr_bytes[224..256]);

        // Read bounded Stripe Index Table and verify header_and_index_sha256 BEFORE trusting entries
        file.seek(SeekFrom::Start(index_table_offset))?;
        let mut index_buf = vec![0u8; index_bytes_len as usize];
        file.read_exact(&mut index_buf)?;

        let mut hdr_hasher = Sha256Hasher::new();
        hdr_hasher.update(&hdr_bytes[0..224]);
        hdr_hasher.update(&index_buf);
        let actual_hdr_idx_sha = hdr_hasher.finalize();
        if actual_hdr_idx_sha.0 != expected_hdr_idx_sha {
            return Err(ContainerError::HeaderIndexChecksumMismatch {
                expected: Sha256Digest(expected_hdr_idx_sha).to_hex(),
                actual: actual_hdr_idx_sha.to_hex(),
            });
        }

        // Validate every stripe index entry for contiguous, non-overlapping, overflow-free bounds
        let mut entries = Vec::with_capacity(stripe_count as usize);
        let mut expected_offset = data_region_offset;

        for idx in 0..(stripe_count as usize) {
            let start = idx * (CONTAINER_INDEX_ENTRY_SIZE as usize);
            let slice: &[u8; 52] = index_buf[start..start + 52].try_into().unwrap();
            let entry = ContainerStripeEntry::from_bytes(slice);

            if (entry.stripe_index as usize) != idx {
                return Err(ContainerError::InvalidOffsetLayout(format!(
                    "stripe index mismatch at position {}: got {}",
                    idx, entry.stripe_index
                )));
            }
            if (entry.payload_length as usize) > MAX_STRIPE_SIZE_BYTES {
                return Err(ContainerError::InvalidOffsetLayout(format!(
                    "stripe {} payload_length {} exceeds MAX_STRIPE_SIZE_BYTES",
                    idx, entry.payload_length
                )));
            }
            if entry.shard_length == 0 || (entry.shard_length as usize) > MAX_STRIPE_SIZE_BYTES {
                return Err(ContainerError::InvalidOffsetLayout(format!(
                    "stripe {} shard_length {} out of bounds",
                    idx, entry.shard_length
                )));
            }
            let expected_shard_len = compute_stripe_shard_length(entry.payload_length as usize, k);
            if (entry.shard_length as usize) != expected_shard_len {
                return Err(ContainerError::InvalidOffsetLayout(format!(
                    "stripe {} shard_length {} != expected {}",
                    idx, entry.shard_length, expected_shard_len
                )));
            }
            if entry.shard_offset != expected_offset {
                return Err(ContainerError::InvalidOffsetLayout(format!(
                    "stripe {} offset {} != expected contiguous offset {}",
                    idx, entry.shard_offset, expected_offset
                )));
            }
            expected_offset = expected_offset
                .checked_add(entry.shard_length as u64)
                .ok_or_else(|| {
                    ContainerError::InvalidOffsetLayout("stripe offset overflow".into())
                })?;
            entries.push(entry);
        }

        if expected_offset != index_table_offset {
            return Err(ContainerError::InvalidOffsetLayout(format!(
                "end of stripes {} != index_table_offset {}",
                expected_offset, index_table_offset
            )));
        }

        let header = ContainerHeader {
            format_version,
            object_id,
            coding_scheme,
            shard_id,
            version,
            stripe_count,
            data_region_offset,
            index_table_offset,
            total_file_size,
            container_body_sha256: Sha256Digest(body_sha),
            header_and_index_sha256: Sha256Digest(expected_hdr_idx_sha),
        };

        Ok(Self {
            file,
            header,
            entries,
        })
    }

    /// Returns the validated [`ContainerHeader`].
    pub const fn header(&self) -> &ContainerHeader {
        &self.header
    }

    /// Returns the validated slice of [`ContainerStripeEntry`] records.
    pub fn entries(&self) -> &[ContainerStripeEntry] {
        &self.entries
    }

    /// Reads and cryptographically verifies a single stripe's shard bytes by `stripe_index` using
    /// bounded random-access I/O.
    pub fn read_stripe(&mut self, stripe_index: usize) -> Result<Vec<u8>, ContainerError> {
        let entry = self
            .entries
            .get(stripe_index)
            .ok_or(ContainerError::StripeIndexOutOfBounds {
                stripe_index,
                stripe_count: self.entries.len(),
            })?
            .clone();

        let mut buf = vec![0u8; entry.shard_length as usize];
        self.file.seek(SeekFrom::Start(entry.shard_offset))?;
        self.file.read_exact(&mut buf)?;

        let actual_sha = sha256_bytes(&buf);
        if actual_sha != entry.shard_sha256 {
            return Err(ContainerError::StripeIntegrityMismatch {
                stripe_index,
                expected: entry.shard_sha256.to_hex(),
                actual: actual_sha.to_hex(),
            });
        }
        Ok(buf)
    }

    /// Streams through the entire container data region, verifying every stripe's SHA-256 digest
    /// AND the container-level `container_body_sha256`.
    pub fn verify_all(&mut self) -> Result<(), ContainerError> {
        self.file
            .seek(SeekFrom::Start(self.header.data_region_offset))?;
        let mut reader = BufReader::with_capacity(64 * 1024, &mut self.file);
        let mut body_hasher = Sha256Hasher::new();

        for (idx, entry) in self.entries.iter().enumerate() {
            let mut buf = vec![0u8; entry.shard_length as usize];
            reader.read_exact(&mut buf)?;
            let stripe_sha = sha256_bytes(&buf);
            if stripe_sha != entry.shard_sha256 {
                return Err(ContainerError::StripeIntegrityMismatch {
                    stripe_index: idx,
                    expected: entry.shard_sha256.to_hex(),
                    actual: stripe_sha.to_hex(),
                });
            }
            body_hasher.update(&buf);
        }

        let actual_body_sha = body_hasher.finalize();
        if actual_body_sha != self.header.container_body_sha256 {
            return Err(ContainerError::ContainerBodyChecksumMismatch {
                expected: self.header.container_body_sha256.to_hex(),
                actual: actual_body_sha.to_hex(),
            });
        }
        Ok(())
    }
}

/// Replaces a single stripe's shard bytes inside an existing (or newly reconstructed) v2 shard
/// container atomically via a `.fspc.repair.tmp.<nonce>` staging file.
#[allow(clippy::too_many_arguments)]
pub fn repair_or_create_container_stripe_atomic(
    target_path: &Path,
    object_id: &str,
    coding_scheme: CodingScheme,
    shard_id: usize,
    version: u64,
    all_stripe_shard_bytes: &[(usize, &[u8])],
    nonce: u64,
) -> Result<(ContainerHeader, Vec<ContainerStripeEntry>), ContainerError> {
    if let Some(parent) = target_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent)?;
        }
    }
    let tmp_path = target_path.with_extension(format!("fspc.repair.tmp.{}", nonce));
    let mut writer =
        ShardContainerWriter::create(&tmp_path, object_id, coding_scheme, shard_id, version)?;
    for (idx, &(payload_len, shard_bytes)) in all_stripe_shard_bytes.iter().enumerate() {
        writer.append_stripe(idx, payload_len, shard_bytes)?;
    }
    let (_, header, entries) = writer.finalize()?;

    // Re-open and verify the repaired container on disk before atomic rename
    let mut verifier = ShardContainerReader::open(&tmp_path)?;
    verifier.verify_all()?;
    drop(verifier);

    fs::rename(&tmp_path, target_path)?;
    if let Some(parent) = target_path.parent() {
        let _ = OpenOptions::new()
            .read(true)
            .open(parent)
            .and_then(|d| d.sync_all());
    }
    Ok((header, entries))
}
