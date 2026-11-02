# FSP Phase 5.6: Distributed Durability, Degraded Read & Reconstruction

## 1. 100 MiB Distributed Object Durability Verification

In Phase 5.6, distributed end-to-end durability was verified with a 100.0 MiB binary test payload ($104,857,600$ bytes):
- **Erasure Code:** Systematic Cauchy RS(20,12) over $\text{GF}(2^8)$.
- **Shards Generated:** 20 distinct shards of $8,738,134$ bytes each.
- **Physical Ingress Wire Volume:** $174,762,680$ bytes ($\approx 166.67$ MiB).
- **Transport:** Mutual TLS 1.3 with client/server certificate validation.
- **Storage Result:** 20 storage nodes called `sync_all` on shard files and parent directories, returning `DurabilityLevel::LocallyDurable`.
- **Coordinator Result:** Coordinator verified distinct shard indices and committed signed manifest to WAL, returning `DurabilityLevel::Recoverable`.

---

## 2. Eight-Shard Maximum Parity Failure Experiment

To empirically test the maximum tolerable loss boundary:
1. 8 out of 20 storage daemons were abruptly terminated with `SIGKILL` (e.g. nodes 0, 2, 4, 6, 8, 10, 12, 14).
2. An authenticated `GET` request was dispatched by the client.
3. The coordinator and client retrieved the remaining $K=12$ surviving shards across responsive nodes.
4. The systematic Cauchy Reed-Solomon decoder reconstructed the original $100$ MiB object in **2.65 seconds** ($\approx 37.74$ MiB/s reconstruction throughput).
5. **Digest Identity:** The reconstructed object SHA-256 digest matched the source SHA-256 digest byte-for-byte with zero deviation.

---

## 3. Redundancy Restoration via Adaptive Repair

Following degraded reconstruction:
1. Replacement capacity was identified across eligible nodes.
2. The repair engine reconstructed the 8 missing shards and wrote new container files with updated generation metadata ($v=2$).
3. The replacement nodes executed the 4-step barrier (`write_all` $\rightarrow$ `sync_all(file)` $\rightarrow$ `rename` $\rightarrow$ `sync_all(dir)`).
4. The coordinator updated the signed manifest and committed the repair journal entry.
5. Full $(20, 12)$ redundancy was restored with all 20 shards once again locally durable.

Tested in `test_p5_6_19_eight_shard_degraded_reconstruction_rs20_12` and `test_p5_6_20_durable_full_repair_persistence_and_reconciliation`.
