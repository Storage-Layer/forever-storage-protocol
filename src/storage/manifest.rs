//! Canonical object manifest serialization, cryptographic self-verification, and pre-allocation
//! untrusted metadata validation.

use std::collections::BTreeSet;
use std::fmt;

use crate::coding::stripe::{
    compute_stripe_shard_length, MAX_STRIPE_SIZE_BYTES, MIN_STRIPE_SIZE_BYTES,
};
use crate::coding::{CodingScheme, LrcConfig};
use crate::storage::integrity::{sha256_bytes, Sha256Digest};

/// Supported manifest format version for Phase 3.
pub const MANIFEST_FORMAT_VERSION: u32 = 1;

/// Maximum number of stripes permitted in a single manifest (`16,384` stripes).
pub const MAX_MANIFEST_STRIPES: usize = 16_384;

/// Validation errors when inspecting or parsing an untrusted [`ObjectManifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestValidationError {
    /// Malformed JSON or missing mandatory field.
    ParseError(String),
    /// Unsupported `format_version`.
    UnsupportedFormatVersion(u32),
    /// Invalid or unsafe `object_id`.
    InvalidObjectId(String),
    /// Unsupported integrity algorithm (only `"sha256"` is permitted).
    UnsupportedIntegrityAlgorithm(String),
    /// Invalid hexadecimal SHA-256 digest string.
    InvalidDigestHex { field: &'static str, reason: String },
    /// Recomputed manifest digest does not match stored `manifest_digest`.
    ManifestDigestMismatch { expected: String, actual: String },
    /// Invalid coding scheme or out-of-range $(n, k, l)$ parameters.
    InvalidCodingSpec(String),
    /// `stripe_size` outside safe memory bounds `[MIN_STRIPE_SIZE_BYTES, MAX_STRIPE_SIZE_BYTES]`.
    InvalidStripeSize(usize),
    /// Stripe count or stripe geometry inconsistent with `object_size`.
    InconsistentStripeLayout(String),
    /// Duplicate `shard_id` within a stripe.
    DuplicateShardId {
        stripe_index: usize,
        shard_id: usize,
    },
    /// Unsafe shard relative path (e.g., path traversal `..` or absolute path).
    UnsafeRelativePath(String),
}

impl fmt::Display for ManifestValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParseError(msg) => write!(f, "Manifest parse error: {}", msg),
            Self::UnsupportedFormatVersion(v) => {
                write!(f, "Unsupported manifest format_version: {}", v)
            }
            Self::InvalidObjectId(id) => write!(f, "Invalid or unsafe object_id: {:?}", id),
            Self::UnsupportedIntegrityAlgorithm(alg) => {
                write!(f, "Unsupported integrity algorithm: {:?}", alg)
            }
            Self::InvalidDigestHex { field, reason } => {
                write!(f, "Invalid SHA-256 digest in {}: {}", field, reason)
            }
            Self::ManifestDigestMismatch { expected, actual } => write!(
                f,
                "Manifest digest mismatch (tampered or corrupt manifest): expected {}, got {}",
                expected, actual
            ),
            Self::InvalidCodingSpec(msg) => write!(f, "Invalid coding specification: {}", msg),
            Self::InvalidStripeSize(sz) => write!(f, "Invalid stripe_size: {}", sz),
            Self::InconsistentStripeLayout(msg) => {
                write!(f, "Inconsistent stripe layout: {}", msg)
            }
            Self::DuplicateShardId {
                stripe_index,
                shard_id,
            } => write!(
                f,
                "Duplicate shard_id {} in stripe {}",
                shard_id, stripe_index
            ),
            Self::UnsafeRelativePath(p) => write!(f, "Unsafe shard relative_path: {:?}", p),
        }
    }
}

impl std::error::Error for ManifestValidationError {}

/// Serializable coding scheme metadata inside an [`ObjectManifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestCodingSpec {
    /// `"reed-solomon"` or `"lrc"`.
    pub scheme: String,
    /// Total coded shards $n$.
    pub n: usize,
    /// Required data shards $k$.
    pub k: usize,
    /// Optional locality parameter $l$ (present when `scheme == "lrc"`).
    pub l: Option<usize>,
}

