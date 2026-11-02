# Phase 4.4 Known Limitations & Production Caveats

**Document ID:** `PHASE4_4_LIMITATIONS.md`  
**Protocol Version:** FSP v0.4.4  
**Date:** 2026-11-01  
**Author:** Principal Distributed Systems Architect  

---

## 1. Explicit Implementation Caveats

In accordance with scientific rigor, the following operational limitations are documented and must not be misrepresented:

### 1. Physical Multi-Host Deployment: NOT EXECUTED
- **Status:** **NOT EXECUTED**
- **Details:** The FSP cluster configuration ([`cluster_5host.toml`](file:///home/rade/Project/Jobs/fsp/deploy/configs/cluster_5host.toml)) and systemd service templates ([`fsp-coordinator.service`](file:///home/rade/Project/Jobs/fsp/deploy/systemd/fsp-coordinator.service), [`fsp-node@.service`](file:///home/rade/Project/Jobs/fsp/deploy/systemd/fsp-node@.service)) have been formally authored and validated in preflight checks. However, deployment to physical bare-metal remote servers across WAN or multi-rack datacenter environments was **not executed** because external physical machines were not provisioned.
- **Impact:** Real WAN jitter, cross-switch packet reordering, and asynchronous OS kernel scheduling across physical NICs remain to be benchmarked in hardware staging.

### 2. 24-Hour Continuous Soak Run: NOT EXECUTED
- **Status:** **NOT EXECUTED**
- **Details:** To maintain reasonable testing turnaround and CI budgets, a bounded 500-operation soak execution was run using [`SoakTestHarness`](file:///home/rade/Project/Jobs/fsp/src/testing/soak_harness.rs). The full 24-hour continuous wall-clock soak run was **not executed**.
- **Impact:** Long-tail filesystem inode fragmentation and multi-day kernel memory page caching behavior require future extended staging runs.

---

## 2. Technical Limitations

1. **Quorum Size Flexibility:** Current coordinator high-availability requires exactly 3 voters (majority = 2). Dynamic membership reconfiguration (e.g., adding a 4th or 5th voter via Raft joint consensus) is not yet supported.
2. **Synchronous Quorum Mutation Overhead:** Gathering majority voter signatures on [`QuorumMutationPermit`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L320-L370) adds one network RTT between the leader and standby voters before a write can be delivered to storage nodes. (Mitigated by TLS connection pooling).
3. **Storage Node Local Compaction:** While metadata snapshots compact the coordinator WAL, storage node `.gen` and `executed_requests.rec` stores grow with the number of unique objects and mutations until garbage collected.
