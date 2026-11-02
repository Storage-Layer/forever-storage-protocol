# Phase 5 Production Observability & Prometheus Metrics
**Forever Storage Protocol (FSP)**
**Date:** 2026-11-01  
**Author:** Principal Site Reliability Engineer & Telemetry Architect  
**Source Module:** [`src/observability/mod.rs`](file:///home/rade/Project/Jobs/fsp/src/observability/mod.rs)

---

## 1. Overview & Architecture

Production storage operations require transparent, low-overhead, continuous telemetry. Phase 5 implements a native, zero-dependency Prometheus exposition text formatter in [`MetricsCollector`](file:///home/rade/Project/Jobs/fsp/src/observability/mod.rs) providing structured `/metrics` telemetry covering:

1. **Storage Subsystem:** Payload bytes, physical disk consumption, parity overhead ratio, and shard counts by status.
2. **Networking Subsystem:** RPC throughput, error rate, p50/p95/p99 latency summaries, connection pool utilization, and TLS handshakes.
3. **Consensus & Quorum:** Current epoch term, healthy voter count, commit index watermark, permit issuance rate, and fencing rejections.
4. **Reliability & Self-Healing:** Active repairs, repair outcomes, scrub cycle runtime, bit-rot detection count, and per-object health histograms.
5. **System Resources:** Memory RSS, free disk space, and network egress volume.

---

## 2. Metric Specifications & Types

```
# ==============================================================================
# FSP Prometheus Telemetry Reference Table
# ==============================================================================
```

| Metric Name | Prometheus Type | Description |
|---|---|---|
| `fsp_storage_bytes_stored_total` | `counter` | Cumulative logical payload bytes stored across all objects. |
| `fsp_storage_raw_disk_bytes` | `gauge` | Total physical disk space consumed (data + Cauchy parity + headers). |
| `fsp_storage_parity_overhead_ratio` | `gauge` | Ratio of raw disk bytes to logical payload (nominal: 1.6667 for RS(20,12)). |
| `fsp_storage_shards_total{status="..."}` | `gauge` | Shards in cluster labeled by `healthy`, `missing`, `corrupted`, `unreachable`. |
| `fsp_network_rpc_requests_total` | `counter` | Total distributed RPC requests received. |
| `fsp_network_rpc_errors_total` | `counter` | Total RPC errors, timeouts, or wire protocol rejections. |
| `fsp_network_connection_pool_active_connections` | `gauge` | Active pooled TLS connections. |
| `fsp_network_rpc_latency_seconds{quantile="..."}` | `summary` | RPC request latency quantiles (`0.5`, `0.95`, `0.99`). |
| `fsp_consensus_current_epoch` | `gauge` | Monotonically increasing leadership epoch term. |
| `fsp_consensus_voters_healthy` | `gauge` | Count of active, responding voters in metadata quorum. |
| `fsp_consensus_commit_index` | `gauge` | Replicated metadata log commit index watermark. |
| `fsp_consensus_permits_issued_total` | `counter` | Total `QuorumMutationPermit` grants issued to coordinators. |
| `fsp_consensus_fencing_rejections_total` | `counter` | Stale leader or stale commit index mutations rejected by storage nodes. |
| `fsp_reliability_active_repairs` | `gauge` | Background shard reconstruction tasks currently executing. |
| `fsp_reliability_repairs_total{status="..."}` | `counter` | Cumulative repair operations labeled by `success` or `failure`. |
| `fsp_reliability_scrub_cycle_duration_seconds` | `gauge` | Time taken to complete the last cluster-wide scrub cycle. |
| `fsp_reliability_bitrot_events_total` | `counter` | Total bit-rot corruption events detected via SHA-256 integrity checks. |
| `fsp_reliability_objects_total{health_state="..."}` | `gauge` | Object counts labeled by `FullyProtected`, `DegradedButRecoverable`, `TemporarilyUnavailable`, `RepairBlockedByCapacity`, `IrrecoverablyLost`. |
| `fsp_resource_memory_rss_bytes` | `gauge` | Resident Set Size (RSS) memory in bytes. |
| `fsp_resource_disk_free_bytes` | `gauge` | Available storage capacity in bytes. |
| `fsp_resource_network_egress_bytes` | `counter` | Total network egress traffic in bytes. |

---

## 3. Sample `/metrics` Text Output

```prometheus
# HELP fsp_storage_bytes_stored_total Total payload bytes stored across all objects.
# TYPE fsp_storage_bytes_stored_total counter
fsp_storage_bytes_stored_total 104857600
# HELP fsp_storage_raw_disk_bytes Total raw bytes committed across physical disks.
# TYPE fsp_storage_raw_disk_bytes gauge
fsp_storage_raw_disk_bytes 174762667
# HELP fsp_storage_parity_overhead_ratio Ratio of raw storage to logical payload.
# TYPE fsp_storage_parity_overhead_ratio gauge
fsp_storage_parity_overhead_ratio 1.6667
# HELP fsp_storage_shards_total Count of shards in cluster by physical status.
# TYPE fsp_storage_shards_total gauge
fsp_storage_shards_total{status="healthy"} 20
fsp_storage_shards_total{status="missing"} 0
fsp_storage_shards_total{status="corrupted"} 0
fsp_storage_shards_total{status="unreachable"} 0
# HELP fsp_network_rpc_requests_total Total distributed RPC requests handled.
# TYPE fsp_network_rpc_requests_total counter
fsp_network_rpc_requests_total 1542
# HELP fsp_consensus_current_epoch Monotonic leadership epoch term.
# TYPE fsp_consensus_current_epoch gauge
fsp_consensus_current_epoch 4
# HELP fsp_reliability_objects_total Total objects categorized by operational health state.
# TYPE fsp_reliability_objects_total gauge
fsp_reliability_objects_total{health_state="FullyProtected"} 1
fsp_reliability_objects_total{health_state="DegradedButRecoverable"} 0
fsp_reliability_objects_total{health_state="TemporarilyUnavailable"} 0
fsp_reliability_objects_total{health_state="RepairBlockedByCapacity"} 0
fsp_reliability_objects_total{health_state="IrrecoverablyLost"} 0
```

---

## 4. Verification

The exposition formatting and label compliance are verified in [`test_p5_10_prometheus_metrics_endpoint_exposition_format`](file:///home/rade/Project/Jobs/fsp/tests/phase5_validation.rs).
Prometheus scrape configurations are deployed in [`deploy/phase5/monitoring_prometheus.yml`](file:///home/rade/Project/Jobs/fsp/deploy/phase5/monitoring_prometheus.yml).