impl ManifestCodingSpec {
    /// Creates a [`ManifestCodingSpec`] from a [`CodingScheme`].
    pub fn from_coding_scheme(cs: CodingScheme) -> Self {
        match cs {
            CodingScheme::ReedSolomon { n, k } => Self {
                scheme: "reed-solomon".to_string(),
                n,
                k,
                l: None,
            },
            CodingScheme::Lrc(cfg) => Self {
                scheme: "lrc".to_string(),
                n: cfg.total_shards,
                k: cfg.data_shards,
                l: Some(cfg.locality),
            },
        }
    }

    /// Validates and converts this specification into a [`CodingScheme`].
    pub fn to_coding_scheme(&self) -> Result<CodingScheme, ManifestValidationError> {
        if self.k == 0 || self.n < self.k || self.n > 64 {
            return Err(ManifestValidationError::InvalidCodingSpec(format!(
                "n={} and k={} outside valid storage bounds (1 <= k <= n <= 64)",
                self.n, self.k
            )));
        }
        match self.scheme.as_str() {
            "reed-solomon" => {
                if self.l.is_some() {
                    return Err(ManifestValidationError::InvalidCodingSpec(
                        "reed-solomon scheme must not specify locality l".to_string(),
                    ));
                }
                Ok(CodingScheme::ReedSolomon {
                    n: self.n,
                    k: self.k,
                })
            }
            "lrc" => {
                let l = self.l.ok_or_else(|| {
                    ManifestValidationError::InvalidCodingSpec(
                        "lrc scheme missing locality l".to_string(),
                    )
                })?;
                let cfg = LrcConfig::new(self.n, self.k, l).map_err(|e| {
                    ManifestValidationError::InvalidCodingSpec(format!(
                        "invalid LRC({},{},{}): {}",
                        self.n, self.k, l, e
                    ))
                })?;
                Ok(CodingScheme::Lrc(cfg))
            }
            other => Err(ManifestValidationError::InvalidCodingSpec(format!(
                "unknown coding scheme {:?}",
                other
            ))),
        }
    }
}

/// Metadata and cryptographic digest for a single placed shard within a stripe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardPlacementRecord {
    /// Logical shard index $0 \dots n-1$.
    pub shard_id: usize,
    /// Storage node index hosting this shard.
    pub node_id: usize,
    /// Logical failure domain index of the hosting node.
    pub domain_id: usize,
    /// Monotonically increasing shard generation version (starts at `1`).
    pub version: u64,
    /// Exact shard file size in bytes (`== stripe.shard_length`).
    pub size_bytes: u64,
    /// Lowercase 64-char hex SHA-256 digest of the shard's bytes.
    pub sha256_digest: String,
    /// Safe relative path under the storage root (`node-XX/objects/<object_id>/...`).
    pub relative_path: String,
}

/// Metadata and shard placements for a single stripe of an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeManifest {
    /// 0-based stripe index.
    pub stripe_index: usize,
    /// Byte offset of this stripe's payload within the original object.
    pub payload_offset: u64,
    /// Unpadded original object bytes stored in this stripe (`0..=stripe_size`).
    pub payload_length: usize,
    /// Per-shard byte length for this stripe.
    pub shard_length: usize,
    /// Placement records for all $n$ shards of this stripe.
    pub placements: Vec<ShardPlacementRecord>,
}

/// Canonical persistent manifest for an erasure-coded object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectManifest {
    /// Schema format version (`1`).
    pub format_version: u32,
    /// Safe object identifier (`1..=128` ASCII characters).
    pub object_id: String,
    /// Exact original object length in bytes.
    pub object_size: u64,
    /// Configured maximum payload bytes per stripe.
    pub stripe_size: usize,
    /// Erasure coding specification.
    pub coding: ManifestCodingSpec,
    /// Cryptographic hash algorithm identifier (`"sha256"`).
    pub integrity_algorithm: String,
    /// SHA-256 hex digest of the full original unpadded object.
    pub object_digest: String,
    /// Ordered stripe manifests covering the entire object.
    pub stripes: Vec<StripeManifest>,
    /// SHA-256 hex digest computed over the canonical manifest body.
    pub manifest_digest: String,
}

