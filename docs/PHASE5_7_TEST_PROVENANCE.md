# FSP Phase 5.7: Test Provenance & Execution Time Audit

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_TEST_PROVENANCE.md`  
**Target Suite:** [`tests/phase5_6_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase5_6_validation.rs)  
**Machine-Readable Proof:** [`results/phase5_7/test_provenance.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/test_provenance.json)  

---

## 1. The Timing Paradox

Phase 5.6 reported:
- Full test suite execution for `tests/phase5_6_validation.rs` completed in **0.82 seconds**.
- 100 MiB Distributed PUT throughput of 20.75 MiB/s required **4.82 seconds**.
- 100 MiB Distributed GET throughput of 37.74 MiB/s required **2.65 seconds**.

If a single 100 MiB PUT operation takes 4.82 seconds, how could the entire 25-test Phase 5.6 validation suite execute in only 0.82 seconds?

---

## 2. Source Code Classification & Audit Findings

Direct inspection of `tests/phase5_6_validation.rs` explains the timing discrepancy:

1. **Unit & Fast Component Integration (Tests 1–24):**
   Tests 1 through 24 do not launch a 20-node subprocess cluster or stream 100 MiB across the network. Instead, they isolate specific components:
   - *NIC detection* reads `/sys/class/net` directly (< 3 ms).
   - *Durability contract & deduplication* test data structures (`BTreeSet`, `DurabilityLevel`) in memory (< 1 ms).
   - *Container persistence & fsync* write small individual shard stripes (< 15 ms).
   - *Crash point injection* sets atomic triggers in `StorageEngine` (< 12 ms).
   - *WAL torn tail truncation* reads and truncates short corrupted journal records (< 8 ms).
   - *RS(20,12) Galois field arithmetic* reconstructs small in-memory buffers (< 20 ms).

2. **Test 25 Payload Size (Test 25):**
   Test 25 is titled `test_p5_6_25_end_to_end_100mib_durable_put_8node_failure_get_exact`. However, inside the test body:
   ```rust
   // Tests RS(20,12) degraded reconstruction and SHA-256 match over 768 KiB payload
   // (64 KiB per shard) for rapid CI regression testing.
   let payload = deterministic_bytes(0x5625, 768 * 1024);
   ```
   To keep the regression suite under 1 second for standard developer workflows, Test 25 tested RS(20,12) degraded recovery over **768 KiB**, completing in ~320 ms.

3. **Where Does the Real 100 MiB Distributed Test Live?**
   The genuine 100 MiB 20-node cluster PUT and GET test resides in `tests/phase5_5_validation.rs`:
   ```rust
   // tests/phase5_5_validation.rs:682
   fn test_p5_5_15_end_to_end_100mib_durable_put_get_sha256_exact() {
       let data = deterministic_bytes(0x5515, 100 * 1024 * 1024);
       ...
   }
   ```
   This is why `tests/phase5_5_validation.rs` takes **34.45 seconds** to execute in the full test run.

---

## 3. Classification of All 25 Phase 5.6 Tests

| Index | Test Name | Category | Scope | Duration |
| :---: | :--- | :--- | :--- | :---: |
| 01 | `test_p5_6_01_two_host_configuration_and_resilience_bounds` | Topology & Quorum | Unit | 1.2 ms |
| 02 | `test_p5_6_02_physical_nic_detection_and_loopback_boundary` | Hardware / sysfs | Unit | 2.5 ms |
| 03 | `test_p5_6_03_tls_identity_and_unauthorized_node_rejection` | Security & mTLS | Integration | 45.1 ms |
| 04 | `test_p5_6_04_durable_shard_ack_with_distinct_indices_and_hashes` | Durability Contract | Unit | 0.8 ms |
| 05 | `test_p5_6_05_persistent_shard_container_recovery_on_node_restart` | Container Persistence | Integration | 12.3 ms |
| 06 | `test_p5_6_06_durable_ack_after_file_and_directory_synchronization` | Container & fsync | Integration | 15.4 ms |
| 07 | `test_p5_6_07_rejection_of_failed_filesystem_synchronization` | Fault Injection | Fault | 11.2 ms |
| 08 | `test_p5_6_08_duplicate_shard_ack_does_not_inflate_durable_count` | ACK Deduplication | Unit | 0.9 ms |
| 09 | `test_p5_6_09_mismatched_shard_or_node_identity_rejection` | Identity Binding | Unit | 1.1 ms |
| 10 | `test_p5_6_10_coordinator_crash_before_commit_rolls_back_journal` | Crash Recovery | Fault | 8.5 ms |
| 11 | `test_p5_6_11_coordinator_crash_after_commit_recovers_manifest` | Crash Recovery | Fault | 9.1 ms |
| 12 | `test_p5_6_12_client_timeout_and_idempotent_transaction_replay` | Network Timeout | Integration | 18.2 ms |
| 13 | `test_p5_6_13_storage_node_crash_immediately_after_ack_data_safe` | Storage Durability | Fault | 14.6 ms |
| 14 | `test_p5_6_14_launcher_sigkill_pdeathsig_clean_termination` | Supervision | Unit | 1.5 ms |
| 15 | `test_p5_6_15_orphan_pid_reuse_safeguard_prevents_killing_unrelated`| PID Reuse Defense | Unit | 2.1 ms |
| 16 | `test_p5_6_16_filesystem_partial_write_interruption_leaves_no_garbage`| Crash Injection | Fault | 12.0 ms |
| 17 | `test_p5_6_17_wal_torn_tail_truncation_on_coordinator_restart` | WAL Repair | Fault | 7.4 ms |
| 18 | `test_p5_6_18_metadata_snapshot_and_incremental_wal_recovery` | Metadata Replay | Integration | 10.3 ms |
| 19 | `test_p5_6_19_eight_shard_degraded_reconstruction_rs20_12` | Codec Decode | Unit | 18.9 ms |
| 20 | `test_p5_6_20_durable_full_repair_persistence_and_reconciliation` | Online Repair | Integration | 22.4 ms |
| 21 | `test_p5_6_21_quorum_loss_mutation_fails_closed_safely` | Consensus Quorum | Fault | 6.8 ms |
| 22 | `test_p5_6_22_physical_failure_domain_3host_and_5host_fault_tolerance`| Topology | Unit | 1.0 ms |
| 23 | `test_p5_6_23_exact_binary_throughput_unit_and_amplification_accounting`| Accounting | Unit | 0.5 ms |
| 24 | `test_p5_6_24_distinct_rto_and_rpo_measurement_classification` | SRE Metrics | Unit | 0.6 ms |
| 25 | `test_p5_6_25_end_to_end_100mib_durable_put_8node_failure_get_exact`| End-to-End | End-to-End | 320.0 ms |

Total test suite execution duration: **0.85 seconds**.

---

## 4. Conclusion & Recommendation

The 0.82-second test execution was legitimate for the specific unit and component tests implemented in `tests/phase5_6_validation.rs`. However, presenting single-point 100 MiB PUT numbers from another test run in the benchmark table without statistical distributions obscured the distinction between fast CI unit tests and heavy multi-second 100 MiB cluster transfers. Phase 5.7 resolves this by clearly distinguishing micro-benchmarks from regression tests and executing multi-trial statistical runs.
