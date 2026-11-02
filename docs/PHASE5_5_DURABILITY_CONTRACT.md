# Phase 5.5 Durability Contract & Write-Path Persistence Semantics

## 1. Explicit Durability Level Taxonomy

In Phase 5.5, FSP formally implemented the [`DurabilityLevel`](file:///home/rade/Project/Jobs/fsp/src/storage/backend.rs#L15) contract to prevent confusing volatile page-cache writes with real disk-committed durability:

```
[Received] ──> [Buffered] ──> [LocallyDurable] ──> [QuorumCommitted] ──> [Recoverable]
```

### Durability Levels Defined:
1. **`Received`:** Bytes arrived in process memory over the wire (TCP/TLS socket buffer). No file write initiated. Volatile on process crash.
2. **`Buffered`:** Bytes written to OS page cache via `write()` / `write_all()`, but not yet committed via disk sync barrier. Survives process crash, but lost on OS kernel panic or power loss.
3. **`LocallyDurable`:** Shard container file written, flushed, verified with `File::sync_all()`, atomically renamed into final place, and parent directory `sync_directory()` executed. Survives node daemon SIGKILL and unexpected machine power loss on barrier-compliant filesystems.
4. **`QuorumCommitted`:** Metadata transaction WAL appended, fsync'd, and committed by majority coordinator quorum ($Q \ge 2$).
5. **`Recoverable`:** At least $K=12$ distinct failure domains confirmed `LocallyDurable` + metadata `QuorumCommitted`. Guarantees full bit-exact reconstruction even under an 8-node or 2-zone simultaneous disaster.

---

## 2. Complete 15-Step Write Path Trace

1. **Client PUT Request:** Client streams payload to active Coordinator.
2. **Authorization & Epoch Verification:** Coordinator validates mTLS peer identity and monotonic epoch.
3. **Erasure Encoding:** Payload split into 12 data shards and 8 parity shards via Cauchy RS(20,12).
4. **Failure-Domain Placement:** Shards assigned across 5 failure zones (4 shards per zone).
5. **Coordinator Journal Prepare:** Coordinator logs `STATE=PREPARING` in transaction WAL.
6. **Concurrent RPC Dispatch:** Shards dispatched via `PutShard` RPCs over mutual TLS 1.3.
7. **Storage Node Idempotency Check:** Node checks durable idempotency database for duplicate `tx_id`.
8. **Staged Container Creation:** Shards written to `.fspc.tmp` in staging directory.
9. **Stripe Streaming & Checksums:** Each stripe's SHA-256 digest validated inline.
10. **File Synchronization:** Storage node flushes and invokes `File::sync_all()` on the container file.
11. **Atomic Publication:** Node executes `fs::rename()` moving `.tmp` to final `objects/<id>/shard-YY.fspc`.
12. **Directory Synchronization:** Node invokes `sync_directory()` on container's parent directory.
13. **Node Acknowledgment:** Node writes `STATE=COMMITTED` to local journal and replies `PutShardAck` with `durability_level: LocallyDurable`.
14. **Quorum Manifest Commit:** Once $\ge K$ nodes acknowledge, coordinator signs manifest with Ed25519 and atomically saves to `manifests/<id>.sig.json` followed by directory fsync.
15. **Client Response:** Coordinator returns `DistributedPutReport` with `durability_level: Recoverable`.
