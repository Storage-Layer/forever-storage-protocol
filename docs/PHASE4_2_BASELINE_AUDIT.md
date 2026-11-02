# FSP Phase 4.2: Comprehensive Baseline Audit & Provenance Verification

## 1. Executive Summary

This audit establishes the ground-truth baseline of the Forever Storage Protocol (FSP) repository before undertaking Phase 4.2 development. All findings are derived from direct inspection of source code, test execution output, and benchmark artifacts.

- **Current Toolchain:** `rustc 1.99.1 (stable)`, `cargo 1.99.1`, Linux x86_64 (`6.6.137+rpt-rpi-2712`), ext4 filesystem.
- **Compiler/Linter Verification:**
  - `cargo fmt --all -- --check`: **PASS** (zero formatting diffs)
  - `cargo clippy --all-targets -- -D warnings`: **PASS** (zero warnings)
  - `cargo test --all-targets -- --test-threads=1`: **PASS** (all 105 tests passing)
  - `cargo build --release`: **PASS** (clean release build)

---

## 2. Test Count Inconsistency Reconciliation

### The Discrepancy
The Phase 4.1 summary reported:
- `"total_tests_configured": 104`
- `"total_tests_passed": 104`

However, the individual breakdown within the same report was:
- `phase0_determinism`: 3
- `phase0_failure_scenarios`: 6
- `phase0_recoverability`: 4
- `phase1_validation`: 16
- `phase2_validation`: 9
- `phase3_validation`: 20
- `phase4_validation`: 20
- `phase4_1_validation`: 25
- `unit_tests_lib`: 2

### Ground Truth Reconciliation
Summing the individual suites:
$$3 + 6 + 4 + 16 + 9 + 20 + 20 + 25 + 2 = 105$$

Running `cargo test --all-targets -- --test-threads=1` executes the following test runners:
1. `unittests src/lib.rs`: **2 passed**
   - `test_placement_planner_enforces_domain_caps_and_rejects_unsatisfiable`
   - `test_durable_idempotency_store_lifecycle`
2. `tests/determinism.rs`: **3 passed**
3. `tests/failure_scenarios.rs`: **6 passed**
4. `tests/recoverability.rs`: **4 passed**
5. `tests/phase1_validation.rs`: **16 passed**
6. `tests/phase2_validation.rs`: **9 passed**
7. `tests/phase3_validation.rs`: **20 passed**
8. `tests/phase4_validation.rs`: **20 passed**
9. `tests/phase4_1_validation.rs`: **25 passed**
10. `bin/fsp_node.rs` & `main.rs`: 0 unit tests each.

**Total Actual Executed & Passing Tests:** **105 tests**.

**Conclusion:** No tests were duplicated, omitted, or skipped. The prior author made an off-by-one arithmetic error in the summary JSON and markdown headline (`104` instead of `105`), despite listing the correct per-target breakdown that sums to 105. The true baseline entering Phase 4.2 is **105 passing tests**.

---

## 3. Process Crash Testing Verification

The audit reviewed how process crashes were simulated in Phase 4 and Phase 4.1:

