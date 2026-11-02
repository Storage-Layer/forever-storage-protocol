# FSP Phase 5.3: Concurrent Garbage Collection Lifecycle & TOCTOU Elimination

## 1. Problem Definition: Time-of-Check to Time-of-Use (TOCTOU) in Storage GC
In distributed storage engines with asynchronous repair and background garbage collection:
1. **GC vs. Active Repair Race:** Shard $S_i$ is evaluated by the GC engine and deemed unreferenced (e.g., from an superseded version $v_1$). Before $S_i$ is unlinked, an adaptive repair job concurrently selects $S_i$ as a reconstruction target or reconstruction source. If GC deletes $S_i$, the repair job either fails or corrupts data redundancy.
2. **GC vs. Concurrent Commit Race:** A client issues a new commit advancing the object from $v_1 \to v_2$. If GC asynchronously unlinks shards based on stale metadata cached prior to commit, committed shards can be destroyed.
3. **Rollback Re-activation Race:** If a coordinator rolls back to a prior snapshot, shards previously marked for reclamation may suddenly become active again.

## 2. Multi-Stage Deletion Lifecycle State Machine
To guarantee safety, Phase 5.3 introduces an explicit 6-stage lifecycle for shard deletion:

```
[Candidate]
    │
    ▼  (audit_gc_eligibility)
[EligibilityChecked]
    │
    ▼  (reserve_shard_deletion + lease)
[DeletionReserved]
    │
    ▼  (tombstone_shard_deletion)
[Tombstoned]
    │
    ▼  (quarantine timeout)
[Quarantined]
    │
    ▼  (commit_physical_deletion: re-verify no repair, version < active_gen)
[PhysicallyDeleted]
```

### Invariants Enforced by State Machine
1. **Active Repair Mutual Exclusion:**
   [`reserve_shard_deletion`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs) checks `is_shard_repairing(object_id, shard_id)`. If an active repair reservation exists, deletion reservation is immediately rejected with `RpcError::Unauthorized`.
2. **Pre-Deletion Revalidation:**
   [`commit_physical_deletion`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs) re-checks:
   - Shard is not undergoing active repair.
   - Version being deleted is strictly strictly lower than the currently recorded generation watermark (`version < cur_gen`).
   - Node has not been rolled back to a snapshot where this version is active.
3. **Redundancy Floor Invariant ($k=12$):**
   No deletion is permitted if surviving shards across all failure domains drop at or below $k=12$. Deletion requires surviving shards $\ge 13$.

Verified by tests:
- [`test_13_concurrent_gc_and_repair_on_same_shard_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L827-L865)
- [`test_14_concurrent_gc_reservation_and_manifest_commit_toctou_prevented`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L867-L915)
- [`test_15_gc_after_snapshot_rollback_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L917-L960)
- [`test_16_protected_checkpoint_and_retained_historical_shard_gc_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L962-L990)
- [`test_17_gc_redundancy_and_failure_domain_policy_violation_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L992-L1015)
- [`test_18_deletion_lifecycle_tombstone_and_physical_deletion`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L1017-L1060)
