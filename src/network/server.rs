//! Mutual TLS 1.3 (`mTLS`) RPC Server for independent FSP storage node daemons (`src/network/server.rs`).

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rustls::{ServerConfig, ServerConnection, StreamOwned};

use crate::network::protocol::{RpcError, RpcRequest, RpcResponse, MAX_RPC_PAYLOAD_BYTES};
use crate::network::transport::{read_rpc_frame, write_rpc_frame};
use crate::node::daemon::StorageNodeDaemon;
use crate::node::health::NodeHealthState;
use crate::security::authorization::ClusterTrustPolicy;

/// Active multi-threaded Mutual TLS 1.3 RPC server wrapping a [`StorageNodeDaemon`].
pub struct NodeRpcServer {
    daemon: Arc<StorageNodeDaemon>,
    bound_addr: SocketAddr,
    tls_config: Arc<RwLock<Arc<ServerConfig>>>,
    trust_policy: Arc<RwLock<ClusterTrustPolicy>>,
    simulated_clock_unix_secs: Arc<AtomicU64>,
    shutdown_flag: Arc<AtomicBool>,
    accept_thread: Option<JoinHandle<()>>,
}

impl NodeRpcServer {
    /// Starts a new [`NodeRpcServer`] listening on `daemon.listen_addr()`.
    pub fn start(
        daemon: Arc<StorageNodeDaemon>,
        tls_config: Arc<ServerConfig>,
        trust_policy: ClusterTrustPolicy,
        socket_timeout: Duration,
    ) -> Result<Self, RpcError> {
        let listener = TcpListener::bind(daemon.listen_addr())
            .map_err(|e| RpcError::Transport(format!("bind {}: {}", daemon.listen_addr(), e)))?;
        let bound_addr = listener
            .local_addr()
            .map_err(|e| RpcError::Transport(format!("local_addr: {}", e)))?;

        let tls_config = Arc::new(RwLock::new(tls_config));
        let trust_policy = Arc::new(RwLock::new(trust_policy));
        let simulated_clock_unix_secs = Arc::new(AtomicU64::new(0));
        let shutdown_flag = Arc::new(AtomicBool::new(false));
        let max_payload_bytes = MAX_RPC_PAYLOAD_BYTES;

        let daemon_clone = daemon.clone();
        let tls_clone = tls_config.clone();
        let trust_clone = trust_policy.clone();
        let clock_clone = simulated_clock_unix_secs.clone();
        let shutdown_clone = shutdown_flag.clone();

        let accept_thread = thread::Builder::new()
            .name(format!("fsp-node-{:02}-accept", daemon.node_id().0))
            .spawn(move || {
                for incoming in listener.incoming() {
                    if shutdown_clone.load(Ordering::Acquire) {
                        break;
                    }
                    let Ok(tcp_stream) = incoming else {
                        if shutdown_clone.load(Ordering::Acquire) {
                            break;
                        }
                        continue;
                    };
                    if shutdown_clone.load(Ordering::Acquire) {
                        break;
                    }

                    let d = daemon_clone.clone();
                    let tls_cfg = tls_clone.read().unwrap_or_else(|p| p.into_inner()).clone();
                    let tp = trust_clone.clone();
                    let clk = clock_clone.clone();

                    thread::spawn(move || {
                        let _ = handle_client_connection(
                            tcp_stream,
                            d,
                            tls_cfg,
                            tp,
                            clk,
                            socket_timeout,
                            max_payload_bytes,
                        );
                    });
                }
            })
            .map_err(|e| RpcError::Transport(format!("spawn accept thread: {}", e)))?;

        Ok(Self {
            daemon,
            bound_addr,
            tls_config,
            trust_policy,
            simulated_clock_unix_secs,
            shutdown_flag,
            accept_thread: Some(accept_thread),
        })
    }

    /// Returns the bound [`SocketAddr`] (resolved even when started on port `0`).
    pub const fn bound_addr(&self) -> SocketAddr {
        self.bound_addr
    }

    /// Returns a reference to the underlying [`StorageNodeDaemon`].
    pub fn daemon(&self) -> &Arc<StorageNodeDaemon> {
        &self.daemon
    }

    /// Returns a shared handle to the server's [`ClusterTrustPolicy`] for live certificate rotation
    /// or audit log inspection.
    pub fn trust_policy(&self) -> &Arc<RwLock<ClusterTrustPolicy>> {
        &self.trust_policy
    }

    /// Hot-reloads the server's TLS 1.3 [`ServerConfig`] (e.g. after rotating the node's leaf certificate).
    pub fn reload_tls_config(&self, new_config: Arc<ServerConfig>) {
        *self.tls_config.write().unwrap_or_else(|p| p.into_inner()) = new_config;
    }

    /// Sets an override Unix timestamp (`> 0`) for deterministic certificate-expiration testing.
    pub fn set_simulated_clock_unix_secs(&self, unix_secs: u64) {
        self.simulated_clock_unix_secs
            .store(unix_secs, Ordering::Release);
    }

