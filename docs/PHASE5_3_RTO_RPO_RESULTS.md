# FSP Phase 5.3: Recovery Time Objective (RTO) & Recovery Point Objective (RPO) Results

## 1. Measured Recovery Time Objective (RTO)
All operations were evaluated on an AMD 24-core x86_64 host running Linux 6.14.

| Recovery Operation | Target Budget | Measured Duration | Performance Margin | Status |
|---|---|---|---|---|
| **Isolated Disaster Rebootstrap** | $< 5.0\text{ s}$ | $32.4\text{ ms}$ | $154\times$ faster | PASS |
| **Node Incarnation Reload on Daemon Restart** | $< 1.0\text{ s}$ | $1.2\text{ ms}$ | $833\times$ faster | PASS |
| **Offline Emergency Read (4 MiB, 8 shards lost)** | $< 15.0\text{ s}$ | $2.84\text{ s}$ | $5.3\times$ faster | PASS |
| **GC Deletion Audit & Shard Reservation** | $< 0.5\text{ s}$ | $0.45\text{ ms}$ | $1111\times$ faster | PASS |
| **Full Coordinator Failover & Permitted PUT** | $< 10.0\text{ s}$ | $1.42\text{ s}$ | $7.0\times$ faster | PASS |

## 2. Recovery Point Objective (RPO) Guarantees

### Committed State RPO: 0 Seconds
For any write mutation that successfully acquired a quorum permit and committed to the replicated log prior to disaster:
- The snapshot index and WAL entries durably guarantee exact point-in-time recovery without loss of committed manifest generations.
- In test [`test_23_coordinator_sigkill_during_restore_preserves_atomic_state`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs), simulated SIGKILL crash during restore produced zero state corruption or lost commits.

### In-Flight Mutations RPO: Bounded to In-Flight Request Window
Write transactions in-flight during a coordinator crash that did not achieve majority permit quorum are discarded fail-closed. Storage nodes refuse uncertified permits upon rebootstrap.

### Post-Disaster Branching RPO: 0 Divergence
By binding all storage node permits to `ClusterIncarnationId`, stale coordinators cannot execute destructive mutations or delayed writes, guaranteeing zero divergence post-recovery.

Full metrics recorded in [`results/phase5_3/rto_rpo.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_3/rto_rpo.json).
