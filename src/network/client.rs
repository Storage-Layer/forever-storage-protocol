//! Mutual TLS 1.3 (`mTLS`) RPC Client for FSP Coordinator and peer-node communication (`src/network/client.rs`).

use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, StreamOwned};

use crate::model::node::NodeId;
use crate::network::protocol::{
    GetShardRequest, GetShardResponseMeta, HeartbeatResponse, NodeStatusResponse, PutShardAck,
    PutShardRequest, RpcError, RpcRequest, RpcResponse, StoredShardSummary, VerifyShardReport,
    VerifyShardRequest, MAX_RPC_PAYLOAD_BYTES,
};
use crate::network::transport::{
    read_rpc_frame, write_rpc_frame, write_rpc_frame_with_version, NetworkFaultPolicy,
};
use crate::security::authorization::ClusterTrustPolicy;
use crate::storage::integrity::sha256_bytes;

/// Mutual TLS 1.3 client enforcing server X.509 certificate verification, [`NodeId`] identity
/// binding, socket timeouts, bounded frame sizes, and optional deterministic fault injection.
#[derive(Debug, Clone)]
pub struct NodeRpcClient {
    tls_config: Arc<RwLock<Arc<ClientConfig>>>,
    trust_policy: Arc<RwLock<ClusterTrustPolicy>>,
    fault_policy: NetworkFaultPolicy,
    socket_timeout: Duration,
    simulated_clock_unix_secs: Arc<AtomicU64>,
}