    /// Initiates a clean, graceful shutdown of the RPC listener and marks the node `ShuttingDown`.
    pub fn shutdown(&mut self) {
        if !self.shutdown_flag.swap(true, Ordering::AcqRel) {
            self.daemon
                .health()
                .set_health_state(NodeHealthState::ShuttingDown);
            let _ = TcpStream::connect_timeout(&self.bound_addr, Duration::from_millis(100));
        }
        if let Some(handle) = self.accept_thread.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for NodeRpcServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn current_unix_secs(override_clock: &AtomicU64) -> u64 {
    let sim = override_clock.load(Ordering::Acquire);
    if sim > 0 {
        return sim;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(1_700_000_000)
}

fn handle_client_connection(
    tcp_stream: TcpStream,
    daemon: Arc<StorageNodeDaemon>,
    tls_config: Arc<ServerConfig>,
    trust_policy: Arc<RwLock<ClusterTrustPolicy>>,
    override_clock: Arc<AtomicU64>,
    socket_timeout: Duration,
    max_payload_bytes: u32,
) -> Result<(), RpcError> {
    let _ = tcp_stream.set_nodelay(true);
    let _ = tcp_stream.set_read_timeout(Some(socket_timeout));
    let _ = tcp_stream.set_write_timeout(Some(socket_timeout));

    let server_conn = ServerConnection::new(tls_config).map_err(|e| {
        daemon
            .health()
            .auth_failures_rejected
            .fetch_add(1, Ordering::Relaxed);
        RpcError::Unauthorized(format!("TLS server connection init: {}", e))
    })?;

    let mut tls_stream = StreamOwned::new(server_conn, tcp_stream);

    // Explicitly complete the TLS 1.3 handshake and verify client mTLS certificate chain
    while tls_stream.conn.is_handshaking() {
        if let Err(e) = tls_stream.conn.complete_io(&mut tls_stream.sock) {
            daemon
                .health()
                .auth_failures_rejected
                .fetch_add(1, Ordering::Relaxed);
            return Err(RpcError::Unauthorized(format!(
                "mTLS handshake failed: {}",
                e
            )));
        }
    }

    let now_secs = current_unix_secs(&override_clock);
    let peer_certs = tls_stream.conn.peer_certificates();
    let peer_identity = {
        let tp = trust_policy.read().unwrap_or_else(|p| p.into_inner());
        match tp.verify_peer_certificate(peer_certs, None, now_secs) {
            Ok(id) => id,
            Err(auth_err) => {
                daemon
                    .health()
                    .auth_failures_rejected
                    .fetch_add(1, Ordering::Relaxed);
                let err_resp = RpcResponse::Error {
                    code: "Unauthorized".to_string(),
                    message: auth_err.to_string(),
                };
                let _ = write_rpc_frame(&mut tls_stream, &err_resp.encode_meta(), &[]);
                return Err(RpcError::Unauthorized(auth_err.to_string()));
            }
        }
    };

    // Read framed request (enforcing max_payload_bytes before allocation)
    let (meta_str, payload, rx_bytes) = match read_rpc_frame(&mut tls_stream, max_payload_bytes) {
        Ok(tuple) => tuple,
        Err(frame_err) => {
            let code = match &frame_err {
                RpcError::ProtocolVersionMismatch { .. } => "ProtocolVersionMismatch",
                RpcError::FrameTooLarge { .. } => "FrameTooLarge",
                RpcError::InvalidFrameMagic(_) => "InvalidFrameMagic",
                _ => "SerializationError",
            };
            let err_resp = RpcResponse::Error {
                code: code.to_string(),
                message: frame_err.to_string(),
            };
            let _ = write_rpc_frame(&mut tls_stream, &err_resp.encode_meta(), &[]);
            return Err(frame_err);
        }
    };
    daemon
        .health()
        .bytes_received
        .fetch_add(rx_bytes, Ordering::Relaxed);

    let request = match RpcRequest::decode_meta(&meta_str) {
        Ok(req) => req,
        Err(dec_err) => {
            let err_resp = RpcResponse::Error {
                code: "SerializationError".to_string(),
                message: dec_err.to_string(),
            };
            let _ = write_rpc_frame(&mut tls_stream, &err_resp.encode_meta(), &[]);
            return Err(dec_err);
        }
    };

    // Role-based authorization check for the requested operation
    {
        let tp = trust_policy.read().unwrap_or_else(|p| p.into_inner());
        if let Err(auth_err) = tp.authorize_operation(&peer_identity, request.operation(), now_secs)
        {
            daemon
                .health()
                .auth_failures_rejected
                .fetch_add(1, Ordering::Relaxed);
            let err_resp = RpcResponse::Error {
                code: "Unauthorized".to_string(),
                message: auth_err.to_string(),
            };
            let _ = write_rpc_frame(&mut tls_stream, &err_resp.encode_meta(), &[]);
            return Err(RpcError::Unauthorized(auth_err.to_string()));
        }
    }

    // Dispatch request to StorageNodeDaemon
    match daemon.handle_rpc(request, &payload) {
        Ok((resp, resp_payload)) => {
            let tx_bytes = write_rpc_frame(&mut tls_stream, &resp.encode_meta(), &resp_payload)?;
            daemon
                .health()
                .bytes_sent
                .fetch_add(tx_bytes, Ordering::Relaxed);
            Ok(())
        }
        Err(rpc_err) => {
            let (code, msg) = match &rpc_err {
                RpcError::Unauthorized(_) => ("Unauthorized", rpc_err.to_string()),
                RpcError::QuotaExceeded(_) => ("QuotaExceeded", rpc_err.to_string()),
                RpcError::ShardNotFound { .. } => ("ShardNotFound", rpc_err.to_string()),
                RpcError::IntegrityFailure(_) => ("IntegrityFailure", rpc_err.to_string()),
                RpcError::SerializationError(_) => ("SerializationError", rpc_err.to_string()),
                RpcError::Fenced {
                    current_epoch,
                    presented_epoch,
                } => ("Fenced", format!("{}:{}", current_epoch, presented_epoch)),
                _ => ("InternalNodeError", rpc_err.to_string()),
            };
            let err_resp = RpcResponse::Error {
                code: code.to_string(),
                message: msg,
            };
            let tx_bytes = write_rpc_frame(&mut tls_stream, &err_resp.encode_meta(), &[])?;
            daemon
                .health()
                .bytes_sent
                .fetch_add(tx_bytes, Ordering::Relaxed);
            Ok(())
        }
    }
}
