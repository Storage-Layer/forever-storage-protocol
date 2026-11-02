# FSP Phase 5.1 — Observability, Telemetry & Prometheus Integration

## 1. Overview
FSP provides production-grade observability via [src/observability/mod.rs](file:///home/rade/Project/Jobs/fsp/src/observability/mod.rs). The [`MetricsCollector`](file:///home/rade/Project/Jobs/fsp/src/observability/mod.rs) records telemetry across 6 domains:
1. **Storage:** Bytes stored, raw disk bytes, shard health state counters.
2. **Networking:** RPC requests, errors, active TLS connections, p50/p95/p99 latency.
3. **Consensus:** Quorum term, healthy voters, commit index, permits issued, fencing rejections.
4. **Reliability:** Active repairs, repair outcomes, scrub duration, object resilience states.
5. **Resources:** Memory RSS, free disk bytes, egress traffic.
6. **Disaster Recovery (Phase 5.1):** Active emergency reads, recovery freshness status classifications, backup timestamps, restore outcomes.

## 2. Phase 5.1 Prometheus Exposition Metrics
The `/metrics` endpoint exports the following metrics in Prometheus text exposition format:

```prometheus
# HELP fsp_recovery_emergency_reads_active Active offline emergency reads in flight.
# TYPE fsp_recovery_emergency_reads_active gauge
fsp_recovery_emergency_reads_active 0

# HELP fsp_recovery_emergency_reads_total Cumulative emergency reads by status.
# TYPE fsp_recovery_emergency_reads_total counter
fsp_recovery_emergency_reads_total{status="total"} 14
fsp_recovery_emergency_reads_total{status="failed"} 6

# HELP fsp_recovery_freshness_classifications_total Emergency reads classified by freshness.
# TYPE fsp_recovery_freshness_classifications_total counter
fsp_recovery_freshness_classifications_total{status="authentic_latest"} 8
fsp_recovery_freshness_classifications_total{status="historical"} 1
fsp_recovery_freshness_classifications_total{status="unverifiable"} 1

# HELP fsp_recovery_backup_last_timestamp_seconds Unix timestamp of last metadata backup.
# TYPE fsp_recovery_backup_last_timestamp_seconds gauge
fsp_recovery_backup_last_timestamp_seconds 1700001234

# HELP fsp_recovery_restores_total Cumulative disaster recovery restore executions.
# TYPE fsp_recovery_restores_total counter
fsp_recovery_restores_total{status="total"} 3
fsp_recovery_restores_total{status="failed"} 1
```

## 3. Prometheus Alerting Rules Configuration
Located at [deploy/phase5_1/monitoring/prometheus_alerts.yml](file:///home/rade/Project/Jobs/fsp/deploy/phase5_1/monitoring/prometheus_alerts.yml):
- `FspObjectDegradedRecoverable`: Fires when objects drop below full redundancy.
- `FspObjectIrrecoverablyLost`: Critical alert when surviving shards $< 12$.
- `FspMetadataBackupStale`: Fires when backup age exceeds 30 minutes.
- `FspVoterQuorumDegraded`: Fires when healthy voters drop below 3.
- `FspEmergencyReadFailed`: Fires on unauthorized emergency read attempts.
