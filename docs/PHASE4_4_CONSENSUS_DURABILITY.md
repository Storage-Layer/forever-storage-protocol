# Phase 4.4 Consensus Durability, Replicated WAL & Snapshot Integrity

**Document ID:** `PHASE4_4_CONSENSUS_DURABILITY.md`  
**Protocol Version:** FSP v0.4.4  
**Date:** 2026-11-01  
**Author:** Principal Distributed Systems Architect, Storage Reliability Engineer  

---

## 1. Durable Replicated Write-Ahead Log (`ReplicatedLogManager`)

The FSP metadata replication system in [`ReplicatedLogManager`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L350-L415) provides append-only persistence of committed manifests, monotonic generation watermarks, repair journal state updates, and cluster node registrations via `replicated_log.wal`.

### Invariants Maintained
1. **Append-Only Fsync:** Every log entry proposal is serialized to disk with synchronous `file.sync_all()` before memory state is updated.
2. **Majority Quorum Commit:** Entries advance `commit_index` if and only if $\ge 2$ out of 3 cluster voters have acknowledged receipt via AppendEntries RPC.
3. **Log Matching Guarantee:** Standby replicas verify that `prev_log_index` and `prev_log_term` match their local history before appending entries. Conflicting uncommitted entries from unseated leaders are truncated.

---

## 2. Metadata Snapshots & Rollback Prevention

As the replicated log grows, log truncation is required to bound startup replay time and disk usage. Phase 4.4 introduces [`MetadataSnapshot`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L320-L380):

```rust
pub struct MetadataSnapshot {
    pub snapshot_index: u64,
    pub snapshot_term: u64,
    pub committed_manifest_generations: BTreeMap<String, u64>,
    pub snapshot_digest_hex: String,
}
```

### Snapshot Digest & Integrity
The snapshot digest is computed deterministically using SHA-256 over canonical text:
```text
FSP-SNAPSHOT-V1
index=<snapshot_index>
term=<snapshot_term>
gen:<obj_id>=<generation>
...
```

### Rollback Rejection Rule
In [`ReplicatedLogManager::install_snapshot`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L770-L840):
$$\text{If } snapshot.snapshot\_index < current\_commit\_index \implies \text{Reject with } \texttt{StorageError::CorruptMetadata}$$

This strictly prevents an attacker or a delayed snapshot synchronization stream from rewinding the coordinator's commit index or reverting manifest generations to previous states.

---

## 3. Consensus Durability Experiments (A through E)

Five mandatory fault-boundary experiments were executed in [`tests/phase4_4_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs):

### Experiment A: Crash Before Majority Quorum Commit
- **Procedure:** Proposal logged by leader $C_1$, but $C_1$ crashes before receiving majority acknowledgments.
- **Result:** Commit index remains 0. When standby $C_2$ takes over at higher term, the uncommitted entry is discarded. Zero uncommitted state leaks.

### Experiment B: Crash After Majority Quorum Commit
- **Procedure:** Proposal acknowledged by 2 of 3 voters. Commit index advances to 1. Coordinator abruptly crashes.
- **Result:** Upon reboot, `replicated_log.wal` is parsed from disk. The committed entry is fully reconstructed; commit index is restored.

### Experiment C: Conflicting Leaders Proposal Rejection
- **Procedure:** Standby replica at Term 2 receives AppendEntries from deposed leader claiming Term 1.
- **Result:** Standby rejects request: `request term 1 < local term 2`. Split-brain log branch is prevented.

### Experiment D: Full WAL Replay and State Machine Recovery
- **Procedure:** Coordinator logs 5 consecutive entries and crashes.
- **Result:** Startup replay recovers all 5 entries, exact index monotonicity, and applied state.

### Experiment E: Snapshot Installation Rejects Rollback
- **Procedure:** Log manager at commit index 10 receives snapshot with index 5.
- **Result:** Rejected with `StorageError::CorruptMetadata("snapshot rollback rejected: snapshot index 5 < current commit index 10")`. Subsequent snapshot with index 10 installs cleanly and truncates log entries $\le 10$.
