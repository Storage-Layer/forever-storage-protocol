//! Wire-level RPC message definitions, explicit protocol versioning, size limits, and deterministic
//! binary framing for FSP Phase 4 distributed storage communication (`src/network/protocol.rs`).

use std::fmt;
use std::str::FromStr;

use crate::coding::{CodingScheme, LrcConfig};
use crate::model::node::NodeId;
use crate::security::authorization::RpcOperation;
use crate::storage::manifest::validate_safe_identifier;
use crate::storage::DurabilityLevel;

/// Wire magic prefix for every FSP network frame (`"FSPN"`).
pub const FRAME_MAGIC: [u8; 4] = *b"FSPN";

/// Current wire protocol version (`1`). Mismatched versions are explicitly rejected.
pub const NETWORK_PROTOCOL_VERSION: u16 = 1;

/// Maximum permitted metadata/header frame size (`256 KiB`).
pub const MAX_RPC_META_BYTES: u32 = 256 * 1024;

/// Maximum permitted binary payload frame size (`32 MiB`).
pub const MAX_RPC_PAYLOAD_BYTES: u32 = 32 * 1024 * 1024;

/// Stable network and RPC protocol error categories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// Socket/TLS transport error or connection reset.
    Transport(String),
    /// Request or response timed out.
    Timeout(String),
    /// Simulated packet loss or network partition injected by test harness.
    SimulatedNetworkDrop(String),
    /// Invalid frame magic bytes.
    InvalidFrameMagic([u8; 4]),
    /// Unsupported wire protocol version.
    ProtocolVersionMismatch { expected: u16, actual: u16 },
    /// Frame metadata or binary payload exceeds maximum allowed size.
    FrameTooLarge {
        field: &'static str,
        size: u32,
        limit: u32,
    },
    /// Malformed serialized request or response.
    SerializationError(String),
    /// Mutual TLS identity or role authorization failure.
    Unauthorized(String),
    /// Node storage quota exceeded (`ENOSPC` / capacity limit).
    QuotaExceeded(String),
    /// Requested shard or stripe does not exist on the node.
    ShardNotFound {
        object_id: String,
        shard_id: usize,
        version: u64,
    },
    /// Cryptographic SHA-256 or container verification failure.
    IntegrityFailure(String),
    /// Internal node storage engine error.
    InternalNodeError(String),
    /// Mutating request rejected because coordinator's leadership epoch was superseded.
    Fenced {
        current_epoch: u64,
        presented_epoch: u64,
    },
    /// Local filesystem or I/O error.
    Io(String),
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(msg) => write!(f, "Network transport error: {}", msg),
            Self::Timeout(msg) => write!(f, "Network timeout: {}", msg),
            Self::SimulatedNetworkDrop(msg) => write!(f, "Simulated network drop: {}", msg),
            Self::InvalidFrameMagic(m) => write!(f, "Invalid frame magic: {:?}", m),
            Self::ProtocolVersionMismatch { expected, actual } => write!(
                f,
                "Protocol version mismatch: expected v{}, got v{}",
                expected, actual
            ),
            Self::FrameTooLarge { field, size, limit } => write!(
                f,
                "Oversized network frame ({}): {} bytes exceeds limit {} bytes",
                field, size, limit
            ),
            Self::SerializationError(msg) => write!(f, "RPC serialization error: {}", msg),
            Self::Unauthorized(msg) => {
                write!(f, "RPC authentication/authorization failed: {}", msg)
            }
            Self::QuotaExceeded(msg) => write!(f, "Node storage quota exceeded: {}", msg),
            Self::ShardNotFound {
                object_id,
                shard_id,
                version,
            } => write!(
                f,
                "Shard not found on node: object={} shard={} v{}",
                object_id, shard_id, version
            ),
            Self::IntegrityFailure(msg) => write!(f, "Shard integrity failure: {}", msg),
            Self::InternalNodeError(msg) => write!(f, "Internal node error: {}", msg),
            Self::Fenced {
                current_epoch,
                presented_epoch,
            } => write!(
                f,
                "Coordinator fenced: current node epoch {} > presented epoch {}",
                current_epoch, presented_epoch
            ),
            Self::Io(msg) => write!(f, "Node I/O error: {}", msg),
        }
    }
}

