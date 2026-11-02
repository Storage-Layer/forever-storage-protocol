//! Storage node health states and atomic operational telemetry counters (`src/node/health.rs`).

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::time::Instant;

/// Operational health state of a running storage node daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeHealthState {
    /// Node is healthy and accepting all authorized reads and writes.
    Healthy = 0,
    /// Node experienced recoverable errors or high load; still serving requests.
    Degraded = 1,
    /// Node storage capacity or maintenance policy allows reads/verifies only.
    ReadOnly = 2,
    /// Node is draining connections for graceful shutdown.
    ShuttingDown = 3,
}

impl NodeHealthState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Healthy => "Healthy",
            Self::Degraded => "Degraded",
            Self::ReadOnly => "ReadOnly",
            Self::ShuttingDown => "ShuttingDown",
        }
    }

    pub const fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Healthy,
            1 => Self::Degraded,
            2 => Self::ReadOnly,
            _ => Self::ShuttingDown,
        }
    }
}

/// Atomic telemetry and health tracker for a storage node daemon.
#[derive(Debug)]
pub struct NodeHealthTelemetry {
    started_at: Instant,
    state: AtomicU8,
    pub requests_processed: AtomicU64,
    pub auth_failures_rejected: AtomicU64,
    pub bytes_received: AtomicU64,
    pub bytes_sent: AtomicU64,
    pub fsync_count: AtomicU64,
}

impl Default for NodeHealthTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeHealthTelemetry {
    pub fn new() -> Self {
        Self {
            started_at: Instant::now(),
            state: AtomicU8::new(NodeHealthState::Healthy as u8),
            requests_processed: AtomicU64::new(0),
            auth_failures_rejected: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            fsync_count: AtomicU64::new(0),
        }
    }

    pub fn health_state(&self) -> NodeHealthState {
        NodeHealthState::from_u8(self.state.load(Ordering::Acquire))
    }

    pub fn set_health_state(&self, state: NodeHealthState) {
        self.state.store(state as u8, Ordering::Release);
    }

    pub fn uptime_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }
}
