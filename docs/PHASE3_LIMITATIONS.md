# FSP Phase 3 — Limitations, Risk Assessment & Phase 4 Recommendations

## 1. Epistemic Classification of Phase 3 Capabilities

| Capability / Claim | Classification | Evidence Source |
|---|---|---|
| Byte-accurate systematic Cauchy $\text{RS}(20, 12)$ encoding and decoding over $\text{GF}(2^8)$ (`0x11d`) recovers any 12-of-20 shard subset | **Analytical Guarantee + Measured Byte Verification** | Super-regularity of Cauchy matrices + exhaustive 4,845 4-erasure & 1,000 stratified 8-erasure tests ([`tests/phase3_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase3_validation.rs#L198-L263)) |
| Byte-accurate $\text{LRC}(22, 12, 4)$ and $\text{LRC}(20, 12, 4)$ perform 4-shard XOR local single-shard repair (`-66.7%` repair read bytes) | **Measured Byte Verification** | `test_p3_19` ([`tests/phase3_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase3_validation.rs#L948-L1012)) and `results/phase3/benchmark_results.json` (`34.95 MB` vs `104.86 MB` read on `100 MiB` repair) |
| Streaming `100 MiB` `PUT -> VERIFY -> DELETE 800 SHARDS -> GET -> REPAIR -> VERIFY -> SUBPROCESS RESTART GET` reproduces exact SHA-256 and bytes with `10.76 MiB` Peak RSS | **Real Byte-Level & Process-Restart Measurement** | [results/phase3/recovery_results.json](file:///home/rade/Project/Jobs/fsp/results/phase3/recovery_results.json) & `test_p3_20` |
| Uncommitted transactions killed via `std::process::exit(137)` at `MidCommittingBeforeManifest` are rolled back cleanly on restart | **Actual Process-Kill Test** | `test_p3_20_subprocess_kill_crash_recovery_and_process_restart_persistence` ([`tests/phase3_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase3_validation.rs#L1015-L1121)) |
| Power-loss durability across hardware reset | **OS / Hardware Assumption (Unverified in Software)** | Depends on kernel `fsync` + storage controller honoring `FUA` without volatile write-cache spoofing |
| Multi-host network transport, Byzantine peer authentication, and cross-node distributed consensus/leases | **Unverified / Out of Scope for Phase 3** | Phase 4 scope |

---

## 2. Remaining Security, Reliability & Performance Limitations

### 2.1 Single-Filesystem Failure Domain Limitation
In Phase 3, `node-00` through `node-19` are subdirectories under a single host filesystem root (`.fsp-dev/`).
- **Risk:** A physical disk crash, filesystem metadata corruption (`ext4` journal failure), or host OS compromise destroys all 20 node directories simultaneously.
- **Mitigation in Phase 4:** Map each `NodeId` to an independent network storage daemon (`gRPC` / `QUIC` over TLS) backed by dedicated physical storage hardware in distinct racks/ASNs.

### 2.2 Per-Shard `fsync` Barrier Overhead on Multi-Stripe Objects
During a `100 MiB` `PUT` (`100` stripes $\times 20$ shards = `2,000` shard files), `WriteTransaction` (`src/storage/transaction.rs`) writes and `sync_all()`s `2,000` individual staging files, re-reads them in `Verifying`, and renames them into `20` node directories.
- **Measured Effect:** While in-memory $\text{RS}(20, 12)$ encoding runs at **`99.22 MiB/s`**, end-to-end crash-safe `PUT` on a single local disk runs at **`7.57 MiB/s`** (`13.21 s` for `100 MiB`), dominated by `2,000` sequential `fsync` calls and pre-commit verification reads.
- **Mitigation:**
  1. Pack all stripes of a shard `(object_id, shard_id)` into a single contiguous per-shard container file (`shard-YY.v1.fsp` with an internal stripe offset table), reducing `2,000` files and `2,000` `fsync` barriers to **`20` files and `20` `fsync` barriers** regardless of object size!
  2. Parallelize shard writes across the $n = 20$ independent storage nodes in Phase 4.

### 2.3 Unkeyed SHA-256 Manifest Trust Boundary
While `ObjectManifest` verifies `manifest_digest` and every shard's `sha256_digest`, an attacker with write access to `<root>/metadata/manifests/` can replace both the manifest and shard files and recompute `manifest_digest`.
- **Mitigation in Phase 4:** Sign `manifest_digest` using an Ed25519 owner keypair (`manifest_signature`) and verify signatures prior to reconstruction or repair.

### 2.4 Coarse-Grained Concurrent Object Locking
Phase 3 isolates concurrent uploads of *different* objects via unique transaction staging directories (`tx-<pid>-<seq>`), and uses atomic `rename` for manifest publication. However, concurrent `repair_object` and `put_file` targeting the **exact same `object_id`** do not acquire an advisory file lock (`flock`) on `<object_id>.manifest.json`.
- **Mitigation in Phase 4:** Add per-object advisory file locks (`flock` / lease tokens) and monotonic manifest generation numbers (`manifest_generation: u64`).

---

## 3. Phase 4 Distributed-Node Recommendations

1. **Per-Shard Multi-Stripe Container Format:** Collapse `stripe-XXXXXX.shard-YY.v1.fsp` into a single `object-<id>.shard-YY.v1.fsp` file per node containing a stripe index header + contiguous stripe chunks, eliminating small-file explosion on large objects (`> 1 GiB`).
2. **Networked Node Daemons with mTLS & Challenge-Response Proofs:** Replace local `node-XX/` directory I/O with an asynchronous network transport where each storage node runs an independent daemon serving `PutShard`, `GetShard`, `VerifyShardChallenge`, and `Heartbeat`.
3. **Signed Manifests & Content-Addressed Root Verification:** Add Ed25519 signatures over `manifest_digest` and enforce content-addressed `object_id == sha256(object)` verification on `GET`.
4. **Background Scrubbing Daemon:** Schedule periodic background `verify_object` scans across stored objects so latent sector errors (bit-rot) on online disks are detected and fed into `AdaptiveStorageRepairPlanner` automatically.
