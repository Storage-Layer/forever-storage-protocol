# Phase 5.4 Failure Domain Validation & Erasure Coding Guarantees

## 1. Mathematical Guarantee

FSP uses Reed-Solomon RS(20,12) over GF($2^8$) with Cauchy generator matrices.
- $N = 20$ (Total shards)
- $K = 12$ (Data shards)
- $M = 8$ (Parity shards)
- Maximum tolerable shard loss: $M = 8$ shards.

In [`src/cluster/topology.rs`](file:///home/rade/Project/Jobs/fsp/src/cluster/topology.rs), the 20 shards are strictly mapped 1:1 across 5 zones:
```
Zone 0: Shards [0, 1, 2, 3]
Zone 1: Shards [4, 5, 6, 7]
Zone 2: Shards [8, 9, 10, 11]
Zone 3: Shards [12, 13, 14, 15]
Zone 4: Shards [16, 17, 18, 19]
```

## 2. Experimental Verification

### Experiment 1: Two Full Zones Outage
- **Test:** `test_p5_4_06_two_full_zones_outage_reconstruction`
- **Fault Injected:** Simulated sudden loss of Zone 1 (Nodes 4..7) and Zone 3 (Nodes 12..15), representing 8 killed nodes simultaneously.
- **Result:**
  - Alive Nodes: 12 nodes (Zones 0, 2, 4)
  - Alive Shards: 12 shards
  - Decoding result: 100% byte-exact match with original 100 MiB payload.
  - CRC32C and BLAKE3 verification passed.

### Experiment 2: Three Full Zones Outage (Fail-Closed)
- **Test:** `test_p5_4_07_three_full_zones_outage_fail_closed`
- **Fault Injected:** Simulated loss of Zones 0, 2, and 4 (12 nodes killed).
- **Result:**
  - Alive Nodes: 8 nodes (Zones 1 and 3)
  - Requirement: $K = 12$
  - Result: The decoder immediately rejected the reconstruction request with `FspError::InsufficientValidShards(8)`.
  - Zero partial or corrupt bytes were emitted to the caller.

### Experiment 3: LRC Local Group Repair
- **Test:** `test_p5_4_12_lrc_local_group_repair_reduced_io`
- **Configuration:** LRC(20,12,4) with local parity groups of size 4.
- **Scenario:** Single node failure in Zone 0 (Node 0 lost).
- **Result:**
  - Conventional Reed-Solomon reconstructs by reading $K=12$ surviving shards across all zones.
  - LRC Local Group reads only the 3 surviving shards in Zone 0 plus 1 local parity shard ($4$ shards total).
  - Network and disk IO reduced by **66.67%** (4 shards transferred instead of 12 shards).
