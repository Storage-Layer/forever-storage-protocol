# FSP Phase 4.1: Known Limitations & Production Readiness Assessment

## 1. Network & Deployment Scope

> [!WARNING]
> **Single-Host Loopback Evaluation Only**
> All multi-node distributed tests, TLS 1.3 handshakes, and self-healing scenarios were executed on a single host across localhost loopback ports (`127.0.0.1`). 
> **Physical multi-host, cross-datacenter, and wide-area network (WAN) deployments were NOT executed.** Real-world network hazards such as asymmetrical latency, MTU packet fragmentation, BGP route flapping, and cross-switch partition splits must be evaluated in Phase 5 staging environments.

---

## 2. Test Harness Concurrency Constraints

### File Descriptor Saturation (`EMFILE`)
- Each integration test launches between 20 and 28 independent node server threads and mTLS client sockets.
- Running the full integration test suite with high parallelism exhausts the default OS file descriptor limit (`os error 24: Too many open files`).
- **Mitigation:** Tests must be executed using `--test-threads=1` or with elevated `ulimit -n 65536`.

---

## 3. High Availability & Coordinator Clustering

- **Active-Passive HA Model:** The failover architecture is designed around shared lease mechanisms and durable journal replay (documented in [`docs/PHASE4_1_COORDINATOR_HA.md`](file:///home/rade/Project/Jobs/fsp/docs/PHASE4_1_COORDINATOR_HA.md)).
- **Consensus Integration:** A fully distributed consensus engine (e.g., Raft or Paxos) for automatic multi-leader quorum replication of manifest generation state has not been implemented. Coordinator failover currently relies on external shared storage or explicit lease management.

---

## 4. Network Client Connection Overhead

- Current test client RPC utilities establish a new TCP socket and perform a complete TLS 1.3 handshake for each storage operation.
- This creates connection establishment latency that limits single-client distributed PUT throughput to $\approx 26.5\text{ MiB/s}$.
- Persistent connection pooling and HTTP/2 multiplexing should be implemented in future phases to achieve link-rate saturation.

---

## 5. Independent Scrubbing Bandwidth Trade-Off

- Coordinator independent verification streams the full byte content of each shard across the network to compute independent SHA-256 digests.
- While this guarantees immunity against fraudulent node self-reports, in multi-petabyte clusters it incurs linear network bandwidth consumption.
- Future optimizations should combine Merkle branch spot-checking with rate-limited periodic full audits.