impl NodeRpcClient {
    /// Creates a new [`NodeRpcClient`].
    pub fn new(
        tls_config: Arc<ClientConfig>,
        trust_policy: ClusterTrustPolicy,
        socket_timeout: Duration,
    ) -> Self {
        Self {
            tls_config: Arc::new(RwLock::new(tls_config)),
            trust_policy: Arc::new(RwLock::new(trust_policy)),
            fault_policy: NetworkFaultPolicy::default(),
            socket_timeout,
            simulated_clock_unix_secs: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Returns a reference to the [`NetworkFaultPolicy`] for deterministic test fault injection.
    pub const fn fault_policy(&self) -> &NetworkFaultPolicy {
        &self.fault_policy
    }

    /// Replaces the [`NetworkFaultPolicy`].
    pub fn set_fault_policy(&mut self, policy: NetworkFaultPolicy) {
        self.fault_policy = policy;
    }

    /// Returns a shared handle to the client's [`ClusterTrustPolicy`].
    pub fn trust_policy(&self) -> &Arc<RwLock<ClusterTrustPolicy>> {
        &self.trust_policy
    }

    /// Hot-reloads the client's TLS 1.3 [`ClientConfig`] (e.g. after rotating the coordinator's leaf certificate).
    pub fn reload_tls_config(&self, new_config: Arc<ClientConfig>) {
        *self.tls_config.write().unwrap_or_else(|p| p.into_inner()) = new_config;
    }

    /// Sets an override Unix timestamp (`> 0`) for deterministic certificate-expiration testing.
    pub fn set_simulated_clock_unix_secs(&self, unix_secs: u64) {
        self.simulated_clock_unix_secs
            .store(unix_secs, Ordering::Release);
    }

    fn current_unix_secs(&self) -> u64 {
        let sim = self.simulated_clock_unix_secs.load(Ordering::Acquire);
        if sim > 0 {
            return sim;
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(1_700_000_000)
    }

    /// Executes a raw [`RpcRequest`] and `payload` against `expected_node_id` at `addr`.
    pub fn send_request(
        &self,
        expected_node_id: NodeId,
        addr: &str,
        request: &RpcRequest,
        payload: &[u8],
    ) -> Result<(RpcResponse, Vec<u8>), RpcError> {
        self.fault_policy.check_before_request(expected_node_id)?;

        let mut tls_stream = self.connect_and_verify_mtls(expected_node_id, addr)?;
        let meta = request.encode_meta();
        write_rpc_frame(&mut tls_stream, &meta, payload)?;

        let (resp_meta, mut resp_payload, _) =
            read_rpc_frame(&mut tls_stream, MAX_RPC_PAYLOAD_BYTES)?;
        if matches!(request, RpcRequest::GetShard(_)) {
            self.fault_policy
                .maybe_corrupt_payload(expected_node_id, &mut resp_payload);
        }

        let resp = RpcResponse::decode_meta(&resp_meta)?;
        match resp {
            RpcResponse::Error { code, message } => Err(map_error_code(&code, message)),
            other => Ok((other, resp_payload)),
        }
    }

    /// Sends a custom-versioned raw frame to `addr` (for testing protocol version mismatch or oversized frame rejection).
    pub fn send_raw_frame_with_version(
        &self,
        expected_node_id: NodeId,
        addr: &str,
        version: u16,
        meta: &str,
        payload: &[u8],
    ) -> Result<RpcResponse, RpcError> {
        let mut tls_stream = self.connect_and_verify_mtls(expected_node_id, addr)?;
        write_rpc_frame_with_version(&mut tls_stream, version, meta, payload)?;
        let (resp_meta, _, _) = read_rpc_frame(&mut tls_stream, MAX_RPC_PAYLOAD_BYTES)?;
        let resp = RpcResponse::decode_meta(&resp_meta)?;
        match resp {
            RpcResponse::Error { code, message } => Err(map_error_code(&code, message)),
            other => Ok(other),
        }
    }

    fn connect_and_verify_mtls(
        &self,
        expected_node_id: NodeId,
        addr: &str,
    ) -> Result<StreamOwned<ClientConnection, TcpStream>, RpcError> {
        let sock_addr: SocketAddr = addr
            .parse()
            .map_err(|e| RpcError::Transport(format!("invalid socket addr {:?}: {}", addr, e)))?;
        let tcp_stream =
            TcpStream::connect_timeout(&sock_addr, self.socket_timeout).map_err(|e| {
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) {
                    RpcError::Timeout(format!("connect {}: {}", addr, e))
                } else {
                    RpcError::Transport(format!("connect {}: {}", addr, e))
                }
            })?;
        let _ = tcp_stream.set_nodelay(true);
        let _ = tcp_stream.set_read_timeout(Some(self.socket_timeout));
        let _ = tcp_stream.set_write_timeout(Some(self.socket_timeout));

        let host = addr.split(':').next().unwrap_or("localhost");
        let server_name = ServerName::try_from(host.to_string())
            .or_else(|_| ServerName::try_from("localhost".to_string()))
            .map_err(|e| RpcError::Transport(format!("invalid server name: {}", e)))?;
        let tls_cfg = self
            .tls_config
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let client_conn = ClientConnection::new(tls_cfg, server_name)
            .map_err(|e| RpcError::Unauthorized(format!("TLS client init: {}", e)))?;
        let mut tls_stream = StreamOwned::new(client_conn, tcp_stream);

        while tls_stream.conn.is_handshaking() {
            tls_stream
                .conn
                .complete_io(&mut tls_stream.sock)
                .map_err(|e| RpcError::Unauthorized(format!("mTLS client handshake: {}", e)))?;
        }

        // Verify that the storage node's X.509 certificate is trusted, unexpired, and bound to `expected_node_id`!
        let peer_certs = tls_stream.conn.peer_certificates();
        let now_secs = self.current_unix_secs();
        {
            let tp = self.trust_policy.read().unwrap_or_else(|p| p.into_inner());
            tp.verify_peer_certificate(peer_certs, Some(expected_node_id), now_secs)
                .map_err(|e| RpcError::Unauthorized(e.to_string()))?;
        }

        Ok(tls_stream)
    }

    /// Invokes `PutShard` on `node_id` at `addr`.
    pub fn put_shard(
        &self,
        node_id: NodeId,
        addr: &str,
        req: PutShardRequest,
        payload: &[u8],
    ) -> Result<PutShardAck, RpcError> {
        let (resp, _) = self.send_request(node_id, addr, &RpcRequest::PutShard(req), payload)?;
        match resp {
            RpcResponse::PutShard(ack) => {
                if ack.node_id != node_id {
                    return Err(RpcError::Unauthorized(format!(
                        "PutShardAck node_id mismatch: expected {:?}, got {:?}",
                        node_id, ack.node_id
                    )));
                }
                Ok(ack)
            }
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to PutShard: {:?}",
                other
            ))),
        }
    }

    /// Invokes `PutShardFenced` with an explicit coordinator leadership epoch.
    pub fn put_shard_fenced(
        &self,
        node_id: NodeId,
        addr: &str,
        req: PutShardRequest,
        payload: &[u8],
        epoch: u64,
    ) -> Result<PutShardAck, RpcError> {
        let (resp, _) = self.send_request(
            node_id,
            addr,
            &RpcRequest::PutShardFenced {
                request: req,
                epoch,
            },
            payload,
        )?;
        match resp {
            RpcResponse::PutShard(ack) => {
                if ack.node_id != node_id {
                    return Err(RpcError::Unauthorized(format!(
                        "PutShardAck node_id mismatch: expected {:?}, got {:?}",
                        node_id, ack.node_id
                    )));
                }
                Ok(ack)
            }
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to PutShardFenced: {:?}",
                other
            ))),
        }
    }

    /// Invokes `DeleteShardFenced` with an explicit coordinator leadership epoch.
    #[allow(clippy::too_many_arguments)]
    pub fn delete_shard_fenced(
        &self,
        node_id: NodeId,
        addr: &str,
        tx_id: String,
        object_id: String,
        shard_id: usize,
        version: u64,
        epoch: u64,
    ) -> Result<bool, RpcError> {
        let req = RpcRequest::DeleteShardFenced {
            tx_id,
            object_id,
            shard_id,
            version,
            epoch,
        };
        let (resp, _) = self.send_request(node_id, addr, &req, &[])?;
        match resp {
            RpcResponse::DeleteShard { deleted } => Ok(deleted),
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to DeleteShardFenced: {:?}",
                other
            ))),
        }
    }

    /// Invokes `GetShard` on `node_id` at `addr` and cryptographically verifies each returned stripe's SHA-256.
    pub fn get_shard(
        &self,
        node_id: NodeId,
        addr: &str,
        req: GetShardRequest,
    ) -> Result<(GetShardResponseMeta, Vec<u8>), RpcError> {
        let (resp, payload) = self.send_request(node_id, addr, &RpcRequest::GetShard(req), &[])?;
        match resp {
            RpcResponse::GetShard(meta) => {
                // Verify each stripe's SHA-256 on the client side before returning!
                let mut offset = 0usize;
                for (stripe_idx, slen, expected_sha) in &meta.stripes {
                    let end = offset.checked_add(*slen).ok_or_else(|| {
                        RpcError::IntegrityFailure("GetShard stripe length overflow".into())
                    })?;
                    if end > payload.len() {
                        return Err(RpcError::IntegrityFailure(format!(
                            "GetShard truncated payload from node-{:02}",
                            node_id.0
                        )));
                    }
                    let actual_sha = sha256_bytes(&payload[offset..end]).to_hex();
                    if &actual_sha != expected_sha {
                        return Err(RpcError::IntegrityFailure(format!(
                            "GetShard wire integrity failure from node-{:02} stripe {}: expected {}, got {}",
                            node_id.0, stripe_idx, expected_sha, actual_sha
                        )));
                    }
                    offset = end;
                }
                Ok((meta, payload))
            }
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to GetShard: {:?}",
                other
            ))),
        }
    }

    /// Invokes `DeleteShard` on `node_id` at `addr`.
    pub fn delete_shard(
        &self,
        node_id: NodeId,
        addr: &str,
        tx_id: &str,
        object_id: &str,
        shard_id: usize,
        version: u64,
    ) -> Result<bool, RpcError> {
        let (resp, _) = self.send_request(
            node_id,
            addr,
            &RpcRequest::DeleteShard {
                tx_id: tx_id.to_string(),
                object_id: object_id.to_string(),
                shard_id,
                version,
            },
            &[],
        )?;
        match resp {
            RpcResponse::DeleteShard { deleted } => Ok(deleted),
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to DeleteShard: {:?}",
                other
            ))),
        }
    }

    /// Invokes `VerifyShard` on `node_id` at `addr`.
    pub fn verify_shard(
        &self,
        node_id: NodeId,
        addr: &str,
        req: VerifyShardRequest,
    ) -> Result<VerifyShardReport, RpcError> {
        let (resp, _) = self.send_request(node_id, addr, &RpcRequest::VerifyShard(req), &[])?;
        match resp {
            RpcResponse::VerifyShard(rep) => Ok(rep),
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to VerifyShard: {:?}",
                other
            ))),
        }
    }

    /// Invokes `ListShards` on `node_id` at `addr`.
    pub fn list_shards(
        &self,
        node_id: NodeId,
        addr: &str,
        object_id_filter: Option<String>,
    ) -> Result<Vec<StoredShardSummary>, RpcError> {
        let (resp, _) = self.send_request(
            node_id,
            addr,
            &RpcRequest::ListShards { object_id_filter },
            &[],
        )?;
        match resp {
            RpcResponse::ListShards(list) => Ok(list),
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to ListShards: {:?}",
                other
            ))),
        }
    }

    /// Invokes `Heartbeat` on `node_id` at `addr`.
    pub fn heartbeat(
        &self,
        node_id: NodeId,
        addr: &str,
        sequence: u64,
        timestamp_unix_secs: u64,
    ) -> Result<HeartbeatResponse, RpcError> {
        let (resp, _) = self.send_request(
            node_id,
            addr,
            &RpcRequest::Heartbeat {
                sequence,
                timestamp_unix_secs,
            },
            &[],
        )?;
        match resp {
            RpcResponse::Heartbeat(hb) => Ok(hb),
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to Heartbeat: {:?}",
                other
            ))),
        }
    }

    /// Invokes `GetNodeStatus` on `node_id` at `addr`.
    pub fn get_node_status(
        &self,
        node_id: NodeId,
        addr: &str,
    ) -> Result<NodeStatusResponse, RpcError> {
        let (resp, _) = self.send_request(node_id, addr, &RpcRequest::GetNodeStatus, &[])?;
        match resp {
            RpcResponse::GetNodeStatus(st) => Ok(st),
            other => Err(RpcError::SerializationError(format!(
                "unexpected response to GetNodeStatus: {:?}",
                other
            ))),
        }
    }
}

fn map_error_code(code: &str, message: String) -> RpcError {
    match code {
        "Unauthorized" => RpcError::Unauthorized(message),
        "QuotaExceeded" => RpcError::QuotaExceeded(message),
        "ShardNotFound" => RpcError::ShardNotFound {
            object_id: message,
            shard_id: 0,
            version: 0,
        },
        "IntegrityFailure" => RpcError::IntegrityFailure(message),
        "ProtocolVersionMismatch" => RpcError::ProtocolVersionMismatch {
            expected: 1,
            actual: 99,
        },
        "FrameTooLarge" => RpcError::FrameTooLarge {
            field: "frame",
            size: u32::MAX,
            limit: MAX_RPC_PAYLOAD_BYTES,
        },
        "SerializationError" => RpcError::SerializationError(message),
        "Fenced" => {
            let mut parts = message.split(':');
            let cur = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let pres = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            RpcError::Fenced {
                current_epoch: cur,
                presented_epoch: pres,
            }
        }
        _ => RpcError::InternalNodeError(message),
    }
}
