//! Production Observability and Prometheus-Compatible Metrics (`src/observability/mod.rs`).
//!
//! Provides telemetry collection and Prometheus exposition text format for storage,
//! networking, consensus, reliability, and system resources.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::recovery::repair::ObjectHealthState;

/// Storage-related metrics.
#[derive(Debug, Default)]
pub struct StorageMetrics {
    pub payload_bytes_stored: AtomicU64,
    pub raw_disk_bytes: AtomicU64,
    pub healthy_shards: AtomicU64,
    pub missing_shards: AtomicU64,
    pub corrupted_shards: AtomicU64,
    pub unreachable_shards: AtomicU64,
}

/// Networking-related metrics.
#[derive(Debug, Default)]
pub struct NetworkMetrics {
    pub rpc_requests_total: AtomicU64,
    pub rpc_errors_total: AtomicU64,
    pub active_tls_connections: AtomicU64,
    pub tls_handshakes_total: AtomicU64,
    pub p50_latency_micros: AtomicU64,
    pub p95_latency_micros: AtomicU64,
    pub p99_latency_micros: AtomicU64,
}

/// Consensus and Quorum-related metrics.
#[derive(Debug, Default)]
pub struct ConsensusMetrics {
    pub current_epoch: AtomicU64,
    pub healthy_voters: AtomicU64,
    pub commit_index: AtomicU64,
    pub permits_issued_total: AtomicU64,
    pub fencing_rejections_total: AtomicU64,
}

/// Reliability, Scrubbing, and Self-Healing metrics.
#[derive(Debug, Default)]
pub struct ReliabilityMetrics {
    pub active_repairs: AtomicU64,
    pub repairs_succeeded_total: AtomicU64,
    pub repairs_failed_total: AtomicU64,
    pub scrub_cycle_duration_secs: AtomicU64,
    pub bitrot_detected_total: AtomicU64,
    pub objects_fully_protected: AtomicU64,
    pub objects_degraded_recoverable: AtomicU64,
    pub objects_temporarily_unavailable: AtomicU64,
    pub objects_repair_blocked: AtomicU64,
    pub objects_irrecoverably_lost: AtomicU64,
}

/// Host and Resource utilization metrics.
#[derive(Debug, Default)]
pub struct ResourceMetrics {
    pub memory_rss_bytes: AtomicU64,
    pub disk_free_bytes: AtomicU64,
    pub network_egress_bytes: AtomicU64,
}

/// Disaster Recovery and Emergency Read metrics.
#[derive(Debug, Default)]
pub struct RecoveryMetrics {
    pub emergency_reads_active: AtomicU64,
    pub emergency_reads_total: AtomicU64,
    pub emergency_reads_failed_total: AtomicU64,
    pub recovery_freshness_authentic_latest: AtomicU64,
    pub recovery_freshness_historical: AtomicU64,
    pub recovery_freshness_unverifiable: AtomicU64,
    pub last_metadata_backup_timestamp_unix_secs: AtomicU64,
    pub disaster_recovery_restores_total: AtomicU64,
    pub disaster_recovery_restores_failed_total: AtomicU64,
}

