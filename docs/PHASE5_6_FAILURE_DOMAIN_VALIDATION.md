# FSP Phase 5.6: Failure Domain Validation & Multi-Host Resilience Proofs

## 1. Mathematical Formalism of RS(20, 12) Failure Tolerances

Forever Storage Protocol encodes every erasure stripe into $N = 20$ shards, requiring any $K = 12$ distinct shards to reconstruct the original data bytes. The maximum tolerable lost shards is:
$$M = N - K = 20 - 12 = 8 \text{ shards}$$

When mapping shards to physical machines or failure zones, durability is determined by the shard allocation across those physical boundaries.

---

## 2. Multi-Host Hardware Topologies

### Topology 1: Five Independent Physical Hosts (Optimal)
- **Allocation:** Exactly 4 nodes (and 4 shards per stripe) per host.
  $$\text{Host}_1: 4, \quad \text{Host}_2: 4, \quad \text{Host}_3: 4, \quad \text{Host}_4: 4, \quad \text{Host}_5: 4$$
- **Single Host Loss:** $20 - 4 = 16$ shards survive ($16 \ge 12$). System survives with 4 spare parity shards remaining.
- **Double Host Loss:** $20 - 8 = 12$ shards survive ($12 = K$). System survives and reconstructs 100% of data.
- **Triple Host Loss:** $20 - 12 = 8$ shards survive ($8 < 12$). System fails closed immediately; zero corrupt bytes served.
- **Hardware Fault Tolerance:** **2 Physical Host Failures Tolerated.**

### Topology 2: Three Independent Physical Hosts (Degraded)
- **Allocation:** 7 shards on Host A, 7 shards on Host B, 6 shards on Host C.
  $$\text{Host}_A: 7, \quad \text{Host}_B: 7, \quad \text{Host}_C: 6$$
- **Single Host Loss (Worst Case Host A or B):** $20 - 7 = 13$ shards survive ($13 \ge 12$). System survives.
- **Double Host Loss (Any 2 Hosts):** At least $7 + 6 = 13$ shards lost; at most 7 survive ($7 < 12$).
- **Hardware Fault Tolerance:** **1 Physical Host Failure Tolerated.**

### Topology 3: Two Independent Physical Hosts (Pilot Staging Only)
- **Allocation:** 10 shards on Host A, 10 shards on Host B.
- **Single Host Loss:** $20 - 10 = 10$ shards survive ($10 < 12$).
- **Hardware Fault Tolerance:** **0 Physical Host Failures Tolerated.**
- **Caution:** A 2-host deployment cannot tolerate losing either physical host. It is valid only for network transport debugging, never for production durability.

Tested in `test_p5_6_01_two_host_configuration_and_resilience_bounds` and `test_p5_6_22_physical_failure_domain_3host_and_5host_fault_tolerance`.
