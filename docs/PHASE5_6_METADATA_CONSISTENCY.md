# FSP Phase 5.6: Metadata Consistency & Coordinator WAL Recovery

## 1. Monotonic Metadata Write-Ahead Log (WAL)

The coordinator manages all mutating object operations through an append-only, synchronous WAL in [`src/coordinator/transactions.rs`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs):

1. **PREPARING Entry:** Before sending network RPCs to storage nodes, a transaction record is persisted:
   ```text
   STATE=PREPARING
   tx_id=ctx-obj-123-000001
   object_id=obj-123
   version=1
   TARGET=0|0|127.0.0.1:9501
   ...
   ```
2. **Quorum Shard Gathering:** Storage node responses are gathered. Every `PutShardAck` is verified for identity, shard index, version, and `LocallyDurable` status.
3. **COMMITTED Entry & Manifest:** Upon quorum confirmation ($N \ge K$), the manifest is signed with Ed25519, written atomically to disk, and the WAL entry state is updated to `STATE=COMMITTED` with `sync_all()`.
4. **ABORTED Entry:** If fewer than required shards acknowledge, the WAL entry transitions to `STATE=ABORTED`, and uncommitted remote shards are cleaned up.

---

## 2. Crash Recovery Semantics

### Scenario A: Coordinator Crash in PREPARING State
- If the coordinator process terminates before writing `STATE=COMMITTED`, upon restart the recovery scanner identifies the orphaned `PREPARING` record.
- The transaction was never acknowledged to the client as committed.
- The recovery routine dispatches cleanup RPCs to delete any staged shards on storage nodes and updates the journal to `STATE=ABORTED`.
- Tested in `test_p5_6_10_coordinator_crash_before_commit_rolls_back_journal`.

### Scenario B: Coordinator Crash After COMMITTED State
- If the coordinator crashes after the WAL commit barrier or after manifest publication, the signed manifest on disk is authoritative.
- The recovered coordinator re-registers the object and advances the monotonic generation counter.
- Tested in `test_p5_6_11_coordinator_crash_after_commit_recovers_manifest`.

### Scenario C: Torn WAL Tail Truncation
- If a crash occurs during a disk write to the WAL file resulting in a partial line (e.g. `SEQ=10 OP=COM`), the recovery parser validates line integrity and safely truncates the file to the last complete transaction record.
- Tested in `test_p5_6_17_wal_torn_tail_truncation_on_coordinator_restart`.
