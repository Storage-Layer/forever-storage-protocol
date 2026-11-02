//! Framed binary RPC transport over TLS 1.3 (`rustls`) with strict frame-size bounds, protocol
//! version validation, socket timeouts, and deterministic network fault injection (`src/network/transport.rs`).

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::model::node::NodeId;
use crate::network::protocol::{
    RpcError, FRAME_MAGIC, MAX_RPC_META_BYTES, MAX_RPC_PAYLOAD_BYTES, NETWORK_PROTOCOL_VERSION,
};

/// 14-byte wire frame header (`magic[4] || version_le[2] || meta_len_le[4] || payload_len_le[4]`).
pub const FRAME_HEADER_BYTES: usize = 14;

/// Writes a framed RPC message (`version`, `meta`, `payload`) to `stream`.
pub fn write_rpc_frame_with_version<W: Write>(
    stream: &mut W,
    version: u16,
    meta: &str,
    payload: &[u8],
) -> Result<u64, RpcError> {
    let meta_bytes = meta.as_bytes();
    if meta_bytes.len() > (MAX_RPC_META_BYTES as usize) {
        return Err(RpcError::FrameTooLarge {
            field: "meta_len",
            size: meta_bytes.len() as u32,
            limit: MAX_RPC_META_BYTES,
        });
    }
    if payload.len() > (MAX_RPC_PAYLOAD_BYTES as usize) {
        return Err(RpcError::FrameTooLarge {
            field: "payload_len",
            size: payload.len() as u32,
            limit: MAX_RPC_PAYLOAD_BYTES,
        });
    }

    let mut hdr = [0u8; FRAME_HEADER_BYTES];
    hdr[0..4].copy_from_slice(&FRAME_MAGIC);
    hdr[4..6].copy_from_slice(&version.to_le_bytes());
    hdr[6..10].copy_from_slice(&(meta_bytes.len() as u32).to_le_bytes());
    hdr[10..14].copy_from_slice(&(payload.len() as u32).to_le_bytes());

    stream
        .write_all(&hdr)
        .map_err(|e| map_io_to_rpc_err("write header", e))?;
    stream
        .write_all(meta_bytes)
        .map_err(|e| map_io_to_rpc_err("write meta", e))?;
    if !payload.is_empty() {
        stream
            .write_all(payload)
            .map_err(|e| map_io_to_rpc_err("write payload", e))?;
    }
    stream
        .flush()
        .map_err(|e| map_io_to_rpc_err("flush frame", e))?;

    Ok((FRAME_HEADER_BYTES + meta_bytes.len() + payload.len()) as u64)
}

/// Writes a standard v1 framed RPC message to `stream`.
pub fn write_rpc_frame<W: Write>(
    stream: &mut W,
    meta: &str,
    payload: &[u8],
) -> Result<u64, RpcError> {
    write_rpc_frame_with_version(stream, NETWORK_PROTOCOL_VERSION, meta, payload)
}

/// Reads and validates a framed RPC message from `stream`, enforcing `max_payload_bytes` BEFORE
/// allocating any payload buffer.
pub fn read_rpc_frame<R: Read>(
    stream: &mut R,
    max_payload_bytes: u32,
) -> Result<(String, Vec<u8>, u64), RpcError> {
    let mut hdr = [0u8; FRAME_HEADER_BYTES];
    stream
        .read_exact(&mut hdr)
        .map_err(|e| map_io_to_rpc_err("read header", e))?;

    let mut magic = [0u8; 4];
    magic.copy_from_slice(&hdr[0..4]);
    if magic != FRAME_MAGIC {
        return Err(RpcError::InvalidFrameMagic(magic));
    }

    let version = u16::from_le_bytes(hdr[4..6].try_into().unwrap());
    if version != NETWORK_PROTOCOL_VERSION {
        return Err(RpcError::ProtocolVersionMismatch {
            expected: NETWORK_PROTOCOL_VERSION,
            actual: version,
        });
    }

    let meta_len = u32::from_le_bytes(hdr[6..10].try_into().unwrap());
    let payload_len = u32::from_le_bytes(hdr[10..14].try_into().unwrap());

    if meta_len == 0 || meta_len > MAX_RPC_META_BYTES {
        return Err(RpcError::FrameTooLarge {
            field: "meta_len",
            size: meta_len,
            limit: MAX_RPC_META_BYTES,
        });
    }
    let effective_limit = max_payload_bytes.min(MAX_RPC_PAYLOAD_BYTES);
    if payload_len > effective_limit {
        return Err(RpcError::FrameTooLarge {
            field: "payload_len",
            size: payload_len,
            limit: effective_limit,
        });
    }

    let mut meta_buf = vec![0u8; meta_len as usize];
    stream
        .read_exact(&mut meta_buf)
        .map_err(|e| map_io_to_rpc_err("read meta", e))?;
    let meta_str = String::from_utf8(meta_buf)
        .map_err(|e| RpcError::SerializationError(format!("non-UTF8 frame meta: {}", e)))?;

    let mut payload_buf = vec![0u8; payload_len as usize];
    if payload_len > 0 {
        stream
            .read_exact(&mut payload_buf)
            .map_err(|e| map_io_to_rpc_err("read payload", e))?;
    }

    let total_bytes = (FRAME_HEADER_BYTES as u64) + (meta_len as u64) + (payload_len as u64);
    Ok((meta_str, payload_buf, total_bytes))
}

