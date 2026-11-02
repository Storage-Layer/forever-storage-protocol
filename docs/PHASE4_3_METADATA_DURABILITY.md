# FSP Phase 4.3: Metadata Durability, WAL Replay & Crash Reconciliation

This document analyzes the durable state machine and crash recovery mechanics verified in **FSP Phase 4.3**.

---

## 1. Subsystem Architecture

Metadata durability in FSP is governed by three cooperative on-disk subsystems:
1. **Replicated Log Manager (`replicated_log.wal`):**
   - Implemented in [`ReplicatedLogManager`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L341).
   - Records monotonic `LogEntry` records (`term`, `index`, `record`).
   - Every proposal is committed to disk using `OpenOptions::append` followed by `file.sync_all()`.
   - Replay reconstructs `last_log_index` and verifies log prefix continuity upon coordinator startup.
2. **Durable Repair Journal (`repair_jobs/*.rec`):**
   - Implemented in [`DurableRepairJournal`](file:///home/rade/Project/Jobs/fsp/src/coordinator/repair_journal.rs#L182).
   - Manages discrete on-disk records for every active repair operation.
   - Updates are fsynced atomically with directory synchronization.
3. **Monotonic Generation Watermarks (`*.gen`):**
   - Implemented in [`TrustedSignerPolicy`](file:///home/rade/Project/Jobs/fsp/src/security/signatures.rs#L362).
   - Enforces replay protection on object manifests, strictly forbidding manifest rollback ($gen_{new} \le gen_{recorded}$).

---

## 2. Crash Recovery & In-Flight State Reconciliation

### 2.1 Replicated Log Crash Recovery
When a coordinator crashes (e.g. via abrupt SIGKILL or power outage):
- Proposed but un-replicated log entries remain in the local WAL.
- Upon restart or standby promotion, [`ReplicatedLogManager::open_or_init`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L354) parses all entries from disk.
- Uncommitted entries (where index $> commit\_index$) are retained in memory/WAL but are **not** acknowledged to clients.
- If a new leader is elected with a higher term, standby replicas accept [`AppendEntriesRequest`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L320), truncating any uncommitted conflicting entries and bringing replicas into exact parity with the certified leader.

### 2.2 Repair Journal Crash Reconciliation
If a coordinator terminates while executing active shard repairs:
- Jobs in states `Repairing`, `Scheduled`, `Verifying`, or `CapacityCheck` are in-flight and unverified.
- When a new coordinator (or restarted instance) initializes via [`DurableRepairJournal::open_or_init`](file:///home/rade/Project/Jobs/fsp/src/coordinator/repair_journal.rs#L188), it reads all `.rec` files.
- It detects in-flight jobs and automatically reconciles them:
  ```rust
  job.state = RepairJobState::Pending;
  job.error_msg = Some("interrupted by coordinator restart".to_string());
  write_and_sync_file(&path, job.encode().as_bytes())?;
  ```
- This prevents orphaned repair jobs or permanently locked target nodes, enabling the cluster scheduler to re-evaluate and complete repairs safely.

---

## 3. Test Verification

Verified by:
- [`tests/phase4_3_validation.rs::test_p4_3_09_coordinator_sigkill_reconciles_uncommitted_state`](file:///home/rade/Project/Jobs/fsp/tests/phase4_3_validation.rs#L380): Confirms uncommitted WAL entries are retained without false client commit acks.
- [`tests/phase4_3_validation.rs::test_p4_3_10_manifest_generation_conflict_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase4_3_validation.rs#L415): Confirms conflicting manifest versions at the same generation are rejected.
- [`tests/phase4_3_validation.rs::test_p4_3_11_manifest_rollback_strictly_blocked`](file:///home/rade/Project/Jobs/fsp/tests/phase4_3_validation.rs#L446): Confirms generation rollbacks ($5 \to 4$) are rejected.
- [`tests/phase4_3_validation.rs::test_p4_3_16_repair_consistency_during_active_leader_failover`](file:///home/rade/Project/Jobs/fsp/tests/phase4_3_validation.rs#L625): Confirms in-flight `Repairing` jobs reset safely to `Pending`.
