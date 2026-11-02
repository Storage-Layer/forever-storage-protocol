# FSP Phase 5.7: Metadata Durability & Coordinator WAL Recovery

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_METADATA_RECOVERY.md`  
**Machine-Readable Proof:** [`results/phase5_7/coordinator_recovery.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/coordinator_recovery.json)  
**Tests:** `tests/phase5_6_validation.rs` (Tests 10, 11, 17, 18)  

---

## 1. Metadata Architecture & Threat Model

In FSP, metadata represents object manifests, transaction states, stripe layouts, and cryptographic signature headers. Coordinator metadata corruption leads to catastrophic data unavailability or unrecoverable silent data loss.

The metadata subsystem must guarantee:
1. **Crash-Before-Commit Rollback:** If the coordinator terminates before the commit record is fsynced, no partial object is exposed.
2. **Crash-After-Commit Durability:** Once committed, the object must be immediately recoverable upon reboot.
3. **Torn Write Immunity:** Interrupted writes during sudden power loss produce partial bytes at the end of the log. The WAL must detect and truncate corrupted tail records using cryptographic checksums.
4. **Fast Recovery via Checkpointing:** Large WAL logs are periodically compacted into persistent snapshots, allowing cold recovery to initialize from snapshot + incremental log tail.

---

## 2. Empirical Verification Results

### Scenario 1: Rollback on Pre-Commit Crash
- **Test:** `test_p5_6_10_coordinator_crash_before_commit_rolls_back_journal`
- **Injection:** Coordinator writes `Prepare` transaction record, sends stripes, but is killed by SIGKILL prior to appending `Commit`.
- **Outcome:** On restart, coordinator reads journal. The transaction lacks a matching `Commit` record. The journal replay rolls back the allocation, and the object ID remains non-existent.
- **Verification:** PASS. Zero dangling metadata.

### Scenario 2: Committed Manifest Recovery on Post-Commit Crash
- **Test:** `test_p5_6_11_coordinator_crash_after_commit_recovers_manifest`
- **Injection:** Coordinator writes `Commit` record, executes fsync barrier, and is immediately terminated.
- **Outcome:** On restart, replay engine reads the `Commit` record, populates the active manifest table, and verifies all stripe signatures. The object is immediately accessible to client GET requests.
- **Verification:** PASS. Zero committed data lost.

### Scenario 3: Torn Tail WAL Truncation
- **Test:** `test_p5_6_17_wal_torn_tail_truncation_on_coordinator_restart`
- **Injection:** Corrupted trailing bytes (torn sector write) are appended to the WAL file to simulate interrupted disk writes.
- **Outcome:** The WAL parser verifies record CRC32 / SHA-256 headers. The partial record fails validation. The coordinator truncates the file back to the last valid byte offset, logs a warning, and resumes cleanly.
- **Verification:** PASS. Self-healing log recovery.

### Scenario 4: Snapshot Checkpoint + Incremental Replay
- **Test:** `test_p5_6_18_metadata_snapshot_and_incremental_wal_recovery`
- **Mechanism:** Coordinator creates a baseline snapshot checkpoint of 100 objects, followed by 20 new incremental mutations in the WAL.
- **Outcome:** Cold restart loads the snapshot base and replays only the incremental 20 transactions, recovering all 120 objects with zero data drift.
- **Verification:** PASS. O(log delta) startup time.

---

## 3. Metadata Recovery Summary

| Failure Condition | Risk | FSP Defense | Empirical Outcome |
| :--- | :--- | :--- | :---: |
| **Crash before 2PC commit** | Orphaned allocations | Journal rollback | PASS |
| **Crash after commit record** | Lost metadata | Journal replay + fsync | PASS |
| **Torn sector at log tail** | Log parser crash | CRC verification + truncation | PASS |
| **Large transaction history** | Slow recovery | Snapshot compaction | PASS |
