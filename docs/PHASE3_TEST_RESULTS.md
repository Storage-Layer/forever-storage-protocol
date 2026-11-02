# FSP Phase 3 — Verification, Failure-Injection & Test Results

## 1. Workspace Test Suite Summary (`58 / 58` Passed)

Executed via `cargo test --all-targets`:

| Test Suite | Target File | Tests Passed | Tests Failed | Scope |
|---|---|---|---|---|
| Phase 0 Determinism | [`tests/determinism.rs`](file:///home/rade/Project/Jobs/fsp/tests/determinism.rs) | `3` | `0` | Seed replay & trace divergence |
| Phase 0 Recoverability | [`tests/recoverability.rs`](file:///home/rade/Project/Jobs/fsp/tests/recoverability.rs) | `4` | `0` | Logical $(n, k)$ threshold & transient vs permanent loss |
| Phase 0 Failure Scenarios | [`tests/failure_scenarios.rs`](file:///home/rade/Project/Jobs/fsp/tests/failure_scenarios.rs) | `6` | `0` | Domain placement, repair queue & metric invariants |
| Phase 1 Monte Carlo Validation | [`tests/phase1_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase1_validation.rs) | `16` | `0` | Wilson CI, Kaplan–Meier/RMST, worker-count determinism, v1 falsification |
| Phase 2 Blind Detection & Adaptive v2 | [`tests/phase2_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase2_validation.rs) | `9` | `0` | Ground-truth isolation, risk bounds, 19/20 burst survival, LRC, Weibull, NIC contention |
| **Phase 3 Real Storage Engine** | [`tests/phase3_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase3_validation.rs) | **`20`** | **`0`** | Real $\text{GF}(2^8)$ $\text{RS}(20,12)$ & $\text{LRC}(22,12,4)$, FIPS 180-4 SHA-256, 6-state crash transactions, failure injection, subprocess kill recovery |
| **Total** | **6 Integration Targets** | **`58`** | **`0`** | **100% Pass Rate** |

---

## 2. Coverage of the 20 Required Phase 3 Test Categories

| Category | Test Function in [`tests/phase3_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase3_validation.rs) | Verification Details |
|---|---|---|
| 1. RS Round-Trip & NIST SHA-256 | `test_p3_01_rs_20_12_encoding_decoding_round_trip_and_nist_sha256_vectors` | Verifies 4 NIST FIPS 180-4 vectors (`""`, `"abc"`, 448-bit block, $10^6 \times \text{'a'}$) and systematic $\text{RS}(20, 12)$ round-trip (Scenario A). |
| 2. Arbitrary 12-of-20 Recovery | `test_p3_02_arbitrary_12_of_20_recovery_across_data_and_parity_subsets` | Verifies Scenarios B (1 deleted), C (4 deleted), and D (8 deleted, including 8 data shards lost + all 8 parity shards used). |
| 3. Exhaustive & Stratified Erasure Combinations | `test_p3_03_all_4845_four_shard_erasures_and_1000_stratified_eight_shard_erasures_of_rs_20_12` | Exhaustively verifies **all $\binom{20}{4} = 4,845$** 4-shard erasure combinations and **1,000 stratified 8-shard erasure combinations** (including extremal boundary subsets). |
| 4. Nine-Shard Unrecoverability | `test_p3_04_nine_shard_unrecoverability_rejected_safely` | Verifies Scenario E: 9 deleted shards ($11 < 12$ surviving) and duplicate `ShardId` inputs fail safely with `InsufficientValidShards` / `DuplicateShardId`. |
| 5. Corrupt & Truncated Shard Detection | `test_p3_05_corrupt_and_truncated_shard_detection_scenarios_f_and_g` | Verifies Scenarios F (1 shard byte-corrupted in place) and G (4 shards byte-corrupted + 2 shards truncated + 2 shards deleted = 8 damaged shards): corrupt shards are rejected before decoding, exact object is reconstructed from remaining 12 shards, and all 8 damaged shards are repaired and verified. |
| 6. Incorrect Object Digest & Tampered Manifest | `test_p3_06_incorrect_object_digest_and_tampered_manifest_rejection` | Verifies that unsealed manifest edits fail with `ManifestDigestMismatch` and forged `object_digest` fails with `ObjectIntegrityFailure`. |
| 7. Empty & Irregular File Sizes | `test_p3_07_empty_0b_1b_4kb_and_unaligned_prime_file_sizes` | Tests `0 B`, `1 B`, `7 B`, `63 B`, `64 B`, `65 B`, `4,096 B`, `4,097 B`, and `99,991 B` through full `PUT -> DELETE 8 SHARDS -> GET -> REPAIR -> VERIFY` lifecycle. |
| 8. Large-File Streaming | `test_p3_08_large_file_streaming_bounded_memory_verification` | Verifies 5-stripe (`4.5 MiB`) streaming `PUT` and degraded `GET` with 8 distinct shards deleted per stripe. |
| 9. Manifest Validation | `test_p3_09_manifest_pre_allocation_validation_rejects_malformed_and_dos_inputs` | Rejects unsupported `format_version`, `stripe_size > 64 MiB` DoS, duplicate `shard_id`, `..` relative paths, and malformed JSON before allocation. |
| 10. Atomic Commit Visibility | `test_p3_10_atomic_commit_visibility_and_uncommitted_isolation` | Verifies that crashes at `AfterPreparing`, `MidWriting`, `AfterVerifying`, and `MidCommittingBeforeManifest` never expose a partially uploaded object. |
| 11. Crash Recovery State Machine | `test_p3_11_crash_recovery_state_machine_all_points_and_idempotency` | Verifies rollback of partial shard files published during `MidCommittingBeforeManifest`, transition to `Aborted`, and strict multi-pass idempotency. |
| 12. Repair Idempotency & Interrupted Repair | `test_p3_12_repair_idempotency_and_interrupted_repair_scenario_h` | Verifies Scenario H (interrupted repair leaving `.repair.tmp`), cleanup via `recover()`, full repair, and 0-write idempotency on second `repair_object()` call. |
| 13. Concurrent Object Operations | `test_p3_13_concurrent_multi_thread_object_uploads_and_reads` | 8 OS threads concurrently executing `put_bytes` and `get_to_bytes` against a shared storage root. |
| 14. Filesystem Path Safety | `test_p3_14_filesystem_path_traversal_and_symlink_redirection_defense` | Verifies rejection of `../escape.txt`, `node-00/objects/../../escape.txt`, `/etc/passwd`, and Unix symbolic link redirection inside `node-00/objects/`. |
| 15. Disk-Full / Quota Exhaustion | `test_p3_15_disk_full_node_quota_exhaustion_rollback` | Enforces 100-byte quota on `node-05`, verifies `QuotaExceeded` error and clean transaction rollback with 0 orphan shard files. |
| 16. Multiple Object Isolation | `test_p3_16_multiple_object_isolation_and_independent_lifecycle` | Destroying 10 shards of `object-alpha` does not affect reconstruction or repair of `object-beta`. |
| 17. Stale & Conflicting Shard Versions | `test_p3_17_stale_and_conflicting_shard_versions_scenarios_i_and_j` | Verifies Scenarios I (`.v0.fsp` stale shard returned after node recovery) and J (`.v99.fsp` conflicting duplicate version file): both are detected, excluded from decoding, and cleaned up during `repair_object`. |
| 18. Adaptive Repair v2 Integration | `test_p3_18_adaptive_repair_v2_integration_with_real_storage_and_blind_detector` | Verifies `AdaptiveStorageRepairPlanner` + `FailureDetector`: immediately repairs confirmed corrupt shard on online Node 1 while deferring transiently unreachable shard on Suspect Node 0, then cancels in-flight repair when Node 0 recovers. |
| 19. Real Byte-Level LRC(22,12,4) & LRC(20,12,4) | `test_p3_19_lrc_22_12_4_and_lrc_20_12_4_real_byte_encoding_local_4shard_xor_repair_and_global_mr_decoding` | Verifies 4-shard XOR `LocalGroup` repair (`disk_bytes_read = 4 * shard_len`) for all 15 local-group shards and global MR decoding across 8 erasures. |
| 20. Subprocess Kill & Process Restart | `test_p3_20_subprocess_kill_crash_recovery_and_process_restart_persistence` | Spawns real child processes (`Command::new(CARGO_BIN_EXE_fsp-sim)`), kills child process with `std::process::exit(137)` at `MidCommittingBeforeManifest`, runs `storage recover` in a fresh process, repairs 8 deleted shards, and verifies byte-for-byte retrieval after process restart. |

---

## 3. Mandatory 12-Step 100 MiB Real Filesystem Integration Experiment

Recorded in [results/phase3/recovery_results.json](file:///home/rade/Project/Jobs/fsp/results/phase3/recovery_results.json):

1. **Deterministic 100 MiB File Generated:** `104,857,600` bytes (`100 MiB`).
2. **Original SHA-256 Digest:** `2258e1d173caaacbff15c68c2ff7fac974211631bd49f95c8f0628dfc5dcc17c`.
3. **Uploaded (`PUT`) to FSP Local Storage Backend ($\text{RS}(20, 12)$):** `100` stripes $\times 20$ shards = `2,000` shard files (`87,384` bytes each, `174,768,000` bytes total on disk).
4. **Verified All 20 Shards per Stripe:** `2,000 / 2,000` shard files verified healthy.
5. **Deleted 8 Distinct Shards per Stripe (`800` shard files deleted):** Shards `0, 1, 2, 3, 4, 12, 15, 19` deleted across all 100 stripes, leaving 12 shards per stripe (`1,200` shards).
6. **Reconstructed (`GET`) Original 100 MiB Object from 12 Surviving Shards:** Completed in `3.37 s` (`29.7 MiB/s`).
7. **Confirmed Reconstructed Digest:** `2258e1d173caaacbff15c68c2ff7fac974211631bd49f95c8f0628dfc5dcc17c` (**exact match**).
8. **Repaired (`REPAIR`) Deleted Shards:** Reconstructed and atomically published all `800` missing shard files (`104,860,800` bytes read, `69,907,200` bytes written).
9. **Verified Restored Shards:** `2,000 / 2,000` shards verified healthy on disk.
10. **Process Restart:** Dropped in-process engine and spawned a fresh OS child process (`./target/release/fsp-sim storage get lifecycle-100mb ...`).
11. **Retrieved Object After Restart:** `104,857,600` bytes written (`2,000` shards verified, `174,768,000` disk bytes read).
12. **Verified Byte-for-Byte Equality After Restart:** SHA-256 `2258e1d173caaacbff15c68c2ff7fac974211631bd49f95c8f0628dfc5dcc17c` and streaming 64 KiB chunk-by-chunk comparison confirmed **100% byte-for-byte identity**, followed by an additional 200-shard corruption/truncation repair cycle (`200` corrupted/truncated shards repaired and verified).
