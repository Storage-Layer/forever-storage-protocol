# Phase 4.4 Failure Injection Experiments: Host Loss & Redundancy Restoration

**Document ID:** `PHASE4_4_FAILURE_INJECTION.md`  
**Protocol Version:** FSP v0.4.4  
**Date:** 2026-11-01  
**Author:** Principal Distributed Systems Engineer, Storage Reliability Engineer  

---

## 1. Experimental Overview

To validate physical resilience against severe infrastructure loss, Phase 4.4 evaluated the behavior of FSP under catastrophic multi-host outages and subsequent self-healing workflows.

The experiments simulated:
1. Complete simultaneous physical loss of 2 out of 5 hosts (`host-alpha` and `host-bravo`).
2. Immediate degraded read requests during the ongoing outage.
3. Adaptive repair triggers to reconstruct missing shards onto spare capacity.
4. Redundancy verification confirming 100% data recoverability.

---

## 2. Experiment 1: Double Physical Host Loss & Degraded Reads

- **Target Object:** 100 MiB multi-stripe dataset encoded with Systematic RS(20,12).
- **Failure Injection:** Total simulated destruction of `host-alpha` and `host-bravo`.
- **Impact:** Shards 0 through 7 (8 shards out of 20 total) completely erased from the cluster.
- **Survivors:** 12 shards (shards 8 through 19) survive across `host-charlie`, `host-delta`, and `host-echo`.

### Result
In [`test_p4_4_20_full_physical_host_loss_degraded_read_and_rs20_12_recovery`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L980-L1010):
- Degraded read executed against the 12 surviving shards.
- The systematic Vandermonde generator matrix inverted the $12 \times 12$ submatrix.
- **Payload Reconstructed:** Exactly matching original SHA-256 digest with **zero byte errors**.
- **Data Loss Events:** **0 (Zero)**.

---

## 3. Experiment 2: Full Redundancy Restoration via Adaptive Repair v2

- **Procedure:** Following the loss of 8 shards, the coordinator's adaptive repair engine was triggered.
- **Target:** Restore all 8 missing shards to available spare nodes across surviving failure domains.

### Result
In [`test_p4_4_21_full_redundancy_restoration_adaptive_repair_v2`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L1012-L1040):
- Missing shards 0..7 reconstructed using decoded payload.
- All 20 shards rewritten, verified against original stripe hashes, and committed.
- Redundancy restored back to 20/20 shards ($1.667\times$ safety factor).

---

## 4. Key Observations

1. **Deterministic Recovery:** Systematic RS(20,12) provides deterministic single-pass decoding without iterative approximations.
2. **Bandwidth Efficiency:** Reconstructing missing shards requires reading only $k=12$ shards rather than the entire cluster, saving 40% network read bandwidth compared to naive full replication.
3. **No Stale Overwrites:** Epoch watermarks and commit index checks prevent uncoordinated repair threads from writing obsolete data.
