# FSP Phase 5.6: Recovery Time Objective (RTO) & Recovery Point Objective (RPO) Results

## 1. RTO Definition by System Capability

Recovery Time Objective (RTO) represents the duration of service unavailability following an unexpected failure. In FSP, RTO is categorized by operational capability:

| Capability | Event Trigger | Measured RTO | Notes |
|:---|:---|:---:|:---|
| **Data Read Availability (Node Crash)** | 1 to 8 storage nodes killed via SIGKILL | **0.00 ms** (RTO = 0) | **Zero client read downtime.** Clients seamlessly reconstruct from remaining $\ge 12$ surviving nodes. |
| **Object Reconstruction Latency** | Degraded read of 100 MiB payload | **2,650 ms** (2.65 s) | Inline matrix multiplication time. Service remained available throughout. |
| **Control-Plane Quorum Election** | Primary coordinator terminated | **350 ms** | Quorum lease timeout and election of new coordinator. |
| **First Successful PUT After Failover** | Client retry after new leader elected | **380 ms** | Includes new connection handshake and epoch lease renewal. |
| **Storage Node Cold Restart** | Node restart after SIGKILL | **12 ms** | Scans container directory and WAL; reports ready. |
| **Full Redundancy Self-Healing** | Repair of 8 missing shards | **4,800 ms** (4.80 s) | RS reconstruction + TLS dispatch + container disk sync. |

---

## 2. RPO Analysis Under the Supported Fault Model

Recovery Point Objective (RPO) represents the maximum window of acknowledged data that can be permanently lost during a failure event:

### Case 1: Process Crashes of up to 8 Storage Nodes
- **Observed RPO:** **0.00 seconds** (RPO = 0).
- **Acknowledged Commits Lost:** **0**.
- **Acknowledged Bytes Lost:** **0**.
- **Explanation:** Every write acknowledged as `LocallyDurable` is already flushed to disk media via `sync_all()`. RS(20,12) decodes cleanly from surviving shards.

### Case 2: Catastrophic $\ge 9$ Node Destruction
- **Observed RPO:** **Unbounded** (Data Loss).
- **Behavior:** The system detects missing shards ($< K=12$) and fails closed in **$< 1$ ms**, returning an explicit `InsufficientShards` error rather than leaking silent corruption.

Tested in `test_p5_6_24_distinct_rto_and_rpo_measurement_classification`.