impl std::error::Error for RpcError {}

/// Summary of a stored shard container reported by `ListShards`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredShardSummary {
    pub object_id: String,
    pub shard_id: usize,
    pub version: u64,
    pub stripe_count: u32,
    pub container_size_bytes: u64,
    pub container_body_sha256: String,
}

/// Request metadata for `PutShard` (accompanied by concatenated stripe shard bytes in the binary payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutShardRequest {
    pub tx_id: String,
    pub object_id: String,
    pub shard_id: usize,
    pub version: u64,
    pub coding_scheme: CodingScheme,
    /// `(payload_length, shard_length, expected_stripe_sha256_hex)` for each stripe in order.
    pub stripes: Vec<(usize, usize, String)>,
}

/// Authenticated acknowledgement returned by a storage node after persisting and verifying `PutShard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutShardAck {
    pub node_id: NodeId,
    pub tx_id: String,
    pub object_id: String,
    pub shard_id: usize,
    pub version: u64,
    pub stripe_count: u32,
    pub container_size_bytes: u64,
    pub container_body_sha256: String,
    pub header_and_index_sha256: String,
    pub idempotent_replay: bool,
    pub durability_level: DurabilityLevel,
}

/// Request metadata for `GetShard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetShardRequest {
    pub object_id: String,
    pub shard_id: usize,
    pub version: u64,
    /// If `Some(idx)`, fetches only stripe `idx` via bounded random access; if `None`, fetches all stripes.
    pub stripe_index: Option<usize>,
}

/// Response metadata for `GetShard` (accompanied by verified shard bytes in the binary payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetShardResponseMeta {
    pub node_id: NodeId,
    pub object_id: String,
    pub shard_id: usize,
    pub version: u64,
    /// `(stripe_index, shard_length, stripe_sha256_hex)` for each returned stripe.
    pub stripes: Vec<(usize, usize, String)>,
}

/// Request metadata for `VerifyShard` (full container verification + keyed challenge-response).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyShardRequest {
    pub object_id: String,
    pub shard_id: usize,
    pub version: u64,
    pub challenge_nonce_hex: String,
}

/// Integrity and challenge-response report returned by `VerifyShard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyShardReport {
    pub node_id: NodeId,
    pub object_id: String,
    pub shard_id: usize,
    pub version: u64,
    pub healthy: bool,
    pub stripe_count: u32,
    pub container_size_bytes: u64,
    pub container_body_sha256: String,
    /// `SHA256("FSP-SCRUB-CHALLENGE-V1:" || challenge_nonce_hex || ":" || raw_container_bytes)`
    pub challenge_response_sha256: String,
    pub error_reason: Option<String>,
}

/// Response returned by `Heartbeat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeartbeatResponse {
    pub node_id: NodeId,
    pub sequence: u64,
    pub health_state: String,
    pub used_bytes: u64,
    pub capacity_limit_bytes: u64,
    pub stored_shard_count: usize,
    pub uptime_ms: u64,
}

/// Detailed telemetry returned by `GetNodeStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeStatusResponse {
    pub node_id: NodeId,
    pub listen_addr: String,
    pub identity_id: String,
    pub cert_fingerprint_hex: String,
    pub health_state: String,
    pub used_bytes: u64,
    pub capacity_limit_bytes: u64,
    pub stored_shard_count: usize,
    pub requests_processed: u64,
    pub auth_failures_rejected: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

/// High-level RPC request variants sent over the framed TLS 1.3 transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcRequest {
    PutShard(PutShardRequest),
    PutShardFenced {
        request: PutShardRequest,
        epoch: u64,
    },
    GetShard(GetShardRequest),
    DeleteShard {
        tx_id: String,
        object_id: String,
        shard_id: usize,
        version: u64,
    },
    DeleteShardFenced {
        tx_id: String,
        object_id: String,
        shard_id: usize,
        version: u64,
        epoch: u64,
    },
    VerifyShard(VerifyShardRequest),
    ListShards {
        object_id_filter: Option<String>,
    },
    Heartbeat {
        sequence: u64,
        timestamp_unix_secs: u64,
    },
    GetNodeStatus,
}