fn map_io_to_rpc_err(stage: &str, err: std::io::Error) -> RpcError {
    if matches!(
        err.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        RpcError::Timeout(format!("{}: {}", stage, err))
    } else {
        RpcError::Transport(format!("{}: {}", stage, err))
    }
}

/// Deterministic transport-layer fault injector for testing network partitions, packet loss,
/// delayed heartbeats, and corrupted network shard responses in local integration clusters.
#[derive(Debug, Clone)]
pub struct NetworkFaultPolicy {
    inner: Arc<Mutex<FaultPolicyState>>,
    prng_state: Arc<AtomicU64>,
}

#[derive(Debug, Default)]
struct FaultPolicyState {
    partitioned_nodes: BTreeSet<NodeId>,
    corrupt_response_nodes: BTreeSet<NodeId>,
    delayed_nodes: std::collections::BTreeMap<NodeId, Duration>,
    packet_loss_rate: f64,
}

impl Default for NetworkFaultPolicy {
    fn default() -> Self {
        Self::new(0xF594_0000_0000_0001)
    }
}

impl NetworkFaultPolicy {
    /// Creates a new [`NetworkFaultPolicy`] with deterministic PRNG `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FaultPolicyState::default())),
            prng_state: Arc::new(AtomicU64::new(seed)),
        }
    }

    /// Partitions or heals `node_id` from the coordinator.
    pub fn set_node_partitioned(&self, node_id: NodeId, partitioned: bool) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if partitioned {
            guard.partitioned_nodes.insert(node_id);
        } else {
            guard.partitioned_nodes.remove(&node_id);
        }
    }

    /// Configures `node_id` to flip a byte in its `GetShard` payload over the wire.
    pub fn set_corrupt_get_response(&self, node_id: NodeId, corrupt: bool) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if corrupt {
            guard.corrupt_response_nodes.insert(node_id);
        } else {
            guard.corrupt_response_nodes.remove(&node_id);
        }
    }

    /// Configures artificial network delay on requests to `node_id`.
    pub fn set_node_delay(&self, node_id: NodeId, delay: Option<Duration>) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        match delay {
            Some(d) => {
                guard.delayed_nodes.insert(node_id, d);
            }
            None => {
                guard.delayed_nodes.remove(&node_id);
            }
        }
    }

    /// Configures uniform deterministic packet drop rate in `[0.0, 1.0)`.
    pub fn set_packet_loss_rate(&self, rate: f64) {
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        guard.packet_loss_rate = rate.clamp(0.0, 1.0);
    }

    /// Evaluates whether a request to `node_id` should be dropped, delayed, or allowed.
    pub fn check_before_request(&self, node_id: NodeId) -> Result<(), RpcError> {
        let (is_part, delay_opt, loss_rate) = {
            let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            (
                guard.partitioned_nodes.contains(&node_id),
                guard.delayed_nodes.get(&node_id).copied(),
                guard.packet_loss_rate,
            )
        };
        if is_part {
            return Err(RpcError::SimulatedNetworkDrop(format!(
                "node-{:02} is partitioned",
                node_id.0
            )));
        }
        if let Some(delay) = delay_opt {
            std::thread::sleep(delay);
        }
        if loss_rate > 0.0 {
            let s = self
                .prng_state
                .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
                .wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = s;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            let word = z ^ (z >> 31);
            let u = ((word >> 11) as f64) * (1.0 / ((1u64 << 53) as f64));
            if u < loss_rate {
                return Err(RpcError::SimulatedNetworkDrop(format!(
                    "simulated packet loss on node-{:02} (u={:.4} < {:.4})",
                    node_id.0, u, loss_rate
                )));
            }
        }
        Ok(())
    }

    /// Applies response corruption if configured for `node_id`.
    pub fn maybe_corrupt_payload(&self, node_id: NodeId, payload: &mut [u8]) {
        let should_corrupt = {
            let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            guard.corrupt_response_nodes.contains(&node_id)
        };
        if should_corrupt && !payload.is_empty() {
            payload[0] ^= 0xFF;
        }
    }
}