/// Central cluster observability collector.
#[derive(Debug, Default, Clone)]
pub struct MetricsCollector {
    pub storage: Arc<StorageMetrics>,
    pub network: Arc<NetworkMetrics>,
    pub consensus: Arc<ConsensusMetrics>,
    pub reliability: Arc<ReliabilityMetrics>,
    pub resources: Arc<ResourceMetrics>,
    pub recovery: Arc<RecoveryMetrics>,
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records object health classification count into reliability histogram metrics.
    pub fn record_object_health(&self, state: ObjectHealthState) {
        match state {
            ObjectHealthState::FullyProtected => {
                self.reliability
                    .objects_fully_protected
                    .fetch_add(1, Ordering::SeqCst);
            }
            ObjectHealthState::DegradedButRecoverable => {
                self.reliability
                    .objects_degraded_recoverable
                    .fetch_add(1, Ordering::SeqCst);
            }
            ObjectHealthState::TemporarilyUnavailable => {
                self.reliability
                    .objects_temporarily_unavailable
                    .fetch_add(1, Ordering::SeqCst);
            }
            ObjectHealthState::RepairBlockedByCapacity => {
                self.reliability
                    .objects_repair_blocked
                    .fetch_add(1, Ordering::SeqCst);
            }
            ObjectHealthState::IrrecoverablyLost => {
                self.reliability
                    .objects_irrecoverably_lost
                    .fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    /// Renders Prometheus text-format exposition (`/metrics` endpoint output).
    pub fn render_prometheus_metrics(&self) -> String {
        let mut out = String::with_capacity(4096);

        // 1. Storage
        out.push_str("# HELP fsp_storage_bytes_stored_total Total payload bytes stored across all objects.\n");
        out.push_str("# TYPE fsp_storage_bytes_stored_total counter\n");
        out.push_str(&format!(
            "fsp_storage_bytes_stored_total {}\n",
            self.storage.payload_bytes_stored.load(Ordering::SeqCst)
        ));

        out.push_str(
            "# HELP fsp_storage_raw_disk_bytes Total raw bytes committed across physical disks.\n",
        );
        out.push_str("# TYPE fsp_storage_raw_disk_bytes gauge\n");
        out.push_str(&format!(
            "fsp_storage_raw_disk_bytes {}\n",
            self.storage.raw_disk_bytes.load(Ordering::SeqCst)
        ));

        let payload = self.storage.payload_bytes_stored.load(Ordering::SeqCst);
        let raw = self.storage.raw_disk_bytes.load(Ordering::SeqCst);
        let overhead = if payload > 0 {
            raw as f64 / payload as f64
        } else {
            1.66666667
        };
        out.push_str(
            "# HELP fsp_storage_parity_overhead_ratio Ratio of raw storage to logical payload.\n",
        );
        out.push_str("# TYPE fsp_storage_parity_overhead_ratio gauge\n");
        out.push_str(&format!(
            "fsp_storage_parity_overhead_ratio {:.4}\n",
            overhead
        ));

        out.push_str(
            "# HELP fsp_storage_shards_total Count of shards in cluster by physical status.\n",
        );
        out.push_str("# TYPE fsp_storage_shards_total gauge\n");
        out.push_str(&format!(
            "fsp_storage_shards_total{{status=\"healthy\"}} {}\n",
            self.storage.healthy_shards.load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_storage_shards_total{{status=\"missing\"}} {}\n",
            self.storage.missing_shards.load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_storage_shards_total{{status=\"corrupted\"}} {}\n",
            self.storage.corrupted_shards.load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_storage_shards_total{{status=\"unreachable\"}} {}\n",
            self.storage.unreachable_shards.load(Ordering::SeqCst)
        ));

        // 2. Networking
        out.push_str(
            "# HELP fsp_network_rpc_requests_total Total distributed RPC requests handled.\n",
        );
        out.push_str("# TYPE fsp_network_rpc_requests_total counter\n");
        out.push_str(&format!(
            "fsp_network_rpc_requests_total {}\n",
            self.network.rpc_requests_total.load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_network_rpc_errors_total Total RPC errors or rejections.\n");
        out.push_str("# TYPE fsp_network_rpc_errors_total counter\n");
        out.push_str(&format!(
            "fsp_network_rpc_errors_total {}\n",
            self.network.rpc_errors_total.load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_network_connection_pool_active_connections Number of active TLS connections.\n");
        out.push_str("# TYPE fsp_network_connection_pool_active_connections gauge\n");
        out.push_str(&format!(
            "fsp_network_connection_pool_active_connections {}\n",
            self.network.active_tls_connections.load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_network_rpc_latency_seconds RPC request latency quantiles.\n");
        out.push_str("# TYPE fsp_network_rpc_latency_seconds summary\n");
        out.push_str(&format!(
            "fsp_network_rpc_latency_seconds{{quantile=\"0.5\"}} {:.6}\n",
            self.network.p50_latency_micros.load(Ordering::SeqCst) as f64 / 1_000_000.0
        ));
        out.push_str(&format!(
            "fsp_network_rpc_latency_seconds{{quantile=\"0.95\"}} {:.6}\n",
            self.network.p95_latency_micros.load(Ordering::SeqCst) as f64 / 1_000_000.0
        ));
        out.push_str(&format!(
            "fsp_network_rpc_latency_seconds{{quantile=\"0.99\"}} {:.6}\n",
            self.network.p99_latency_micros.load(Ordering::SeqCst) as f64 / 1_000_000.0
        ));

        // 3. Consensus
        out.push_str("# HELP fsp_consensus_current_epoch Monotonic leadership epoch term.\n");
        out.push_str("# TYPE fsp_consensus_current_epoch gauge\n");
        out.push_str(&format!(
            "fsp_consensus_current_epoch {}\n",
            self.consensus.current_epoch.load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_consensus_voters_healthy Healthy voting members in quorum.\n");
        out.push_str("# TYPE fsp_consensus_voters_healthy gauge\n");
        out.push_str(&format!(
            "fsp_consensus_voters_healthy {}\n",
            self.consensus.healthy_voters.load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_consensus_commit_index Replicated WAL commit index watermark.\n");
        out.push_str("# TYPE fsp_consensus_commit_index gauge\n");
        out.push_str(&format!(
            "fsp_consensus_commit_index {}\n",
            self.consensus.commit_index.load(Ordering::SeqCst)
        ));

        out.push_str(
            "# HELP fsp_consensus_permits_issued_total Total QuorumMutationPermits issued.\n",
        );
        out.push_str("# TYPE fsp_consensus_permits_issued_total counter\n");
        out.push_str(&format!(
            "fsp_consensus_permits_issued_total {}\n",
            self.consensus.permits_issued_total.load(Ordering::SeqCst)
        ));

        out.push_str(
            "# HELP fsp_consensus_fencing_rejections_total Total stale epoch RPCs rejected.\n",
        );
        out.push_str("# TYPE fsp_consensus_fencing_rejections_total counter\n");
        out.push_str(&format!(
            "fsp_consensus_fencing_rejections_total {}\n",
            self.consensus
                .fencing_rejections_total
                .load(Ordering::SeqCst)
        ));

        // 4. Reliability
        out.push_str("# HELP fsp_reliability_active_repairs Number of background repairs currently active.\n");
        out.push_str("# TYPE fsp_reliability_active_repairs gauge\n");
        out.push_str(&format!(
            "fsp_reliability_active_repairs {}\n",
            self.reliability.active_repairs.load(Ordering::SeqCst)
        ));

        out.push_str(
            "# HELP fsp_reliability_repairs_total Cumulative repair operations by status.\n",
        );
        out.push_str("# TYPE fsp_reliability_repairs_total counter\n");
        out.push_str(&format!(
            "fsp_reliability_repairs_total{{status=\"success\"}} {}\n",
            self.reliability
                .repairs_succeeded_total
                .load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_reliability_repairs_total{{status=\"failure\"}} {}\n",
            self.reliability.repairs_failed_total.load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_reliability_scrub_cycle_duration_seconds Scrub cycle duration.\n");
        out.push_str("# TYPE fsp_reliability_scrub_cycle_duration_seconds gauge\n");
        out.push_str(&format!(
            "fsp_reliability_scrub_cycle_duration_seconds {}\n",
            self.reliability
                .scrub_cycle_duration_secs
                .load(Ordering::SeqCst)
        ));

        out.push_str(
            "# HELP fsp_reliability_bitrot_events_total Detected bit-rot corruption events.\n",
        );
        out.push_str("# TYPE fsp_reliability_bitrot_events_total counter\n");
        out.push_str(&format!(
            "fsp_reliability_bitrot_events_total {}\n",
            self.reliability
                .bitrot_detected_total
                .load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_reliability_objects_total Total objects categorized by operational health state.\n");
        out.push_str("# TYPE fsp_reliability_objects_total gauge\n");
        out.push_str(&format!(
            "fsp_reliability_objects_total{{health_state=\"FullyProtected\"}} {}\n",
            self.reliability
                .objects_fully_protected
                .load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_reliability_objects_total{{health_state=\"DegradedButRecoverable\"}} {}\n",
            self.reliability
                .objects_degraded_recoverable
                .load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_reliability_objects_total{{health_state=\"TemporarilyUnavailable\"}} {}\n",
            self.reliability
                .objects_temporarily_unavailable
                .load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_reliability_objects_total{{health_state=\"RepairBlockedByCapacity\"}} {}\n",
            self.reliability
                .objects_repair_blocked
                .load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_reliability_objects_total{{health_state=\"IrrecoverablyLost\"}} {}\n",
            self.reliability
                .objects_irrecoverably_lost
                .load(Ordering::SeqCst)
        ));

        // 5. System Resources
        out.push_str(
            "# HELP fsp_resource_memory_rss_bytes Process resident set size memory in bytes.\n",
        );
        out.push_str("# TYPE fsp_resource_memory_rss_bytes gauge\n");
        out.push_str(&format!(
            "fsp_resource_memory_rss_bytes {}\n",
            self.resources.memory_rss_bytes.load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_resource_disk_free_bytes Available disk storage in bytes.\n");
        out.push_str("# TYPE fsp_resource_disk_free_bytes gauge\n");
        out.push_str(&format!(
            "fsp_resource_disk_free_bytes {}\n",
            self.resources.disk_free_bytes.load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_resource_network_egress_bytes Total network egress bytes.\n");
        out.push_str("# TYPE fsp_resource_network_egress_bytes counter\n");
        out.push_str(&format!(
            "fsp_resource_network_egress_bytes {}\n",
            self.resources.network_egress_bytes.load(Ordering::SeqCst)
        ));

        // 6. Disaster Recovery & Emergency Read
        out.push_str("# HELP fsp_recovery_emergency_reads_active Active offline emergency reads in flight.\n");
        out.push_str("# TYPE fsp_recovery_emergency_reads_active gauge\n");
        out.push_str(&format!(
            "fsp_recovery_emergency_reads_active {}\n",
            self.recovery.emergency_reads_active.load(Ordering::SeqCst)
        ));

        out.push_str(
            "# HELP fsp_recovery_emergency_reads_total Cumulative emergency reads by status.\n",
        );
        out.push_str("# TYPE fsp_recovery_emergency_reads_total counter\n");
        out.push_str(&format!(
            "fsp_recovery_emergency_reads_total{{status=\"total\"}} {}\n",
            self.recovery.emergency_reads_total.load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_recovery_emergency_reads_total{{status=\"failed\"}} {}\n",
            self.recovery
                .emergency_reads_failed_total
                .load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_recovery_freshness_classifications_total Emergency reads classified by freshness.\n");
        out.push_str("# TYPE fsp_recovery_freshness_classifications_total counter\n");
        out.push_str(&format!(
            "fsp_recovery_freshness_classifications_total{{status=\"authentic_latest\"}} {}\n",
            self.recovery
                .recovery_freshness_authentic_latest
                .load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_recovery_freshness_classifications_total{{status=\"historical\"}} {}\n",
            self.recovery
                .recovery_freshness_historical
                .load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_recovery_freshness_classifications_total{{status=\"unverifiable\"}} {}\n",
            self.recovery
                .recovery_freshness_unverifiable
                .load(Ordering::SeqCst)
        ));

        out.push_str("# HELP fsp_recovery_backup_last_timestamp_seconds Unix timestamp of last metadata backup.\n");
        out.push_str("# TYPE fsp_recovery_backup_last_timestamp_seconds gauge\n");
        out.push_str(&format!(
            "fsp_recovery_backup_last_timestamp_seconds {}\n",
            self.recovery
                .last_metadata_backup_timestamp_unix_secs
                .load(Ordering::SeqCst)
        ));

        out.push_str(
            "# HELP fsp_recovery_restores_total Cumulative disaster recovery restore executions.\n",
        );
        out.push_str("# TYPE fsp_recovery_restores_total counter\n");
        out.push_str(&format!(
            "fsp_recovery_restores_total{{status=\"total\"}} {}\n",
            self.recovery
                .disaster_recovery_restores_total
                .load(Ordering::SeqCst)
        ));
        out.push_str(&format!(
            "fsp_recovery_restores_total{{status=\"failed\"}} {}\n",
            self.recovery
                .disaster_recovery_restores_failed_total
                .load(Ordering::SeqCst)
        ));

        out
    }
}
