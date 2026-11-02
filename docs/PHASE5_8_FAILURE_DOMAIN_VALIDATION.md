# FSP Phase 5.8: Physical Failure Domain Bounds & Topology Safety Proof

## 1. Failure Domain Mathematical Principles

The Reed-Solomon codec requires that at least $K=12$ valid coded shards survive from any stripe of $N=20$ shards to guarantee non-destructive data reconstruction.

When deploying storage instances across physical machines (hosts), the placement planner must enforce an upper bound on the number of shards assigned to any single physical host or rack failure domain:

$$\text{Max Shards Per Failure Domain} \le N - K = 20 - 12 = 8$$

---

## 2. Multi-Host Topology Failure Bounds

### 2.1 Two-Host Configuration (50/50 Shard Distribution)
- **Host Allocation:** Host A = 10 shards, Host B = 10 shards.
- **Single Host Loss:** If Host A or Host B experiences a catastrophic failure, 10 shards are lost simultaneously.
- **Surviving Shards:** $20 - 10 = 10$.
- **Reconstruction Condition:** $10 < K=12$. Quorum is strictly unsatisfied.
- **System Verdict:** Fail-closed. Data cannot be reconstructed until the offline host is restored or its storage volumes are attached elsewhere. A 2-host topology is strictly for transport/connectivity validation, never for disaster tolerance.

### 2.2 Three-Host Configuration (Balanced Distribution)
- **Host Allocation:** Host 1 = 7 shards, Host 2 = 7 shards, Host 3 = 6 shards.
- **Single Host Loss:** Losing Host 1 or Host 2 destroys at most 7 shards.
- **Surviving Shards:** $20 - 7 = 13$.
- **Reconstruction Condition:** $13 \ge K=12$. Quorum is satisfied with 1 spare shard.
- **System Verdict:** Full fault tolerance against the loss of any single physical machine.

### 2.3 Five-Host Production Configuration (Symmetric Distribution)
- **Host Allocation:** 4 shards per host across 5 independent racks/hosts.
- **Two Host Loss:** Losing any two machines simultaneously destroys $2 \times 4 = 8$ shards.
- **Surviving Shards:** $20 - 8 = 12$.
- **Reconstruction Condition:** $12 \equiv K=12$. Quorum is satisfied exactly.
- **Three Host Loss:** Destroys 12 shards; 8 survive ($8 < 12$). Fails closed without corrupting or falsifying data.
- **System Verdict:** Enterprise-grade tolerance against 2 concurrent physical host or top-of-rack switch failures.

---

## 3. Test Verification

Verified by automated integration test:
- `test_p5_8_17_physical_topology_safety_bounds` (Passed)