impl RpcRequest {
    /// Maps this request to its corresponding [`RpcOperation`] for RBAC authorization.
    pub const fn operation(&self) -> RpcOperation {
        match self {
            Self::PutShard(_) | Self::PutShardFenced { .. } => RpcOperation::PutShard,
            Self::GetShard(_) => RpcOperation::GetShard,
            Self::DeleteShard { .. } | Self::DeleteShardFenced { .. } => RpcOperation::DeleteShard,
            Self::VerifyShard(_) => RpcOperation::VerifyShard,
            Self::ListShards { .. } => RpcOperation::ListShards,
            Self::Heartbeat { .. } => RpcOperation::Heartbeat,
            Self::GetNodeStatus => RpcOperation::GetNodeStatus,
        }
    }

    /// Serializes the request metadata into a deterministic line-oriented text block.
    pub fn encode_meta(&self) -> String {
        match self {
            Self::PutShard(req) => {
                let (scheme_str, n, k, l) = match req.coding_scheme {
                    CodingScheme::ReedSolomon { n, k } => ("rs", n, k, 0usize),
                    CodingScheme::Lrc(cfg) => {
                        ("lrc", cfg.total_shards, cfg.data_shards, cfg.locality)
                    }
                };
                let mut out = format!(
                    "OP=PutShard\ntx_id={}\nobject_id={}\nshard_id={}\nversion={}\nscheme={}:{}:{}:{}\nstripes={}\n",
                    req.tx_id,
                    req.object_id,
                    req.shard_id,
                    req.version,
                    scheme_str,
                    n,
                    k,
                    l,
                    req.stripes.len()
                );
                for (plen, slen, sha) in &req.stripes {
                    out.push_str(&format!("S={}:{}:{}\n", plen, slen, sha));
                }
                out
            }
            Self::PutShardFenced {
                request: req,
                epoch,
            } => {
                let (scheme_str, n, k, l) = match req.coding_scheme {
                    CodingScheme::ReedSolomon { n, k } => ("rs", n, k, 0usize),
                    CodingScheme::Lrc(cfg) => {
                        ("lrc", cfg.total_shards, cfg.data_shards, cfg.locality)
                    }
                };
                let mut out = format!(
                    "OP=PutShardFenced\nepoch={}\ntx_id={}\nobject_id={}\nshard_id={}\nversion={}\nscheme={}:{}:{}:{}\nstripes={}\n",
                    epoch,
                    req.tx_id,
                    req.object_id,
                    req.shard_id,
                    req.version,
                    scheme_str,
                    n,
                    k,
                    l,
                    req.stripes.len()
                );
                for (plen, slen, sha) in &req.stripes {
                    out.push_str(&format!("S={}:{}:{}\n", plen, slen, sha));
                }
                out
            }
            Self::GetShard(req) => {
                let s_str = match req.stripe_index {
                    Some(idx) => idx.to_string(),
                    None => "ALL".to_string(),
                };
                format!(
                    "OP=GetShard\nobject_id={}\nshard_id={}\nversion={}\nstripe={}\n",
                    req.object_id, req.shard_id, req.version, s_str
                )
            }
            Self::DeleteShard {
                tx_id,
                object_id,
                shard_id,
                version,
            } => format!(
                "OP=DeleteShard\ntx_id={}\nobject_id={}\nshard_id={}\nversion={}\n",
                tx_id, object_id, shard_id, version
            ),
            Self::DeleteShardFenced {
                tx_id,
                object_id,
                shard_id,
                version,
                epoch,
            } => format!(
                "OP=DeleteShardFenced\nepoch={}\ntx_id={}\nobject_id={}\nshard_id={}\nversion={}\n",
                epoch, tx_id, object_id, shard_id, version
            ),
            Self::VerifyShard(req) => format!(
                "OP=VerifyShard\nobject_id={}\nshard_id={}\nversion={}\nnonce={}\n",
                req.object_id, req.shard_id, req.version, req.challenge_nonce_hex
            ),
            Self::ListShards { object_id_filter } => {
                let f = object_id_filter.as_deref().unwrap_or("*");
                format!("OP=ListShards\nfilter={}\n", f)
            }
            Self::Heartbeat {
                sequence,
                timestamp_unix_secs,
            } => format!(
                "OP=Heartbeat\nseq={}\nts={}\n",
                sequence, timestamp_unix_secs
            ),
            Self::GetNodeStatus => "OP=GetNodeStatus\n".to_string(),
        }
    }

