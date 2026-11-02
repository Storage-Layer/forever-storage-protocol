# FSP Phase 5.1 — Garbage Collection & Shard Retention Safety

## 1. Overview
In a long-term data preservation protocol, garbage collection (GC) and tombstone cleanup must never delete shards that are still required by active committed generations or protected by recovery retention policies.

## 2. Invariants Implemented
In [src/coordinator/fencing.rs](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs), [`NodeEpochFencingTracker`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs) enforces three strict safety gates before executing any `DeleteShard` or `GarbageCollectShard` operation:

### Invariant 1: Retention Watermark Guard
Storage nodes maintain a per-object retention watermark `retention_watermarks: BTreeMap<String, u64>`.
If a permit requests deletion of generation $v \le \text{min\_retained\_gen}$, it is rejected with:
`"GC or shard deletion rejected: generation X <= retention watermark Y"`.

### Invariant 2: Recovery Checkpoint Guard
Nodes maintain an active set of protected recovery checkpoints `protected_checkpoints: BTreeSet<String>`.
If an object version is protected by an active checkpoint, deletion is rejected with:
`"GC or shard deletion rejected: generation X of object Y is protected by active recovery checkpoint"`.

### Invariant 3: Active Generation Protection
Storage nodes track the currently active committed generation for each object.
A storage node strictly rejects any permit attempting to delete the current active generation ($v \ge \text{active\_gen}$):
`"GC or shard deletion rejected: cannot delete active committed generation X"`.

## 3. Test Suite Verification
Validated in [tests/phase5_1_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_1_validation.rs):
- `test_20`: Deletion of generation $\le$ retention watermark is rejected.
- `test_21`: Deletion of checkpoint-protected generation is rejected.
- `test_22`: Deletion of current active generation is rejected.
