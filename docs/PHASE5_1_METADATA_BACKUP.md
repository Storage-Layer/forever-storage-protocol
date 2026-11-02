# FSP Phase 5.1 — Metadata Backup & Bundle Architecture

## 1. Overview
FSP replicates control plane state via an append-only Write-Ahead Log (`metadata_replication.wal`) and compacted snapshots (`snapshot.rec`) via [src/coordinator/replication.rs](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs).
To enable catastrophic coordinator recovery (e.g. total host failure, unrecoverable disk corruption of all coordinator nodes), Phase 5.1 introduces **Disaster Recovery Backup Bundles** in [src/recovery/disaster_recovery.rs](file:///home/rade/Project/Jobs/fsp/src/recovery/disaster_recovery.rs).

## 2. DisasterRecoveryBackupBundle Specification
The bundle encapsulates:
- `bundle_version`: Format version (`1`).
- `operator_id`: Identity of the disaster recovery operator.
- `coordinator_term`: Term at which the backup was exported.
- `last_log_index`: Highest log index included in the WAL.
- `commit_index`: Highest committed quorum index in the bundle.
- `snapshot`: The base [`MetadataSnapshot`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs).
- `wal_entries`: Committed WAL entries beyond the snapshot index up to `commit_index`.
- `manifest_signers`: Map of pinned manifest signer public keys.
- `bundle_digest_hex`: SHA-256 over all encapsulated structures.
- `signature_hex`: Operator RFC 8032 Ed25519 signature over `"FSP-DR-BACKUP-V1\n" + bundle_digest`.

```rust
pub struct DisasterRecoveryBackupBundle {
    pub bundle_version: u32,
    pub operator_id: String,
    pub coordinator_term: u64,
    pub last_log_index: u64,
    pub commit_index: u64,
    pub snapshot: MetadataSnapshot,
    pub wal_entries: Vec<LogEntry>,
    pub manifest_signers: BTreeMap<String, String>,
    pub bundle_digest_hex: String,
    pub signature_hex: String,
}
```

## 3. Atomic Export & Persistence Workflow
1. Read current snapshot and committed WAL entries without taking offline downtime.
2. Calculate deterministic canonical digest across snapshot and WAL records.
3. Sign digest using the operator's private key (`dr_operator.key`).
4. Write atomically to `<backup_file>.tmp` and rename to `<backup_file>`.
5. Automated cron schedule ([deploy/phase5_1/backup/backup_schedule.cron](file:///home/rade/Project/Jobs/fsp/deploy/phase5_1/backup/backup_schedule.cron)) exports bundles every 15 minutes.
