# FSP Phase 5.7: Physical Failure Domains & Topology Resilience

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_FAILURE_DOMAIN_VALIDATION.md`  
**Test Reference:** `tests/phase5_6_validation.rs:test_p5_6_22`  
**Deploy Staging:** [`deploy/phase5_7/inventory/hosts.ini`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/inventory/hosts.ini)  

---

## 1. The Fallacy of Logical Failure Domains

In single-host localhost testing, "zones" or "failure domains" are merely logical labels (`zone-0`, `zone-1`, `zone-2`). While logical domains protect against software misconfigurations or simulated crashes of individual processes, they provide **zero resilience against physical hardware events**:
- Physical host motherboard / CPU failure terminates all logical instances on that host.
- Top-of-Rack (ToR) switch failure severs network connectivity for all nodes in that rack.
- Uninterruptible Power Supply (UPS) failure causes instantaneous power loss across the chassis.

---

## 2. Mathematical Fault Tolerance by Topology

To provide genuine physical resilience, FSP's 20 shards ($N=20, K=12$) must be mapped across independent physical machines such that losing any set of physical machines leaves at least $K=12$ surviving shards.

### Case A: Two-Host Topology Profile (`deploy/phase5_7/two_host/`)
- Host A: 10 storage nodes (shards $0..9$).
- Host B: 10 storage nodes (shards $10..19$).
- Failure Scenario: Host A suffers power failure or physical destruction.
- Shards Lost: 10.
- Shards Surviving: 10.
- Minimum Required for RS(20,12): $K=12$.
- **Result:** **DATA LOSS ($10 < 12$).**
- **Conclusion:** Two physical hosts **cannot survive a single host failure** under RS(20,12). It is strictly an experimental setup for testing remote LAN connectivity.

### Case B: Three-Host Topology Profile (`deploy/phase5_7/three_host/`)
- Host 1: 7 storage nodes (shards $0..6$).
- Host 2: 7 storage nodes (shards $7..13$).
- Host 3: 6 storage nodes (shards $14..19$).
- Failure Scenario: Host 1 or Host 2 suffers catastrophic hardware failure.
- Shards Lost: $\max(7) = 7$.
- Shards Surviving: $20 - 7 = \mathbf{13}$.
- Minimum Required: $K=12$.
- **Result:** **SURVIVES ($13 \ge 12$).**
- **Conclusion:** Three physical hosts represent the **minimal viable deployment** for single physical machine fault tolerance under RS(20,12).

### Case C: Five-Host Production Profile (`deploy/phase5_7/five_host/`)
- Host 1 through Host 5: Exactly 4 storage nodes per physical machine.
- Failure Scenario: Two full physical machines crash simultaneously (e.g. rack power circuit trips).
- Shards Lost: $4 + 4 = 8$ shards.
- Shards Surviving: $20 - 8 = \mathbf{12}$.
- Minimum Required: $K=12$.
- **Result:** **SURVIVES ($12 == K$).**
- **Consensus Quorum:** 5 coordinators/voters across 5 hosts require $\lfloor 5/2 \rfloor + 1 = 3$ votes. With 2 hosts down, 3 remain online, preserving consensus quorum.
- **Conclusion:** Five physical hosts provide **dual-machine fault tolerance and quorum preservation**, forming the baseline for enterprise production readiness.

---

## 3. Placement Policy Rule Enforcement

In `src/coordinator/placement.rs`, the placement engine enforces `PlacementPolicy::SpreadAcrossZones`:
$$\forall \text{ zone } Z, \quad |\text{shards in } Z| \le \left\lceil \frac{N}{|Z|} \right\rceil$$
When configured with physical hosts as zones, the coordinator rejects any placement that overconcentrates shards on a single machine, guaranteeing that physical failure bounds are mathematically respected.
