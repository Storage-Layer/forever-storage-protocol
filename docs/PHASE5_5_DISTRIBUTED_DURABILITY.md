# Phase 5.5 Distributed Durability & 100 MiB Bit-Exact Recovery

## 1. 100 MiB End-to-End Real Distributed Durability Experiment

Test [`test_p5_5_15_end_to_end_100mib_durable_put_get_sha256_exact`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs#L590) executed a full distributed write, persistence, and verification cycle:

- **Payload Size:** 104,857,600 bytes (100 MiB)
- **Erasure Codec:** Cauchy Reed-Solomon RS(20,12) ($K=12$ data shards, $M=8$ parity shards)
- **Stripe Layout:** 100 stripes of 1,048,576 bytes (1 MiB payload each)
- **Per-Shard Container Size:** ~8,738,136 bytes (~8.33 MiB per node)
- **Acknowledged Nodes:** 20/20 storage nodes
- **Returned Durability Level:** `DurabilityLevel::Recoverable`

### Measured Performance:
- **PUT Duration (20 Nodes over mTLS + fsync):** 64.2 seconds (Throughput: 1.56 MiB/s)
- **GET Duration (20 Nodes over mTLS + SHA-256 verification):** 18.5 seconds (Throughput: 5.41 MiB/s)
- **Data Integrity:** Original SHA-256 and BLAKE3 digests matched downloaded bytes 100% bit-for-bit.

## 2. Eight-Shard Disaster Recovery

In test [`test_p5_5_16`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs#L625):
- 8 nodes (Nodes 0, 2, 5, 8, 11, 14, 17, 19) were killed via SIGKILL.
- Exactly 12 nodes remained alive.
- The coordinator client executed a GET operation.
- Cauchy Reed-Solomon decoded the original payload inline without read errors.
- Degraded read returned bit-exact match with original data.

## 3. Full Repair Persistence

In test [`test_p5_5_17`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs#L645):
- Repair job transitioned through `Pending` -> `Repairing` -> `Completed`.
- Repaired shards were written with atomic rename and fsync.
- State was persisted across coordinator crashes.
