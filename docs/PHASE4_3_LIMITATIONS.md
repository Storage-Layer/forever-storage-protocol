# FSP Phase 4.3: Engineering Limitations & Production Readiness Audit

This document explicitly delineates what has been formally implemented and proven versus the remaining operational boundaries and known limitations of the Forever Storage Protocol at the end of **Phase 4.3**.

---

## 1. Explicitly NOT Executed or Claimed

1. **Physical Multi-Host Deployment: NOT EXECUTED.**
   While 5-host physical failure domain placement algorithms, configuration parsing, and network communication were validated under simulated distributed multi-process environments, physical deployment across distinct bare-metal server chassis was not executed during this development session.
2. **24-Hour Continuous Soak Test: NOT EXECUTED.**
   All Soak/Stress tests in Phase 4.3 were bounded execution runs (up to 15-minute runs in Phase 4.2 and bounded model checker explorations in Phase 4.3). Long-term multi-day soak testing under continuous disk fill and temperature throttling remains future work.
3. **Dynamic Quorum Cluster Membership Changes:**
   The quorum cluster is strictly configured for 3 voting members ($C_1, C_2, C_3$). Online cluster membership transitions (e.g. Raft joint consensus for dynamically growing from 3 to 5 voters) are not implemented.

---

## 2. Operational Limitations

1. **Disk I/O Bottlenecks Under Large Object Bursts:**
   Synchronous fsync barriers in `MetadataReplicationWal` and `DurableEpochStore` introduce latency overhead on rotating HDDs (mitigated on NVMe SSDs).
2. **Quorum Voter Availability:**
   A 3-node quorum cluster can tolerate the loss of at most 1 coordinator node ($F=1$). Losing 2 coordinators blocks all metadata commits (fail-closed design), requiring manual or scripted disaster recovery from signed backup bundles.
3. **Network MTU and Jumbo Frames:**
   Container v2 multi-stripe serialization aligns to 12,288-byte blocks; standard 1500-byte MTU interfaces require TCP packet fragmentation.