/// Validates that an identifier (`object_id`, `tx_id`, etc.) contains only safe ASCII characters
/// `[A-Za-z0-9_.-]`, is non-empty (`1..=128`), and is not `.` or `..` or starting with `.`.
pub fn validate_safe_identifier(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 || id.starts_with('.') || id.contains("..") {
        return false;
    }
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

/// Validates that a relative path under the storage root is strictly safe (no leading `/`,
/// no `\`, no `..` components, no empty components, no control/NUL bytes).
pub fn validate_safe_relative_path(rel_path: &str) -> bool {
    if rel_path.is_empty()
        || rel_path.len() > 512
        || rel_path.starts_with('/')
        || rel_path.starts_with('\\')
        || rel_path.contains('\\')
        || rel_path.contains('\0')
    {
        return false;
    }
    for part in rel_path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return false;
        }
        if !part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        {
            return false;
        }
    }
    true
}

impl ObjectManifest {
    /// Computes the canonical digest string over all manifest fields except `manifest_digest`.
    pub fn compute_canonical_digest(&self) -> String {
        let mut canon = String::with_capacity(1024 + self.stripes.len() * 512);
        canon.push_str(&format!(
            "v={}|id={}|size={}|stripe_size={}|scheme={}|n={}|k={}|l={:?}|alg={}|obj_digest={}\n",
            self.format_version,
            self.object_id,
            self.object_size,
            self.stripe_size,
            self.coding.scheme,
            self.coding.n,
            self.coding.k,
            self.coding.l,
            self.integrity_algorithm,
            self.object_digest,
        ));
        for s in &self.stripes {
            canon.push_str(&format!(
                "stripe={}|off={}|len={}|shard_len={}\n",
                s.stripe_index, s.payload_offset, s.payload_length, s.shard_length
            ));
            for p in &s.placements {
                canon.push_str(&format!(
                    "shard={}|node={}|dom={}|ver={}|sz={}|sha256={}|path={}\n",
                    p.shard_id,
                    p.node_id,
                    p.domain_id,
                    p.version,
                    p.size_bytes,
                    p.sha256_digest,
                    p.relative_path
                ));
            }
        }
        sha256_bytes(canon.as_bytes()).to_hex()
    }

    /// Updates `self.manifest_digest` to match `compute_canonical_digest()`.
    pub fn seal(&mut self) {
        self.manifest_digest = self.compute_canonical_digest();
    }

