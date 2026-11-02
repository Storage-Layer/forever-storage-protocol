# FSP Phase 4.1 — Capacity Management & Dynamic Topology

## 1. Cluster Health States

Phase 4.1 formalizes five explicit cluster health states in [`ClusterHealthState`](file:///home/rade/Project/Jobs/fsp/src/coordinator/repair_journal.rs):

1. **`FullyProtected`**: Every object has all $n$ shards healthy and placed across isolated failure domains.
2. **`DegradedButRecoverable`**: Shard failures have occurred, but $\ge k$ surviving shards are available and healthy.
3. **`TemporarilyUnavailable`**: Surviving shards $\ge k$, but transient partitions or node reboots prevent immediate quorum retrieval.
4. **`RepairBlockedByCapacity`**: Degraded objects exist, but no eligible spare nodes meet the placement and failure domain isolation criteria.
5. **`IrrecoverablyLost`**: Data loss; surviving shards $< k$ for one or more stripes.

## 2. Dynamic Spare Node Onboarding

When a new spare node joins or re-registers with the coordinator:
- The coordinator invokes `wake_pending_repairs()`.
- All jobs in `WaitingForCapacity` or `RetryPending` transition back to `Pending`.
- The repair planner re-evaluates candidates and immediately schedules reconstruction.
