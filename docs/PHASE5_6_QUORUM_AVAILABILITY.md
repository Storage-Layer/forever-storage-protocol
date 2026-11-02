# FSP Phase 5.6: Quorum Durability & Control-Plane Availability

## 1. Three-Voter Coordinator Architecture

The metadata control plane employs a 3-voter Raft-like quorum to manage cluster incarnation epochs and authorize mutations:
- **Total Voters ($V$):** 3 coordinator voters.
- **Majority Quorum ($Q$):** $\lfloor V / 2 \rfloor + 1 = 2$ voters required to form consensus.

---

## 2. Failure Mode Analysis

| Operational State | Active Voters | Control-Plane Status | Data Read Availability | Mutating Write Availability |
|:---|:---:|:---:|:---:|:---:|
| **Nominal Cluster** | 3 / 3 | **Healthy** | 100% available | 100% available |
| **Single Voter Crash** | 2 / 3 | **Degraded** ($2 \ge 2$) | 100% available | 100% available |
| **Dual Voter Crash** | 1 / 3 | **Failed Closed** ($1 < 2$) | **100% Emergency Read Available** | **0% (Fails closed)** |
| **All Voters Crash** | 0 / 3 | **Offline** | **100% Offline Emergency Read** | **0% (Fails closed)** |

---

## 3. Secure Emergency Read Bypass

When 2 or more coordinator voters are unavailable, the cluster control plane cannot elect a leader or issue mutation permits. However, storage data is **never locked away**:
- The standalone [`Secure Emergency Read`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs) CLI tool bypasses the coordinator.
- It connects directly to the authenticated storage nodes via TLS using root certificates.
- Retrieves available shards, verifies their container SHA-256 digests, and executes local Reed-Solomon decoding.
- This ensures data can always be recovered during catastrophic control-plane outages without risk of uncommitted state corruption.

Tested in `test_p5_6_21_quorum_loss_mutation_fails_closed_safely`.
