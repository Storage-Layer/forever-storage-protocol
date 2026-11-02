# FSP Phase 5.8: Distributed Recovery and Shard Rebuilding

## 1. Reed-Solomon Cauchy Erasure Coding

FSP utilizes a systematic Cauchy Reed-Solomon codec over $\text{GF}(2^8)$:
- **Parameters:** $\text{RS}(n=20, k=12, m=8)$
- **Storage Expansion:** $\frac{20}{12} = 1.6667\times$ (66.7% redundancy overhead)
- **Erasure Tolerance:** Any 8 arbitrary shard erasures (data, parity, or mixed) can be tolerated simultaneously without data loss.
- **Single-Pass Inversion:** Cauchy generator sub-matrices guarantee non-zero determinants, ensuring an invertible decoding matrix $G_S^{-1}$ for every combination of $\binom{20}{12} = 125,970$ subsets.

---

## 2. 100 MiB Cross-Host Shard Recovery Validation

The codec was evaluated against full 100 MiB datasets under the maximum theoretical erasure threshold:
- **Test Case:** `test_p5_8_11_cross_host_100mib_rs20_12_exact_sha256_match`
- **Scenario:** 100 MiB payload partitioned into stripes; 8 consecutive shards completely erased (shards 0..7 erased, shards 8..19 available).
- **Result:** Decoded payload matches original data byte-for-byte; SHA-256 digests identical.

---

## 3. Storage Node Crash Recovery

When a storage node daemon starts or recovers from an abrupt termination (`init_or_open`):
1. **Uncommitted Staging Scrub:** All dangling files in `<storage_root>/staging/` and `<journal>/*.tmp` are deleted.
2. **Container Index Discovery:** Scans `<storage_root>/objects/` recursively. For each `.fspc` container, verifies container magic and reads container header metadata into memory.
3. **Idempotency Re-synchronization:** Reopens the append-only `idempotency.wal` to restore transaction idempotency cache.
4. **Fencing Tracker:** Reads durable highest-seen epoch to prevent replay of stale permits.
