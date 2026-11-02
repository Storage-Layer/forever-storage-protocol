# FSP Phase 5.7: Distributed Recovery & Eight-Shard Degraded Reconstruction

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_DISTRIBUTED_RECOVERY.md`  
**Machine-Readable Proof:** [`results/phase5_7/distributed_recovery.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/distributed_recovery.json)  
**Tests:** `tests/phase5_6_validation.rs` (Tests 13, 19, 25)  

---

## 1. The RS(20,12) Erasure Coding Guarantee

The Forever Storage Protocol configures Reed-Solomon erasure coding with parameters:
$$N = 20 \text{ total shards}, \quad K = 12 \text{ data shards}, \quad M = N - K = 8 \text{ parity shards}$$

The fundamental theorem of Maximum Distance Separable (MDS) codes guarantees that an original object stripe can be completely and bit-for-bit reconstructed from **any subset of $K=12$ surviving shards**, tolerating the simultaneous destruction or absence of up to **$M=8$ shards**.

---

## 2. Eight-Shard Degraded Reconstruction Verification

Degraded reconstruction was evaluated at both the isolated codec level and across the active distributed cluster:

### Codec-Level Reconstruction (Test 19)
- **Test:** `test_p5_6_19_eight_shard_degraded_reconstruction_rs20_12`
- **Method:** An object is striped and encoded into 20 shards ($D_0 \dots D_{11}, P_0 \dots P_7$). Shards $D_0 \dots D_7$ (8 data shards) are completely removed.
- **Decoding:** The decoder receives only $D_8 \dots D_{11}$ (4 data shards) and $P_0 \dots P_7$ (8 parity shards) — exactly 12 surviving shards.
- **Algorithm:** Inverts the corresponding $12 \times 12$ submatrix in Galois Field $GF(2^8)$ using Gaussian elimination.
- **Outcome:** All 8 missing data shards are reconstructed. Reassembled payload matches original bytes with **0 bit divergence**.

### Cluster Cold-Restart Durability (Test 13)
- **Test:** `test_p5_6_13_storage_node_crash_immediately_after_ack_data_safe`
- **Method:** A storage node receives a shard stripe, writes to container, executes `sync_all`, sends durable ACK, and is immediately killed by SIGKILL.
- **Outcome:** The node is restarted cold. It opens the persistent `.container` file, parses the header and stripe index table, and validates the shard payload SHA-256 digest against the manifest.
- **Verdict:** PASS. Committed shards survive process destruction.

### Full 20-Node Degraded Cluster GET (Test 25)
- **Test:** `test_p5_6_25_end_to_end_100mib_durable_put_8node_failure_get_exact`
- **Method:** An object is written across 20 live nodes. During client retrieval, 8 storage nodes are forcefully terminated.
- **Outcome:** Coordinator detects 8 failed connections, requests surviving shards from the remaining 12 nodes, invokes `ReedSolomonCodec::decode_data_shards`, and streams reconstructed bytes to client.
- **Digest Verification:** Client validates final SHA-256 hash. Matches original payload digest bit-for-bit.

---

## 3. Multi-Trial Re-measurement Throughput

In the Phase 5.7 benchmark suite (`fsp-sim --phase5-bench`), 100 MiB degraded 8-shard reconstruction was measured across 5 independent trials:
- **Median Duration:** **0.9254 seconds**
- **Median Throughput:** **108.06 MiB/s**
- **StdDev:** **$\pm 2.01$ MiB/s**

This confirms that CPU-bound degraded reconstruction in optimized Rust exceeds 100 MiB/s, proving that reconstruction does not bottleneck retrieval pipelines.
