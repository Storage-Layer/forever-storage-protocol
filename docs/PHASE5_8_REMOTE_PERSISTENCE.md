# FSP Phase 5.8: Remote Shard Persistence & Durable Write Lifecycle

## 1. Durable Write Barrier Contract

FSP defines a strict multi-barrier contract for every persistent shard write operation before an acknowledgement is returned:

```
[Client/Coordinator]
        │
        ▼ (mTLS 1.3 Streaming Frame)
[Storage Node Daemon]
        │
        ├─► 1. Check storage capacity and active write permit
        ├─► 2. Create uncommitted temporary staging file (<storage_root>/staging/<tx_id>.tmp)
        ├─► 3. Stream payload bytes and verify internal CRC32C and SHA-256
        ├─► 4. DATA BARRIER: Execute fsync() / fdatasync() on file descriptor
        ├─► 5. ATOMIC TRANSITION: Atomic rename(<staging_path>, <objects>/<obj_id>/shard-XX.v1.fspc)
        ├─► 6. METADATA BARRIER: Execute fsync() on parent directory file descriptor
        └─► 7. Record durable commit in local idempotency store and return Durable ACK
```

---

## 2. Durable Acknowledgement Specification

A transaction is only marked `Committed` by the coordinator when at least $K=12$ valid acknowledgements are received satisfying the following invariants:
1. **Distinct Shard Indices:** The received ACKs must originate from at least $K=12$ distinct shard indices ($0 \dots 19$). Duplicate ACKs for the same shard index (e.g., from retry replays or concurrent mirrors) do not increment the quorum count.
2. **Cryptographic Hash Binding:** Each ACK carries the computed SHA-256 digest of the committed container body. The coordinator asserts that this matches the corresponding shard digest signed in the manifest.
3. **Monotonic Epoch Permit:** Every write carries the current consensus leadership epoch permit. If a storage node has observed a higher epoch, the write is rejected immediately.

---

## 3. Storage Failure Fail-Closed Enforcement

When disk I/O, filesystem synchronization, or directory flushing fails:
- The daemon does not emit an ACK.
- Uncommitted temporary files in `<staging>` are flagged for deletion.
- Staged container files are not visible in `<objects>/` and will not be indexed upon startup crash recovery.
- The coordinator marks the shard write as failed and attempts repair or reports unrecoverable failure if fewer than $K$ shards succeed.