| Target Component | Test Reference | Crash Mechanism | Evaluation |
|:---|:---|:---|:---|
| Storage Node Daemon (`fsp-node`) | [`tests/phase4_validation.rs:713`](file:///home/rade/Project/Jobs/fsp/tests/phase4_validation.rs#L713-L860) (`test_p4_09`) | **Subprocess `child.kill()` (SIGKILL)** | Real OS process kill. Verifies kernel socket termination, abrupt un-fsynced temporary file cleanup, and journal recovery upon respawn. |
| Repair Journal | [`tests/phase4_1_validation.rs:306`](file:///home/rade/Project/Jobs/fsp/tests/phase4_1_validation.rs#L306-L342) (`test_p4_1_04`) | **In-process object destruction (`drop(journal)`)** | Not a true OS process kill. Memory is dropped cleanly by Rust RAII; no abrupt thread abort or socket drop. |
| Coordinator Metadata | [`tests/phase4_1_validation.rs:569`](file:///home/rade/Project/Jobs/fsp/tests/phase4_1_validation.rs#L569-L591) (`test_p4_1_11`) | **In-memory export & re-import** | Functional serialization/deserialization test; no process termination. |
| Coordinator Crash Exp D | `results/phase4_1/experiment_d_coordinator_crash.json` | **`immediate_process_drop` (`drop(coord)`)** | In-process instance drop. |

> [!WARNING]
> **Audit Finding: Coordinator SIGKILL was NOT tested in Phase 4.1**
> In Phase 4.1, coordinator crashes were simulated exclusively via in-process object drops (`drop(journal)`). True OS-level subprocess SIGKILL was tested only on the `fsp-node` daemon in Phase 4 (`test_p4_09`).
> In Phase 4.2, adversarial tests must execute real subprocess `SIGKILL` against active coordinator instances to verify kernel socket resets and failover under uncoordinated termination.

---

## 4. Benchmark Provenance Analysis

The Phase 4.1 benchmarks reported a median 8-shard repair duration of **1.34 seconds** and distributed throughput of **26.5 MiB/s**.

### Provenance Audit:
- **Source Artifact:** [`results/phase4_1/experiment_a_eight_shard_recovery.json`](file:///home/rade/Project/Jobs/fsp/results/phase4_1/experiment_a_eight_shard_recovery.json) & [`tests/phase4_1_validation.rs:501`](file:///home/rade/Project/Jobs/fsp/tests/phase4_1_validation.rs#L501-L545) (`test_p4_1_09`).
- **Object Size:** `32,768 bytes` (32 KiB).
- **Stripe Count:** 1 stripe.
- **Erasure Scheme:** RS(20,12) with $n=20, k=12, m=8$.
- **Shard Size:** $\approx 2,731$ bytes per shard.
- **Network Scope:** Localhost loopback (`127.0.0.1`), TLS 1.3 with fresh TCP handshake per RPC.
- **Transferred Bytes:**
  - Ingress: 12 shards $\times \approx 2.7\text{ KiB} \approx 32.8\text{ KiB}$.
  - Egress: 8 repaired shards $\times \approx 2.7\text{ KiB} \approx 21.8\text{ KiB}$.
  - Total network I/O: $\approx 54.6\text{ KiB}$.
- **Durability Operations:**
  - 8 individual container fsyncs on 8 spare storage nodes.
  - Multiple coordinator repair journal `.rec` fsync transitions (`Pending` $\to$ `CapacityCheck` $\to$ `Scheduled` $\to$ `Repairing` $\to$ `Verifying` $\to$ `Completed`).
  - 1 generation watermark fsync (`.gen`).

**Audit Finding:** The 1.34-second duration is dominated by filesystem synchronization (`fsync`) and unpooled TLS handshake overhead on a small 32 KiB payload, **not** data transfer bottleneck. Comparing this directly against 100 MiB or multi-megabyte transfers without standardizing payload and stripe dimensions would be invalid.

---

## 5. Architectural Deficiencies & Phase 4.2 Mandates

1. **Coordinator SPOF:** Single coordinator instance controls writes and repairs. No active-passive failover with durable epoch fencing exists.
2. **Missing Distributed Fencing:** Storage nodes accept `PutShard`, `RepairShard`, and `DeleteShard` without validating leadership epochs, risking split-brain writes under network partitions.
3. **No Dynamic Metadata Replication:** Standby coordinators cannot replicate or shadow active state without manual export/import bundles.
4. **Unvalidated Failure Domains:** Placement policy assigns racks round-robin without mathematical verification of host/rack/region failure tolerance.
5. **No Connection Pooling:** Every RPC opens a new TCP connection and TLS handshake, causing latency and thread/socket exhaustion.
6. **No Real Config Parsing:** Deployment configuration files were decorative documentation rather than strictly parsed TOML structures.
