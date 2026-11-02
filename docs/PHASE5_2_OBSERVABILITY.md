# Phase 5.2 Observability & Alerting Architecture

## 1. Metric Exposition
FSP exports metrics in standard Prometheus text format over HTTP (`/metrics`). Phase 5.2 extends the recovery and fencing instrumentation:

```prometheus
# HELP fsp_fencing_rejections_total Total number of mutating requests rejected due to stale epoch fencing.
# TYPE fsp_fencing_rejections_total counter
fsp_fencing_rejections_total{node_id="node_01",reason="stale_epoch"} 0

# HELP fsp_gc_rejections_total Total number of garbage collection operations rejected by safety audits.
# TYPE fsp_gc_rejections_total counter
fsp_gc_rejections_total{node_id="node_01",reason="threshold_k12"} 0
fsp_gc_rejections_total{node_id="node_01",reason="legal_hold"} 0
fsp_gc_rejections_total{node_id="node_01",reason="active_generation"} 0

# HELP fsp_restore_rejections_total Total number of metadata restore attempts rejected.
# TYPE fsp_restore_rejections_total counter
fsp_restore_rejections_total{reason="rollback"} 0
fsp_restore_rejections_total{reason="secret_detected"} 0
```

## 2. Production Alerting Rules
Configured in [prometheus_alerts.yml](file:///home/rade/Project/Jobs/fsp/deploy/phase5_2/monitoring/prometheus_alerts.yml):
1. **FspStaleEpochRejectionSpike:** Fires if a storage node rejects operations from a stale epoch, signaling an isolated or zombie coordinator attempting unauthorized writes.
2. **FspGcThresholdViolationAttempt:** Warning alert when a deletion permit attempts to reduce shard redundancy at or below $k=12$.
3. **FspDisasterRecoveryRollbackAttempt:** Critical alert when an online restore attempts to roll back local committed index.
4. **FspSecretMaterialDetectedInBackup:** Paging alert indicating private key patterns in a generated backup bundle.