    /// Parses and validates an [`RpcRequest`] from its wire metadata string.
    pub fn decode_meta(meta: &str) -> Result<Self, RpcError> {
        let mut lines = meta.lines();
        let first = lines
            .next()
            .ok_or_else(|| RpcError::SerializationError("empty request meta".into()))?;
        let op = first
            .strip_prefix("OP=")
            .ok_or_else(|| RpcError::SerializationError("missing OP= header".into()))?;

        let mut kv: Vec<(&str, &str)> = Vec::new();
        for line in lines {
            if let Some((k, v)) = line.split_once('=') {
                kv.push((k, v));
            }
        }
        let get_val = |key: &str| -> Result<&str, RpcError> {
            kv.iter()
                .find(|&&(k, _)| k == key)
                .map(|&(_, v)| v)
                .ok_or_else(|| RpcError::SerializationError(format!("missing key {}", key)))
        };

        match op {
            "PutShard" | "PutShardFenced" => {
                let tx_id = get_val("tx_id")?.to_string();
                let object_id = get_val("object_id")?.to_string();
                if !validate_safe_identifier(&tx_id) || !validate_safe_identifier(&object_id) {
                    return Err(RpcError::SerializationError(
                        "unsafe tx_id or object_id".into(),
                    ));
                }
                let shard_id = get_val("shard_id")?
                    .parse::<usize>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let version = get_val("version")?
                    .parse::<u64>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let scheme_raw = get_val("scheme")?;
                let parts: Vec<&str> = scheme_raw.split(':').collect();
                if parts.len() != 4 {
                    return Err(RpcError::SerializationError("malformed scheme".into()));
                }
                let n = parts[1]
                    .parse::<usize>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let k = parts[2]
                    .parse::<usize>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let l = parts[3]
                    .parse::<usize>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let coding_scheme = match parts[0] {
                    "rs" => CodingScheme::ReedSolomon { n, k },
                    "lrc" => CodingScheme::Lrc(
                        LrcConfig::new(n, k, l)
                            .map_err(|e| RpcError::SerializationError(e.to_string()))?,
                    ),
                    other => {
                        return Err(RpcError::SerializationError(format!(
                            "unknown scheme {}",
                            other
                        )))
                    }
                };
                let expected_stripes = get_val("stripes")?
                    .parse::<usize>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let mut stripes = Vec::with_capacity(expected_stripes.min(16_384));
                for &(k_str, v_str) in &kv {
                    if k_str == "S" {
                        let sp: Vec<&str> = v_str.split(':').collect();
                        if sp.len() != 3 {
                            return Err(RpcError::SerializationError("malformed S entry".into()));
                        }
                        let plen = sp[0]
                            .parse::<usize>()
                            .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                        let slen = sp[1]
                            .parse::<usize>()
                            .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                        stripes.push((plen, slen, sp[2].to_string()));
                    }
                }
                if stripes.len() != expected_stripes || stripes.is_empty() {
                    return Err(RpcError::SerializationError(
                        "stripe count mismatch in PutShard".into(),
                    ));
                }
                let req = PutShardRequest {
                    tx_id,
                    object_id,
                    shard_id,
                    version,
                    coding_scheme,
                    stripes,
                };
                if op == "PutShardFenced" {
                    let epoch = get_val("epoch")?
                        .parse::<u64>()
                        .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                    Ok(Self::PutShardFenced {
                        request: req,
                        epoch,
                    })
                } else {
                    Ok(Self::PutShard(req))
                }
            }
            "GetShard" => {
                let object_id = get_val("object_id")?.to_string();
                if !validate_safe_identifier(&object_id) {
                    return Err(RpcError::SerializationError("unsafe object_id".into()));
                }
                let shard_id = get_val("shard_id")?
                    .parse::<usize>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let version = get_val("version")?
                    .parse::<u64>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let stripe_str = get_val("stripe")?;
                let stripe_index = if stripe_str == "ALL" {
                    None
                } else {
                    Some(
                        stripe_str
                            .parse::<usize>()
                            .map_err(|e| RpcError::SerializationError(e.to_string()))?,
                    )
                };
                Ok(Self::GetShard(GetShardRequest {
                    object_id,
                    shard_id,
                    version,
                    stripe_index,
                }))
            }
            "DeleteShard" | "DeleteShardFenced" => {
                let tx_id = get_val("tx_id")?.to_string();
                let object_id = get_val("object_id")?.to_string();
                if !validate_safe_identifier(&tx_id) || !validate_safe_identifier(&object_id) {
                    return Err(RpcError::SerializationError(
                        "unsafe tx_id or object_id".into(),
                    ));
                }
                let shard_id = get_val("shard_id")?
                    .parse::<usize>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let version = get_val("version")?
                    .parse::<u64>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                if op == "DeleteShardFenced" {
                    let epoch = get_val("epoch")?
                        .parse::<u64>()
                        .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                    Ok(Self::DeleteShardFenced {
                        tx_id,
                        object_id,
                        shard_id,
                        version,
                        epoch,
                    })
                } else {
                    Ok(Self::DeleteShard {
                        tx_id,
                        object_id,
                        shard_id,
                        version,
                    })
                }
            }
            "VerifyShard" => {
                let object_id = get_val("object_id")?.to_string();
                if !validate_safe_identifier(&object_id) {
                    return Err(RpcError::SerializationError("unsafe object_id".into()));
                }
                let shard_id = get_val("shard_id")?
                    .parse::<usize>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let version = get_val("version")?
                    .parse::<u64>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let challenge_nonce_hex = get_val("nonce")?.to_string();
                Ok(Self::VerifyShard(VerifyShardRequest {
                    object_id,
                    shard_id,
                    version,
                    challenge_nonce_hex,
                }))
            }
            "ListShards" => {
                let f = get_val("filter")?;
                let object_id_filter = if f == "*" {
                    None
                } else {
                    if !validate_safe_identifier(f) {
                        return Err(RpcError::SerializationError("unsafe filter".into()));
                    }
                    Some(f.to_string())
                };
                Ok(Self::ListShards { object_id_filter })
            }
            "Heartbeat" => {
                let sequence = get_val("seq")?
                    .parse::<u64>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                let timestamp_unix_secs = get_val("ts")?
                    .parse::<u64>()
                    .map_err(|e| RpcError::SerializationError(e.to_string()))?;
                Ok(Self::Heartbeat {
                    sequence,
                    timestamp_unix_secs,
                })
            }
            "GetNodeStatus" => Ok(Self::GetNodeStatus),
            other => Err(RpcError::SerializationError(format!(
                "unknown RPC operation {:?}",
                other
            ))),
        }
    }
}

