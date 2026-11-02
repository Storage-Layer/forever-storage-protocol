# FSP Phase 5.5: Failure Domain Validation & Multi-Host Resilience Proofs

## 1. Reed-Solomon RS(20, 12) Invariants

Forever Storage Protocol (FSP) distributes encoded erasure stripes across independent nodes using a $(N, K) = (20, 12)$ Cauchy Reed-Solomon code over $GF(2^8)$:
- **Data Shards ($K$):** 12 shards.
- **Parity Shards ($M = N - K$):** 8 shards.
- **Maximum Tolerable Shard Loss:** $M = 8$ shards.
- **Minimum Shards for Exact Byte Reconstruction:** $K = 12$ shards.

Any object can tolerate the loss or corruption of up to 8 arbitrary shards without any data loss. If $\ge 9$ shards are lost, the stripe fails closed with a cryptographically verifiable error rather than returning corrupt data.

---

## 2. 5-Zone Failure Topology Proof

In the canonical 20-node deployment ([`ClusterTopology::standard_5_zones_20_nodes`](file:///home/rade/Project/Jobs/fsp/src/cluster/mod.rs)):
- 20 nodes are evenly distributed across 5 failure domains (`zone-a`, `zone-b`, `zone-c`, `zone-d`, `zone-e`), with exactly **4 nodes per zone**.

### Theorem 1 (Two-Zone Survivability):
*Simultaneous catastrophic failure of any 2 failure zones preserves 100% data durability and read availability.*

**Proof:**
1. Let the 5 zones be $Z = \{A, B, C, D, E\}$, with $|Z_i| = 4$ for each $i \in Z$.
2. Suppose an arbitrary 2-zone outage occurs, e.g., $F = \{Z_i, Z_j\}$ where $i \neq j$.
3. Total lost nodes: $|F| \times 4 = 2 \times 4 = 8$ nodes.
4. Remaining surviving nodes: $N_{\text{surv}} = 20 - 8 = 12$ nodes.
5. Since each node holds at most 1 shard of a given stripe (enforced by `StripePlacementRecord` anti-affinity), the surviving nodes hold exactly 12 unique shards.
6. Because $N_{\text{surv}} = 12 \ge K = 12$, the Reed-Solomon decoder reconstructs the exact original object with zero data loss. $\blacksquare$

### Theorem 2 (Three-Zone Fail-Closed Safety):
*Catastrophic failure of 3 failure zones triggers fail-closed quorum abort, preventing silent data corruption or partial reads.*

**Proof:**
1. Suppose an outage affects 3 zones: $|F| = 3$.
2. Total lost nodes: $3 \times 4 = 12$ nodes.
3. Remaining surviving nodes: $20 - 12 = 8$ nodes.
4. Surviving shards available: $8 < K = 12$.
5. The reconstruction threshold is unmet ($8 < 12$). The client and coordinator abort with `StorageError::InsufficientShards { available: 8, required: 12 }`.
6. No corrupt, partial, or unverified payload is ever exposed to the caller. $\blacksquare$

---

## 3. Physical Multi-Host Allocation Strategies

When deploying FSP onto physical host machines, failure domains map directly to physical hardware boundaries:

### 5-Host Physical Mapping (Optimal):
- **Host 1 (Zone A):** 4 storage daemons (`node-00` .. `node-03`)
- **Host 2 (Zone B):** 4 storage daemons (`node-04` .. `node-07`)
- **Host 3 (Zone C):** 4 storage daemons (`node-08` .. `node-11`)
- **Host 4 (Zone D):** 4 storage daemons (`node-12` .. `node-15`)
- **Host 5 (Zone E):** 4 storage daemons (`node-16` .. `node-19`)
- **Durability Guarantee:** Any 2 physical server motherboards, power supplies, or ToR switch ports can fail completely without interrupting data availability.

### 3-Host Physical Mapping (Degraded / Asymmetric):
- **Host 1 (Zone A):** 7 storage daemons
- **Host 2 (Zone B):** 7 storage daemons
- **Host 3 (Zone C):** 6 storage daemons
- **Failure Boundary Analysis:**
  - If Host 1 fails: 7 nodes lost, 13 survive ($13 \ge 12$). System survives single host loss.
  - If Host 2 fails: 7 nodes lost, 13 survive ($13 \ge 12$). System survives single host loss.
  - If Host 3 fails: 6 nodes lost, 14 survive ($14 \ge 12$). System survives single host loss.
  - If any **2 hosts fail** simultaneously: at least $7 + 6 = 13$ nodes lost, leaving at most 7 nodes ($7 < 12$). **System cannot survive two-host failure in 3-host topology.**

### 2-Host Physical Mapping (Minimal Staging / Non-Resilient):
- **Host 1 (Zone A):** 10 storage daemons
- **Host 2 (Zone B):** 10 storage daemons
- **Failure Boundary Analysis:**
  - If either Host 1 or Host 2 fails: 10 nodes lost, leaving only 10 nodes ($10 < 12$).
  - **Neither host failure can be tolerated for reads or writes.** A 2-host topology is suitable only for network transport debugging, never for production durability.
