# FSP Phase 4.2: Coordinator High Availability & Quorum Consensus

## 1. Architecture Overview

In Phase 4.1, the coordinator represented a single point of failure (SPOF) and lacked automated failover safety. 
Phase 4.2 establishes an active-passive coordinator architecture governed by a **3-Member Metadata Quorum** with explicit fail-closed safety.

```text
                  +---------------------------+
                  |  Quorum Witness Node 03   |
                  +-------------+-------------+
                                |
               Quorum Vote RPCs | (Majority = 2/3)
                                |
         +----------------------+----------------------+
         |                                             |
+--------+------------------+              +-----------+---------------+
| Coordinator Primary (01)  |   Replication| Coordinator Standby (02)  |
| Active Leader (Epoch E)   |------------->| Shadow Replica (Standby)  |
+--------+------------------+     WAL      +---------------------------+
         |
Mutating | RPCs with Fencing Token (Epoch E)
         v
+------------------------------------------------------+
| Storage Node Cluster (Enforcing Highest Seen Epoch E)|
+------------------------------------------------------+
```

---

## 2. Quorum Consensus Model

### Membership & Quorum Sizing
- The cluster is configured with 3 voting members:
  - `CoordinatorPrimary`
  - `CoordinatorStandby`
  - `WitnessQuorumPeer` (or tertiary coordinator)
- Cluster size $N = 3$.
- Majority threshold $Q = \lfloor N/2 \rfloor + 1 = 2$ votes.

### Epoch Acquisition Algorithm
1. When an active leader fails, is killed via SIGKILL, or resigns gracefully, the candidate initiates an election for a strictly higher epoch $E_{new} > E_{current}$.
2. The candidate votes for itself and requests votes from its quorum peers.
3. A peer grants its vote only if:
   - $E_{new} > E_{peer\_seen}$
   - The peer has not already voted for another candidate in epoch $E_{new}$.
4. If $\ge 2$ votes are received, the candidate advances its on-disk [`DurableEpochStore`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs) (`<coord_root>/leadership/epoch.rec`) with an atomic `fsync` and assumes the `Leader` role.

### Fail-Closed Partition Behavior
> [!IMPORTANT]
> If a network partition isolates a coordinator such that it can only communicate with itself (1 vote $< 2$), it cannot achieve quorum. The coordinator transitions to `CoordinatorRole::Fenced` and immediately rejects all mutating operations (`Err(StorageError::QuorumUnavailable)`). Timestamp-only leases without quorum consensus are strictly prohibited.

---

## 3. Crash Recovery and State Reconciliation

Upon failover promotion, [`CoordinatorFailoverManager`](file:///home/rade/Project/Jobs/fsp/src/coordinator/failover.rs) performs automatic state reconciliation:
1. **Repair Journal Reconciliation:** Scans the persistent journal (`repair_jobs/*.rec`). Any jobs interrupted in `Repairing` or `CapacityCheck` state are transitioned back to `Pending` with reason `"reconciled after coordinator failover"`.
2. **Metadata Replay:** Replays all outstanding replication records from [`MetadataReplicationWal`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs) (`metadata_replication.wal`) to synchronize manifest generations and node registrations.
3. **Fencing Token Issuance:** Commits the new epoch to storage nodes on the first mutating RPC.