/// High-level RPC response variants returned over the framed TLS 1.3 transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcResponse {
    PutShard(PutShardAck),
    GetShard(GetShardResponseMeta),
    DeleteShard { deleted: bool },
    VerifyShard(VerifyShardReport),
    ListShards(Vec<StoredShardSummary>),
    Heartbeat(HeartbeatResponse),
    GetNodeStatus(NodeStatusResponse),
    Error { code: String, message: String },
}

impl RpcResponse {
    /// Serializes the response metadata into a deterministic line-oriented text block.
    pub fn encode_meta(&self) -> String {
        match self {
            Self::PutShard(ack) => format!(
                "STATUS=OK\nKIND=PutShard\nnode_id={}\ntx_id={}\nobject_id={}\nshard_id={}\nversion={}\nstripes={}\nsize={}\nbody_sha={}\nhdr_sha={}\nreplay={}\ndurability={}\n",
                ack.node_id.0,
                ack.tx_id,
                ack.object_id,
                ack.shard_id,
                ack.version,
                ack.stripe_count,
                ack.container_size_bytes,
                ack.container_body_sha256,
                ack.header_and_index_sha256,
                ack.idempotent_replay,
                ack.durability_level.as_str(),
            ),
            Self::GetShard(meta) => {
                let mut out = format!(
                    "STATUS=OK\nKIND=GetShard\nnode_id={}\nobject_id={}\nshard_id={}\nversion={}\nstripes={}\n",
                    meta.node_id.0,
                    meta.object_id,
                    meta.shard_id,
                    meta.version,
                    meta.stripes.len()
                );
                for (idx, slen, sha) in &meta.stripes {
                    out.push_str(&format!("S={}:{}:{}\n", idx, slen, sha));
                }
                out
            }
            Self::DeleteShard { deleted } => {
                format!("STATUS=OK\nKIND=DeleteShard\ndeleted={}\n", deleted)
            }
            Self::VerifyShard(rep) => format!(
                "STATUS=OK\nKIND=VerifyShard\nnode_id={}\nobject_id={}\nshard_id={}\nversion={}\nhealthy={}\nstripes={}\nsize={}\nbody_sha={}\nresp_sha={}\nerr={}\n",
                rep.node_id.0,
                rep.object_id,
                rep.shard_id,
                rep.version,
                rep.healthy,
                rep.stripe_count,
                rep.container_size_bytes,
                rep.container_body_sha256,
                rep.challenge_response_sha256,
                rep.error_reason.as_deref().unwrap_or("NONE"),
            ),
            Self::ListShards(list) => {
                let mut out = format!("STATUS=OK\nKIND=ListShards\ncount={}\n", list.len());
                for item in list {
                    out.push_str(&format!(
                        "ITEM={}:{}:{}:{}:{}:{}\n",
                        item.object_id,
                        item.shard_id,
                        item.version,
                        item.stripe_count,
                        item.container_size_bytes,
                        item.container_body_sha256
                    ));
                }
                out
            }
            Self::Heartbeat(hb) => format!(
                "STATUS=OK\nKIND=Heartbeat\nnode_id={}\nseq={}\nhealth={}\nused={}\ncap={}\nshards={}\nuptime={}\n",
                hb.node_id.0,
                hb.sequence,
                hb.health_state,
                hb.used_bytes,
                hb.capacity_limit_bytes,
                hb.stored_shard_count,
                hb.uptime_ms,
            ),
            Self::GetNodeStatus(st) => format!(
                "STATUS=OK\nKIND=GetNodeStatus\nnode_id={}\naddr={}\nid={}\nfp={}\nhealth={}\nused={}\ncap={}\nshards={}\nreqs={}\nauth_fail={}\nrx={}\ntx={}\n",
                st.node_id.0,
                st.listen_addr,
                st.identity_id,
                st.cert_fingerprint_hex,
                st.health_state,
                st.used_bytes,
                st.capacity_limit_bytes,
                st.stored_shard_count,
                st.requests_processed,
                st.auth_failures_rejected,
                st.bytes_received,
                st.bytes_sent,
            ),
            Self::Error { code, message } => format!(
                "STATUS=ERR\ncode={}\nmsg={}\n",
                code,
                message.replace('\n', " ")
            ),
        }
    }

