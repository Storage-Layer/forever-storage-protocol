# FSP Phase 5.5: Garbage Collection Crash Safety & Tombstone Lifecycle

## 1. Six-Stage Garbage Collection Lifecycle

To prevent premature data deletion during process crash, power loss, or coordinator failover, FSP implements a six-stage monotonic GC lifecycle with persistent tombstones:

```
[ Active Shard Container ]
           │
           ▼
[ Stage 1: Candidate Discovery & Reference Audit ]
  └── Evaluates global manifest references across all active epochs
           │
           ▼
[ Stage 2: Tombstone Issuance & WAL Log ]
  └── Writes persistent tombstone record to node WAL (`sync_all`)
           │
           ▼
[ Stage 3: Quarantine Isolation ]
  └── Atomically moves shard file to `.quarantine/` directory
           │
           ▼
[ Stage 4: Safe Reclamation Wait Period ]
  └── Enforces 2-epoch / TTL grace period to drain in-flight reads
           │
           ▼
[ Stage 5: Physical Unlink ]
  └── Calls `unlink()` on quarantined shard container
           │
           ▼
[ Stage 6: Parent Directory Barrier Sync ]
  └── Calls `sync_all()` on parent directory to ensure metadata permanence
```

---

## 2. Crash Scenarios During Garbage Collection

| GC Stage | Crash Event | State on Recovery | Invariant Maintained |
|:---|:---|:---|:---|
| **Stage 1 (Discovery)** | SIGKILL before tombstone | Shard remains in active storage path; no metadata change. | **Safety:** Active shards never deleted; candidate re-evaluated on next GC run. |
| **Stage 2 (Tombstone WAL)** | SIGKILL mid-WAL write | WAL entry truncated or uncommitted; recovery discards torn entry. | **Safety:** Shard remains active. |
| **Stage 3 (Quarantine Move)** | SIGKILL mid-rename | POSIX `rename()` is atomic. Either shard is in primary path or quarantine path. | **Consistency:** If in quarantine, tombstone log guides cleanup or restoration. |
| **Stage 4 (Grace Wait)** | Coordinator / node crash | Tombstone preserved in WAL; grace timer resumes on restart. | **Safety:** In-flight concurrent GET requests complete safely. |
| **Stage 5 (Physical Unlink)** | SIGKILL before unlink | Quarantined file remains on disk; recovered node identifies expired tombstone and re-unlinks. | **Liveness:** Leak resolved on next startup pass. |
| **Stage 6 (Directory Sync)** | Power loss after unlink | Ext4 journal replay completes directory entry removal. | **Durability:** Zero orphaned directory blocks. |

---

## 3. Empirical Test Evidence: `test_gc_crash_safety_mid_cycle`

Validated under test suite [`tests/phase5_5_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs) (`test_gc_crash_safety_mid_cycle`):
- A valid shard was created and marked with a persistent tombstone in node storage.
- A simulated mid-cycle crash was injected before final unlinking.
- Upon node re-initialization:
  1. The node inspected pending tombstones in its recovery journal.
  2. The quarantine directory safely isolated unlinked candidates from active read routes.
  3. Active non-garbage shards remained completely unharmed and readable with bit-level integrity verified.
  4. The uncommitted GC operation was cleanly resolved without dangling references or data loss.
