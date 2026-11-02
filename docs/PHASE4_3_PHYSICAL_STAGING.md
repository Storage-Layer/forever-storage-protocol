# FSP Phase 4.3: Physical Multi-Host Staging & Failure Domain Validation

> [!IMPORTANT]
> **Status Notice:** Physical multi-host cluster deployment was **NOT EXECUTED** on physical hardware in this session. All testing was executed on the local development environment using realistic multi-process topologies, discrete failure domain models, and network-level abstractions.

---

## 1. Physical Staging Architecture

FSP Phase 4.3 validates the mathematical shard placement and durability guarantees for real multi-host deployments:
- **Target Topology:** 5 Physical Server Hosts ($H_0 \dots H_4$)
- **Physical Racks:** 5 Independent Racks ($R_0 \dots R_4$)
- **Storage Nodes:** 20 Storage Node daemons (4 daemons per host)
- **Erasure Coding Scheme:** Systematic Cauchy RS(20,12) ($k=12$ data shards, $m=8$ parity shards, $n=20$ total shards)

---

## 2. Failure Domain Durability Validation

Placement constraints are evaluated by [`validate_placement_durability`](file:///home/rade/Project/Jobs/fsp/src/placement/failure_domains.rs#L113):
- **Tolerated Host Failures ($F$):** $F=2$.
- **Max Shards per Host:** $\lfloor 20 / 5 \rfloor = 4$ shards.
- **Max Shards per Rack:** 4 shards.
- **Worst-Case 2-Host Catastrophic Loss:**
  $$\text{Lost Shards} = 2 \times 4 = 8 \text{ shards}$$
  $$\text{Surviving Shards} = 20 - 8 = 12 \text{ shards}$$
- **Recoverability Criterion:**
  $$\text{Surviving Shards} (12) \ge k (12)$$
  The Cauchy RS(20,12) generator matrix guarantees that **any** subset of $k=12$ shards has full rank and can decode the complete original object without data loss.

Verified by [`tests/phase4_3_validation.rs::test_p4_3_17_physical_failure_domain_staging_topology_validation`](file:///home/rade/Project/Jobs/fsp/tests/phase4_3_validation.rs#L648).