    /// Parses an [`RpcResponse`] from its wire metadata string.
    pub fn decode_meta(meta: &str) -> Result<Self, RpcError> {
        let mut kv: Vec<(&str, &str)> = Vec::new();
        for line in meta.lines() {
            if let Some((k, v)) = line.split_once('=') {
                kv.push((k, v));
            }
        }
        let get_val = |key: &str| -> Result<&str, RpcError> {
            kv.iter()
                .find(|&&(k, _)| k == key)
                .map(|&(_, v)| v)
                .ok_or_else(|| RpcError::SerializationError(format!("missing key {}", key)))
        };

        let status = get_val("STATUS")?;
        if status == "ERR" {
            let code = get_val("code")?.to_string();
            let message = get_val("msg")?.to_string();
            return Ok(Self::Error { code, message });
        }

        let kind = get_val("KIND")?;
        match kind {
            "PutShard" => Ok(Self::PutShard(PutShardAck {
                node_id: NodeId(get_val("node_id")?.parse().unwrap_or(0)),
                tx_id: get_val("tx_id")?.to_string(),
                object_id: get_val("object_id")?.to_string(),
                shard_id: get_val("shard_id")?.parse().unwrap_or(0),
                version: get_val("version")?.parse().unwrap_or(1),
                stripe_count: get_val("stripes")?.parse().unwrap_or(1),
                container_size_bytes: get_val("size")?.parse().unwrap_or(0),
                container_body_sha256: get_val("body_sha")?.to_string(),
                header_and_index_sha256: get_val("hdr_sha")?.to_string(),
                idempotent_replay: get_val("replay")? == "true",
                durability_level: get_val("durability")
                    .ok()
                    .and_then(|s| DurabilityLevel::from_str(s).ok())
                    .unwrap_or(DurabilityLevel::LocallyDurable),
            })),
            "GetShard" => {
                let mut stripes = Vec::new();
                for &(k, v) in &kv {
                    if k == "S" {
                        let sp: Vec<&str> = v.split(':').collect();
                        if sp.len() == 3 {
                            stripes.push((
                                sp[0].parse().unwrap_or(0),
                                sp[1].parse().unwrap_or(0),
                                sp[2].to_string(),
                            ));
                        }
                    }
                }
                Ok(Self::GetShard(GetShardResponseMeta {
                    node_id: NodeId(get_val("node_id")?.parse().unwrap_or(0)),
                    object_id: get_val("object_id")?.to_string(),
                    shard_id: get_val("shard_id")?.parse().unwrap_or(0),
                    version: get_val("version")?.parse().unwrap_or(1),
                    stripes,
                }))
            }
            "DeleteShard" => Ok(Self::DeleteShard {
                deleted: get_val("deleted")? == "true",
            }),
            "VerifyShard" => {
                let err_raw = get_val("err")?;
                Ok(Self::VerifyShard(VerifyShardReport {
                    node_id: NodeId(get_val("node_id")?.parse().unwrap_or(0)),
                    object_id: get_val("object_id")?.to_string(),
                    shard_id: get_val("shard_id")?.parse().unwrap_or(0),
                    version: get_val("version")?.parse().unwrap_or(1),
                    healthy: get_val("healthy")? == "true",
                    stripe_count: get_val("stripes")?.parse().unwrap_or(0),
                    container_size_bytes: get_val("size")?.parse().unwrap_or(0),
                    container_body_sha256: get_val("body_sha")?.to_string(),
                    challenge_response_sha256: get_val("resp_sha")?.to_string(),
                    error_reason: if err_raw == "NONE" {
                        None
                    } else {
                        Some(err_raw.to_string())
                    },
                }))
            }
            "ListShards" => {
                let mut list = Vec::new();
                for &(k, v) in &kv {
                    if k == "ITEM" {
                        let sp: Vec<&str> = v.split(':').collect();
                        if sp.len() == 6 {
                            list.push(StoredShardSummary {
                                object_id: sp[0].to_string(),
                                shard_id: sp[1].parse().unwrap_or(0),
                                version: sp[2].parse().unwrap_or(1),
                                stripe_count: sp[3].parse().unwrap_or(1),
                                container_size_bytes: sp[4].parse().unwrap_or(0),
                                container_body_sha256: sp[5].to_string(),
                            });
                        }
                    }
                }
                Ok(Self::ListShards(list))
            }
            "Heartbeat" => Ok(Self::Heartbeat(HeartbeatResponse {
                node_id: NodeId(get_val("node_id")?.parse().unwrap_or(0)),
                sequence: get_val("seq")?.parse().unwrap_or(0),
                health_state: get_val("health")?.to_string(),
                used_bytes: get_val("used")?.parse().unwrap_or(0),
                capacity_limit_bytes: get_val("cap")?.parse().unwrap_or(0),
                stored_shard_count: get_val("shards")?.parse().unwrap_or(0),
                uptime_ms: get_val("uptime")?.parse().unwrap_or(0),
            })),
            "GetNodeStatus" => Ok(Self::GetNodeStatus(NodeStatusResponse {
                node_id: NodeId(get_val("node_id")?.parse().unwrap_or(0)),
                listen_addr: get_val("addr")?.to_string(),
                identity_id: get_val("id")?.to_string(),
                cert_fingerprint_hex: get_val("fp")?.to_string(),
                health_state: get_val("health")?.to_string(),
                used_bytes: get_val("used")?.parse().unwrap_or(0),
                capacity_limit_bytes: get_val("cap")?.parse().unwrap_or(0),
                stored_shard_count: get_val("shards")?.parse().unwrap_or(0),
                requests_processed: get_val("reqs")?.parse().unwrap_or(0),
                auth_failures_rejected: get_val("auth_fail")?.parse().unwrap_or(0),
                bytes_received: get_val("rx")?.parse().unwrap_or(0),
                bytes_sent: get_val("tx")?.parse().unwrap_or(0),
            })),
            other => Err(RpcError::SerializationError(format!(
                "unknown response KIND {:?}",
                other
            ))),
        }
    }
}
