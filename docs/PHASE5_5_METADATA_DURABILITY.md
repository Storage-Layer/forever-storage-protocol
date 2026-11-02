# Phase 5.5 Coordinator Metadata Durability & Quorum Recovery

## 1. Metadata Write-Ahead Logging (WAL) Architecture

Coordinator metadata state transitions are protected by a crash-safe Write-Ahead Log:
- **Location:** `<coord_root>/journal/<tx_id>.coord_tx`
- **Format:** Line-based text journal with explicit fields:
  ```text
  STATE=PREPARING
  tx_id=ctx-obj-01-000001
  object_id=obj-01
  version=1
  TARGET=0|0|127.0.0.1:9100
  ...
  ```
- **Sync Ordering:** The coordinator writes the WAL entry and calls `write_and_sync_file` (flushing and fsyncing to disk) **before** issuing any network RPCs to storage nodes.
- **Commit Marking:** After the majority quorum confirms receipt, the coordinator renames and fsyncs the signed manifest, then rewrites the journal entry to `STATE=COMMITTED` with `sync_directory()`.

## 2. Startup Reconciliation & Crash Recovery

When a coordinator restarts ([`DistributedCoordinator::recover_coordinator_transactions`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs#L351)):
1. **Uncommitted Transactions (`STATE=PREPARING`):**
   - The coordinator scans the journal.
   - Any transaction that did not achieve `STATE=COMMITTED` is aborted.
   - Orphaned remote shard fragments that were prepared are issued remote cleanup RPCs.
2. **Committed Transactions (`STATE=COMMITTED`):**
   - The coordinator loads the corresponding `<id>.sig.json` manifest.
   - Cryptographic Ed25519 signature is verified against `TrustedSignerPolicy`.
   - Manifest generation is restored monotonically.

## 3. Test Evidence

- Test [`test_p5_5_08`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs#L380): Proved that a truncated/torn WAL record is safely identified and aborted on coordinator startup without corrupting active manifests.
- Test [`test_p5_5_09`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs#L400): Proved that a committed transaction and its signed manifest survive an abrupt coordinator crash and restart with 100% data and generation integrity.
