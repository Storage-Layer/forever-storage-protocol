# FSP Phase 4.2: Known Limitations & Production Readiness Assessment

## 1. Network & Execution Scope

> [!WARNING]
> **Physical Multi-Host Hardware Deployment: NOT EXECUTED**
> All multi-node tests, failover simulations, and benchmark evaluations were conducted on a single physical host across localhost loopback ports (`127.0.0.1`). 
> **Physical multi-host deployments across distinct bare-metal machines, top-of-rack switches, and wide-area networks were NOT executed.** Real physical network hazards (asymmetric routing splits, cross-switch link saturation, packet fragmentation) must be evaluated in Phase 5 staging environments.

---

## 2. Extended Soak Testing Notice

> [!WARNING]
> **Extended 24-Hour Soak Testing: NOT EXECUTED**
> Bounded initial soak testing (15 minutes, 1,250 PUT/GET cycles, 40 node restarts, 15 elections) was executed and passed with zero data corruption events and stable memory usage. 
> Continuous 24-hour long-duration testing was **NOT EXECUTED** due to task execution time boundaries.

---

## 3. Remaining Single Points of Failure & Future Work

1. **Quorum Membership Dynamics:** The current quorum size is fixed at 3 members. Dynamic online membership changes (e.g. adding or removing quorum nodes via Raft joint consensus) are planned for Phase 5.
2. **Metadata Replay Throughput:** Metadata WAL replay is single-threaded; large clusters with millions of objects will require snapshot compaction and parallel replay.
3. **WAN Traffic Control:** Loopback testing does not simulate high BDP (Bandwidth-Delay Product) WAN networks.
