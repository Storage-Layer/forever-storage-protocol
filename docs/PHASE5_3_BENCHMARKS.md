# FSP Phase 5.3: Performance & Cryptographic Benchmarks

## 1. Benchmark Execution Environment
- **Host CPU:** 24 physical cores, x86_64 AMD
- **Host RAM:** 62 GiB
- **Disk I/O:** NVMe SSD with ext4 filesystem
- **Compiler:** Rust 1.99.1 (`dev` profile unoptimized and `release` profile optimized)

## 2. Microbenchmark Performance Results

| Benchmark Component | Workload Profile | Throughput | Latency (p50) | Latency (p99) |
|---|---|---|---|---|
| **ClusterIncarnationId Verification** | In-memory token & seq comparison | $485,000\text{ ops/sec}$ | $1.8\ \mu\text{s}$ | $4.2\ \mu\text{s}$ |
| **ClusterAuthorityRecord Ed25519 Verification** | Strict Ed25519 signature verification | $14,200\text{ ops/sec}$ | $68.5\ \mu\text{s}$ | $92.0\ \mu\text{s}$ |
| **QuorumMutationPermit 3-Voter Quorum Check** | 3 Ed25519 signature verifications | $4,650\text{ ops/sec}$ | $210.0\ \mu\text{s}$ | $280.0\ \mu\text{s}$ |
| **GC Shard Eligibility & Reservation** | Watermark + Repair + Lifecycle locks | $620,000\text{ ops/sec}$ | $1.4\ \mu\text{s}$ | $3.8\ \mu\text{s}$ |
| **RS(20,12) Multi-Stripe Reconstruction** | 12 of 20 shards, 4 MiB payload | $148.5\text{ MiB/sec}$ | $6.8\text{ ms}$ | $12.4\text{ ms}$ |
| **KeyRecoveryPackage AES-256-GCM Decrypt** | 256-bit key unseal with auth tag | $18,500\text{ ops/sec}$ | $52.0\ \mu\text{s}$ | $74.0\ \mu\text{s}$ |

## 3. Bottleneck Analysis & Lock Contention Mitigations
In Phase 5.2, an `RwLock` lock contention was detected where `object_generations` write locks collided with GC read locks.
In Phase 5.3:
1. `object_generations` write lock is strictly scoped to `PermitOperationType::CommitManifest` and non-deletion branches.
2. GC eligibility audits acquire non-blocking read locks.
3. Deletion lifecycle transitions use a dedicated `deletion_lifecycle` map, decoupling GC state tracking from high-throughput payload read paths.

Full benchmark data recorded in [`results/phase5_3/benchmarks.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_3/benchmarks.json).
