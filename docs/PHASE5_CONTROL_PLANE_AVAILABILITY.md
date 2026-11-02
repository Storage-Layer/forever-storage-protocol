# Phase 5 Control Plane Availability vs. Data Durability
**Forever Storage Protocol (FSP)**
**Date:** 2026-11-01  
**Author:** Principal Distributed Systems Architect & Reliability Research Scientist  
**Relevant Modules:** [`src/coordinator/leadership.rs`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs), [`src/cli/mod.rs`](file:///home/rade/Project/Jobs/fsp/src/cli/mod.rs)

---

## 1. The Fundamental Asymmetry: Durability vs. Availability

A critical insight demonstrated in Phase 5 is the mathematical separation between **Data Durability** (the persistent survival of byte information) and **Service Availability** (the live readiness of the metadata control plane to coordinate operations).

In a 5-host physical cluster deploying systematic Reed–Solomon $\text{RS}(20,12)$ erasure coding:
- 20 shards are stored across 5 independent physical hosts ($4$ shards per host).
- When any $2$ physical hosts fail simultaneously:
  $$\text{Shards Lost} = 2 \times 4 = 8 \text{ shards}$$
  $$\text{Shards Surviving} = 20 - 8 = 12 \text{ shards}$$
- Since the surviving shards $12 \ge k=12$, **the payload data is 100% durable, intact, and mathematically reconstructable**.

However, the fate of the **control plane** depends entirely on the metadata quorum design:

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│              Simultaneous Loss of 2 Physical Hosts (e.g. Host 1 & Host 2)       │
├──────────────────────────────────────┬──────────────────────────────────────────┤
│ 3-Voter Quorum (Hosts 1, 2, 3)       │ 5-Voter Quorum (1 Voter per Host 1..5)   │
├──────────────────────────────────────┼──────────────────────────────────────────┤
│ - Voters lost: 2 (Hosts 1 & 2)       │ - Voters lost: 2 (Hosts 1 & 2)           │
│ - Voters surviving: 1 (Host 3)       │ - Voters surviving: 3 (Hosts 3, 4, 5)    │
│ - Threshold needed: 2 of 3           │ - Threshold needed: 3 of 5               │
│ - Quorum state: LOST (1 < 2)         │ - Quorum state: SURVIVED (3 >= 3)        │
│ - Control plane: FAILS CLOSED        │ - Control plane: ONLINE & OPERATIONAL    │
│ - Online mutations: HALTED           │ - Online mutations: ACTIVE               │
│ - Online reads: HALTED               │ - Online reads: ACTIVE                   │
│ - Data Durability: 100% INTACT       │ - Data Durability: 100% INTACT           │
└──────────────────────────────────────┴──────────────────────────────────────────┘
```

---

## 2. Quantitative Model & Quorum Comparison

| Quorum Model | Cluster Size ($N$) | Threshold ($\lfloor N/2 \rfloor + 1$) | Max Voter Losses Tolerated | Aligned with RS(20,12) 2-Host Loss? |
|---|---|---|---|---|
| **3-Voter Quorum** | 3 | 2 | 1 host loss | **NO** (Losing 2 hosts destroys control plane) |
| **5-Voter Quorum** | 5 | 3 | 2 host losses | **YES** (Survives any 2 host losses with active quorum) |
| **7-Voter Quorum** | 7 | 4 | 3 host losses | Exceeds data plane tolerance ($20 - 12 = 8 < 3 \times 4$) |

### Formalization of 5-Voter Quorum in [`QuorumState`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs)
FSP's [`QuorumState`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs) dynamically calculates:
$$\text{cluster\_size} = |\text{peers}| + 1$$
$$\text{quorum\_threshold} = \left\lfloor \frac{\text{cluster\_size}}{2} \right\rfloor + 1$$

When configured with 4 peer coordinators (total 5 voters distributed across the 5 hosts), the cluster tolerates the loss of any 2 hosts without dropping quorum or losing leadership authority.

---

## 3. Emergency Offline Read Mode

When operating in legacy 3-voter configurations or during catastrophic network partitions where the control plane has failed closed, **data must never be held hostage by a stalled coordinator**.

Phase 5 introduces the Emergency Offline Reader (`fsp emergency-read`):

```bash
fsp emergency-read --id <object-id> --config deploy/phase5/cluster_5host.toml --output /path/to/restored.bin
```

### Architecture of Emergency Offline Recovery:
1. **Direct Storage Root Access:** The tool accesses local or NFS/SAN mounted storage roots (`data/node_*`) directly without contacting coordinator daemons or acquiring network locks.
2. **Autonomous Shard Discovery:** Scans `<storage_root>/objects/<object-id>/` for container files (`shard-*.fspc`) and individual shards.
3. **Cryptographic Self-Verification:** Each shard's SHA-256 hash is independently verified against container metadata. Corrupted shards are discarded.
4. **Direct Systematic Cauchy RS(20,12) Reconstruction:** As long as any 12 verified shards survive, the Cauchy decoder mathematically reconstructs the original uncorrupted byte stream.
5. **Zero Quorum Dependency:** Does not require leader elections, WAL commits, or active network consensus.

This mechanism was empirically verified in [`test_p5_06_emergency_offline_read_path_when_control_plane_unavailable`](file:///home/rade/Project/Jobs/fsp/tests/phase5_validation.rs), where 2 entire physical hosts were destroyed, the coordinator was terminated, and the emergency tool reconstructed the exact byte payload with 100% fidelity.

---

## 4. Operational Recommendations

1. **Deploy 5 Voters in Production:** When using RS(20,12) over 5 physical machines, deploy 1 coordinator voter on each of the 5 machines. This synchronizes the durability failure boundary (2 hosts) with the availability failure boundary (2 hosts).
2. **Never Force Single-Voter Overrides Online:** If quorum is lost, do not attempt split-brain unilateral commits. Instead, allow the system to fail closed for writes, and use `fsp emergency-read` for critical read availability until physical hosts are restored.
