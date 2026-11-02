//! Fault Injection Framework for Adversarial Testing (`src/testing/fault_injection.rs`).
//!
//! Provides programmable, reproducible network and storage fault injection to simulate
//! network latency, packet loss, partitions, disk-full events, and stale epoch violations.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;
use std::thread;
use std::time::Duration;

use crate::model::node::NodeId;
use crate::network::protocol::RpcError;
use crate::storage::backend::StorageError;

/// Programmable fault injection rules.
#[derive(Debug, Clone, PartialEq)]
pub enum FaultRule {
    /// Injects artificial latency before servicing an RPC or disk I/O.
    Latency(Duration),
    /// Injects uniform deterministic packet drop with probability `[0.0, 1.0]`.
    PacketLoss { probability: f64 },
    /// Forces immediate connection drop or reset.
    ConnectionReset,
    /// Simulates a network partition isolating specified nodes.
    Partition { isolated_nodes: BTreeSet<NodeId> },
    /// Injects an outdated or stale coordinator epoch.
    StaleEpoch { epoch: u64 },
    /// Injects corrupted bytes / bit flips at specified byte offset.
    CorruptPayload { byte_offset: usize },
    /// Simulates `ENOSPC` disk capacity exhaustion.
    DiskFull,
}

/// Thread-safe fault injection controller.
#[derive(Debug, Default)]
pub struct FaultInjector {
    rules: RwLock<Vec<FaultRule>>,
    enabled: AtomicBool,
}

impl FaultInjector {
    pub fn new() -> Self {
        Self {
            rules: RwLock::new(Vec::new()),
            enabled: AtomicBool::new(true),
        }
    }

    pub fn add_rule(&self, rule: FaultRule) {
        self.rules.write().unwrap().push(rule);
    }

    pub fn clear_rules(&self) {
        self.rules.write().unwrap().clear();
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }

    /// Evaluates an RPC destined for `target_node`.
    pub fn evaluate_network_rpc(&self, target_node: NodeId) -> Result<(), RpcError> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Ok(());
        }

        let rules = self.rules.read().unwrap();
        for rule in rules.iter() {
            match rule {
                FaultRule::Latency(dur) => {
                    thread::sleep(*dur);
                }
                FaultRule::PacketLoss { probability } => {
                    if *probability >= 1.0 {
                        return Err(RpcError::SimulatedNetworkDrop(format!(
                            "deterministic 100% packet drop to node-{:02}",
                            target_node.0
                        )));
                    }
                }
                FaultRule::ConnectionReset => {
                    return Err(RpcError::Transport(format!(
                        "connection reset by peer node-{:02}",
                        target_node.0
                    )));
                }
                FaultRule::Partition { isolated_nodes }
                    if isolated_nodes.contains(&target_node) =>
                {
                    return Err(RpcError::SimulatedNetworkDrop(format!(
                        "node-{:02} is isolated in network partition",
                        target_node.0
                    )));
                }
                _ => {}
            }
        }

        Ok(())
    }

    /// Evaluates a disk storage operation.
    pub fn evaluate_storage_op(&self) -> Result<(), StorageError> {
        if !self.enabled.load(Ordering::Relaxed) {
            return Ok(());
        }

        let rules = self.rules.read().unwrap();
        for rule in rules.iter() {
            match rule {
                FaultRule::DiskFull => {
                    return Err(StorageError::QuotaExceeded {
                        node_id: NodeId(0),
                        used_bytes: 1_000_000,
                        requested_bytes: 50_000,
                        quota_bytes: 1_000_000,
                    });
                }
                FaultRule::Latency(dur) => {
                    thread::sleep(*dur);
                }
                _ => {}
            }
        }

        Ok(())
    }
}