    /// Validates every field of the manifest before any shard buffer allocation or filesystem access.
    pub fn validate(&self) -> Result<CodingScheme, ManifestValidationError> {
        if self.format_version != MANIFEST_FORMAT_VERSION {
            return Err(ManifestValidationError::UnsupportedFormatVersion(
                self.format_version,
            ));
        }
        if !validate_safe_identifier(&self.object_id) {
            return Err(ManifestValidationError::InvalidObjectId(
                self.object_id.clone(),
            ));
        }
        if self.integrity_algorithm != "sha256" {
            return Err(ManifestValidationError::UnsupportedIntegrityAlgorithm(
                self.integrity_algorithm.clone(),
            ));
        }
        Sha256Digest::from_hex(&self.object_digest).map_err(|reason| {
            ManifestValidationError::InvalidDigestHex {
                field: "object_digest",
                reason,
            }
        })?;
        Sha256Digest::from_hex(&self.manifest_digest).map_err(|reason| {
            ManifestValidationError::InvalidDigestHex {
                field: "manifest_digest",
                reason,
            }
        })?;

        let expected_manifest_digest = self.compute_canonical_digest();
        if self.manifest_digest != expected_manifest_digest {
            return Err(ManifestValidationError::ManifestDigestMismatch {
                expected: expected_manifest_digest,
                actual: self.manifest_digest.clone(),
            });
        }

        if self.stripe_size < MIN_STRIPE_SIZE_BYTES || self.stripe_size > MAX_STRIPE_SIZE_BYTES {
            return Err(ManifestValidationError::InvalidStripeSize(self.stripe_size));
        }

        let scheme = self.coding.to_coding_scheme()?;
        let n = scheme.total_shards();
        let k = scheme.data_shards();

        if self.stripes.is_empty() || self.stripes.len() > MAX_MANIFEST_STRIPES {
            return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                "invalid stripe count {}",
                self.stripes.len()
            )));
        }

        if self.object_size == 0 {
            if self.stripes.len() != 1 || self.stripes[0].payload_length != 0 {
                return Err(ManifestValidationError::InconsistentStripeLayout(
                    "0-byte object must have exactly 1 stripe with payload_length == 0".to_string(),
                ));
            }
        } else {
            let expected_stripes = (self.object_size as usize).div_ceil(self.stripe_size);
            if self.stripes.len() != expected_stripes {
                return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                    "expected {} stripes for object_size={}, got {}",
                    expected_stripes,
                    self.object_size,
                    self.stripes.len()
                )));
            }
        }

        let mut cumulative_offset = 0u64;
        for (idx, stripe) in self.stripes.iter().enumerate() {
            if stripe.stripe_index != idx {
                return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                    "stripe index mismatch: expected {}, got {}",
                    idx, stripe.stripe_index
                )));
            }
            if stripe.payload_offset != cumulative_offset {
                return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                    "stripe {} payload_offset mismatch: expected {}, got {}",
                    idx, cumulative_offset, stripe.payload_offset
                )));
            }
            if stripe.payload_length > self.stripe_size {
                return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                    "stripe {} payload_length {} exceeds stripe_size {}",
                    idx, stripe.payload_length, self.stripe_size
                )));
            }
            if self.object_size > 0 && stripe.payload_length == 0 {
                return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                    "non-empty object has 0-length stripe at index {}",
                    idx
                )));
            }

            let expected_shard_len = compute_stripe_shard_length(stripe.payload_length, k);
            if stripe.shard_length != expected_shard_len {
                return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                    "stripe {} shard_length mismatch: expected {}, got {}",
                    idx, expected_shard_len, stripe.shard_length
                )));
            }

            if stripe.placements.len() != n {
                return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                    "stripe {} has {} placements, expected n={}",
                    idx,
                    stripe.placements.len(),
                    n
                )));
            }

            let mut seen_shards = BTreeSet::new();
            for p in &stripe.placements {
                if p.shard_id >= n {
                    return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                        "shard_id {} out of range for n={}",
                        p.shard_id, n
                    )));
                }
                if !seen_shards.insert(p.shard_id) {
                    return Err(ManifestValidationError::DuplicateShardId {
                        stripe_index: idx,
                        shard_id: p.shard_id,
                    });
                }
                if p.version == 0 {
                    return Err(ManifestValidationError::InconsistentStripeLayout(
                        "shard version must be >= 1".to_string(),
                    ));
                }
                if p.size_bytes != (stripe.shard_length as u64) {
                    return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                        "shard {} size_bytes {} != stripe shard_length {}",
                        p.shard_id, p.size_bytes, stripe.shard_length
                    )));
                }
                Sha256Digest::from_hex(&p.sha256_digest).map_err(|reason| {
                    ManifestValidationError::InvalidDigestHex {
                        field: "shard_placement.sha256_digest",
                        reason,
                    }
                })?;
                if !validate_safe_relative_path(&p.relative_path) {
                    return Err(ManifestValidationError::UnsafeRelativePath(
                        p.relative_path.clone(),
                    ));
                }
            }

            cumulative_offset += stripe.payload_length as u64;
        }

        if cumulative_offset != self.object_size {
            return Err(ManifestValidationError::InconsistentStripeLayout(format!(
                "sum of stripe payload_lengths ({}) != object_size ({})",
                cumulative_offset, self.object_size
            )));
        }

        Ok(scheme)
    }

    /// Serializes the manifest into deterministic, human-readable canonical JSON.
    pub fn to_canonical_json(&self) -> String {
        let l_str = match self.coding.l {
            Some(l) => l.to_string(),
            None => "null".to_string(),
        };
        let mut out = String::with_capacity(1024 + self.stripes.len() * 1024);
        out.push_str("{\n");
        out.push_str(&format!("  \"format_version\": {},\n", self.format_version));
        out.push_str(&format!("  \"object_id\": \"{}\",\n", self.object_id));
        out.push_str(&format!("  \"object_size\": {},\n", self.object_size));
        out.push_str(&format!("  \"stripe_size\": {},\n", self.stripe_size));
        out.push_str("  \"coding\": {\n");
        out.push_str(&format!("    \"scheme\": \"{}\",\n", self.coding.scheme));
        out.push_str(&format!("    \"n\": {},\n", self.coding.n));
        out.push_str(&format!("    \"k\": {},\n", self.coding.k));
        out.push_str(&format!("    \"l\": {}\n", l_str));
        out.push_str("  },\n");
        out.push_str("  \"integrity\": {\n");
        out.push_str(&format!(
            "    \"algorithm\": \"{}\",\n",
            self.integrity_algorithm
        ));
        out.push_str(&format!(
            "    \"object_digest\": \"{}\",\n",
            self.object_digest
        ));
        out.push_str(&format!(
            "    \"manifest_digest\": \"{}\"\n",
            self.manifest_digest
        ));
        out.push_str("  },\n");
        out.push_str("  \"stripes\": [\n");
        for (s_idx, s) in self.stripes.iter().enumerate() {
            out.push_str("    {\n");
            out.push_str(&format!("      \"stripe_index\": {},\n", s.stripe_index));
            out.push_str(&format!(
                "      \"payload_offset\": {},\n",
                s.payload_offset
            ));
            out.push_str(&format!(
                "      \"payload_length\": {},\n",
                s.payload_length
            ));
            out.push_str(&format!("      \"shard_length\": {},\n", s.shard_length));
            out.push_str("      \"placements\": [\n");
            for (p_idx, p) in s.placements.iter().enumerate() {
                let comma = if p_idx + 1 < s.placements.len() {
                    ","
                } else {
                    ""
                };
                out.push_str(&format!(
                    "        {{\"shard_id\":{},\"node_id\":{},\"domain_id\":{},\"version\":{},\"size_bytes\":{},\"sha256_digest\":\"{}\",\"relative_path\":\"{}\"}}{}\n",
                    p.shard_id,
                    p.node_id,
                    p.domain_id,
                    p.version,
                    p.size_bytes,
                    p.sha256_digest,
                    p.relative_path,
                    comma
                ));
            }
            out.push_str("      ]\n");
            let s_comma = if s_idx + 1 < self.stripes.len() {
                ","
            } else {
                ""
            };
            out.push_str(&format!("    }}{}\n", s_comma));
        }
        out.push_str("  ]\n");
        out.push_str("}\n");
        out
    }

    /// Parses and validates an [`ObjectManifest`] from its JSON representation.
    pub fn from_json(json: &str) -> Result<Self, ManifestValidationError> {
        let format_version = extract_u64_field(json, "\"format_version\":")? as u32;
        let object_id = extract_str_field(json, "\"object_id\":")?;
        let object_size = extract_u64_field(json, "\"object_size\":")?;
        let stripe_size = extract_u64_field(json, "\"stripe_size\":")? as usize;

        let coding_pos = json
            .find("\"coding\":")
            .ok_or_else(|| ManifestValidationError::ParseError("missing coding block".into()))?;
        let coding_sub = &json[coding_pos..];
        let scheme = extract_str_field(coding_sub, "\"scheme\":")?;
        let n = extract_u64_field(coding_sub, "\"n\":")? as usize;
        let k = extract_u64_field(coding_sub, "\"k\":")? as usize;
        let l = extract_opt_usize_field(coding_sub, "\"l\":")?;

        let integrity_pos = json
            .find("\"integrity\":")
            .ok_or_else(|| ManifestValidationError::ParseError("missing integrity block".into()))?;
        let integrity_sub = &json[integrity_pos..];
        let integrity_algorithm = extract_str_field(integrity_sub, "\"algorithm\":")?;
        let object_digest = extract_str_field(integrity_sub, "\"object_digest\":")?;
        let manifest_digest = extract_str_field(integrity_sub, "\"manifest_digest\":")?;

        let stripes_pos = json
            .find("\"stripes\":")
            .ok_or_else(|| ManifestValidationError::ParseError("missing stripes array".into()))?;
        let stripes_sub = &json[stripes_pos..];

        let mut stripes = Vec::new();
        let mut search_offset = 0usize;
        while let Some(rel_idx) = stripes_sub[search_offset..].find("\"stripe_index\":") {
            let block_start = search_offset + rel_idx;
            let block = &stripes_sub[block_start..];
            let stripe_index = extract_u64_field(block, "\"stripe_index\":")? as usize;
            let payload_offset = extract_u64_field(block, "\"payload_offset\":")?;
            let payload_length = extract_u64_field(block, "\"payload_length\":")? as usize;
            let shard_length = extract_u64_field(block, "\"shard_length\":")? as usize;

            let placements_pos = block.find("\"placements\":").ok_or_else(|| {
                ManifestValidationError::ParseError("missing placements array".into())
            })?;
            let after_placements = &block[placements_pos..];
            let arr_open = after_placements.find('[').ok_or_else(|| {
                ManifestValidationError::ParseError("malformed placements '['".into())
            })?;
            let arr_close = after_placements[arr_open..].find(']').ok_or_else(|| {
                ManifestValidationError::ParseError("malformed placements ']'".into())
            })? + arr_open;
            let placements_slice = &after_placements[arr_open + 1..arr_close];

            let mut placements = Vec::new();
            let mut p_off = 0usize;
            while let Some(obj_open) = placements_slice[p_off..].find('{') {
                let p_start = p_off + obj_open;
                let obj_close = placements_slice[p_start..].find('}').ok_or_else(|| {
                    ManifestValidationError::ParseError("unclosed placement object".into())
                })? + p_start;
                let p_str = &placements_slice[p_start..=obj_close];

                let shard_id = extract_u64_field(p_str, "\"shard_id\":")? as usize;
                let node_id = extract_u64_field(p_str, "\"node_id\":")? as usize;
                let domain_id = extract_u64_field(p_str, "\"domain_id\":")? as usize;
                let version = extract_u64_field(p_str, "\"version\":")?;
                let size_bytes = extract_u64_field(p_str, "\"size_bytes\":")?;
                let sha256_digest = extract_str_field(p_str, "\"sha256_digest\":")?;
                let relative_path = extract_str_field(p_str, "\"relative_path\":")?;

                placements.push(ShardPlacementRecord {
                    shard_id,
                    node_id,
                    domain_id,
                    version,
                    size_bytes,
                    sha256_digest,
                    relative_path,
                });

                p_off = obj_close + 1;
            }

            stripes.push(StripeManifest {
                stripe_index,
                payload_offset,
                payload_length,
                shard_length,
                placements,
            });

            search_offset = block_start + placements_pos + arr_close + 1;
        }

        let manifest = Self {
            format_version,
            object_id,
            object_size,
            stripe_size,
            coding: ManifestCodingSpec { scheme, n, k, l },
            integrity_algorithm,
            object_digest,
            stripes,
            manifest_digest,
        };
        manifest.validate()?;
        Ok(manifest)
    }
}

fn extract_str_field(haystack: &str, key: &str) -> Result<String, ManifestValidationError> {
    let pos = haystack
        .find(key)
        .ok_or_else(|| ManifestValidationError::ParseError(format!("missing field {}", key)))?;
    let rest = haystack[pos + key.len()..].trim_start();
    if !rest.starts_with('"') {
        return Err(ManifestValidationError::ParseError(format!(
            "expected string value after {}",
            key
        )));
    }
    let after_quote = &rest[1..];
    let end_quote = after_quote.find('"').ok_or_else(|| {
        ManifestValidationError::ParseError(format!("unterminated string after {}", key))
    })?;
    Ok(after_quote[..end_quote].to_string())
}

fn extract_u64_field(haystack: &str, key: &str) -> Result<u64, ManifestValidationError> {
    let pos = haystack
        .find(key)
        .ok_or_else(|| ManifestValidationError::ParseError(format!("missing field {}", key)))?;
    let rest = haystack[pos + key.len()..].trim_start();
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if num_end == 0 {
        return Err(ManifestValidationError::ParseError(format!(
            "expected integer after {}",
            key
        )));
    }
    rest[..num_end].parse::<u64>().map_err(|e| {
        ManifestValidationError::ParseError(format!("invalid integer after {}: {}", key, e))
    })
}

fn extract_opt_usize_field(
    haystack: &str,
    key: &str,
) -> Result<Option<usize>, ManifestValidationError> {
    let pos = haystack
        .find(key)
        .ok_or_else(|| ManifestValidationError::ParseError(format!("missing field {}", key)))?;
    let rest = haystack[pos + key.len()..].trim_start();
    if rest.starts_with("null") {
        return Ok(None);
    }
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if num_end == 0 {
        return Err(ManifestValidationError::ParseError(format!(
            "expected integer or null after {}",
            key
        )));
    }
    let val = rest[..num_end].parse::<usize>().map_err(|e| {
        ManifestValidationError::ParseError(format!("invalid integer after {}: {}", key, e))
    })?;
    Ok(Some(val))
}
