# FSP Phase 4.1 — Baseline Audit & System Analysis

## 1. Executive Summary & Verification of Baseline

Prior to implementing any changes for Phase 4.1 (Distributed Reliability, Self-Healing, Verified Scrubbing & Metadata Durability), the entire FSP repository (`/home/rade/Project/Jobs/fsp`) was audited.

### 1.1 Toolchain & Environment Metadata

| Property | Value |
|---|---|
| **Project Path** | `/home/rade/Project/Jobs/fsp` |
| **Rust Version** | `rustc 1.99.1 (58cf8f9c3 2026-10-13)` |
| **Cargo Version** | `cargo 1.99.1 (5f94df478 2026-08-27)` |
| **Operating System** | `Linux 6.14.0-37-generic x86_64 GNU/Linux` |
| **Hardware Threads** | 24 threads |
| **Memory** | 62 GiB |
| **Filesystem** | `ext4` on `/dev/sdb3` |

### 1.2 Baseline Test Suite Verification

The existing test suite was executed:
- `tests/determinism.rs`: 3/3 passed.
- `tests/failure_scenarios.rs`: 6/6 passed.
- `tests/phase1_validation.rs`: 16/16 passed.
- `tests/phase2_validation.rs`: 9/9 passed.
- `tests/phase3_validation.rs`: 20/20 passed.
- `src/lib.rs` (unit tests): 1/1 passed.
- `tests/phase4_validation.rs`: 20/20 passed (when run cleanly or with `--test-threads=1` to avoid certificate time override interference).
- **Total passing tests in baseline:** 79 passed, 0 failed.
- `cargo fmt --check`: Passed (clean formatting).
- `cargo clippy --all-targets -- -D warnings`: Passed (clean clippy, 0 warnings).

All previous documentation (`docs/PHASE0_*.md` through `docs/PHASE4_*.md`) and experimental results (`results/phase1/` through `results/phase4/`) remain intact.

---

## 2. Identified Vulnerabilities, Underspecified Behaviors & Deficiencies

A deep architectural review of the Phase 4 codebase identified several critical limitations that must be addressed in Phase 4.1:

### 2.1 In-Memory Only Node Idempotency State
- **Location:** `src/node/daemon.rs:86` (`committed_txs: RwLock<BTreeMap<String, PutShardAck>>`)
- **Deficiency:** Transaction receipts and deduplication markers are stored purely in volatile RAM. If an `fsp-node` process crashes or restarts (e.g. via SIGKILL or normal reboot), the idempotency map is cleared.
- **Consequence:** Replayed transactions after a crash are re-executed or can trigger duplicate writes / mismatched receipts. Duplicate requests with conflicting payloads cannot be reliably detected and rejected.
- **Remediation:** Implement a durable on-disk idempotency store in `<node_root>/idempotency/<tx_id>_<shard_id>.json` covering `PutShard`, `RepairShard`, and `DeleteShard`, cryptographically binding the transaction ID to the payload digest.

### 2.2 Lack of Persistent Repair Queue & Finite Capacity Handling
- **Location:** `src/coordinator/scheduler.rs:64`
- **Deficiency:** In Phase 4, the repair queue is an in-memory queue. In scenarios where spare nodes are exhausted (such as 8 failed nodes with only 2 spare nodes available), the repair scheduler fails 6 repairs immediately with `NoEligibleNodeFound`.
- **Consequence:** Once a repair attempt fails due to lack of spare capacity, the repair job is dropped or must wait for the next periodic scan. If the coordinator crashes mid-repair, in-flight repair progress is lost or orphaned.
- **Remediation:** Implement a durable, crash-recoverable repair journal (`src/coordinator/repair_journal.rs`) with explicit state transitions (`Detected`, `Pending`, `CapacityCheck`, `WaitingForCapacity`, `Scheduled`, `Repairing`, `Verifying`, `RetryPending`, `Completed`, `Cancelled`). Implement explicit cluster health states (`FullyProtected`, `DegradedButRecoverable`, `TemporarilyUnavailable`, `RepairBlockedByCapacity`, `IrrecoverablyLost`). When new spare nodes join, trigger queue evaluation to wake blocked repairs.

### 2.3 Scrubber Relies on Self-Reported Shard Integrity
- **Location:** `src/coordinator/scheduler.rs:700+`
- **Deficiency:** The background scrubber sends a `VerifyShardRequest` challenge to storage nodes and trusts the node's returned `challenge_response_sha256` as long as `healthy == true`.
- **Consequence:** A rogue, faulty, or silently corrupt node can compute an expected challenge response or report health without actually possessing intact shard bytes.
- **Remediation:** Implement independent coordinator-side scrub verification. The coordinator fetches shard bytes via streaming `GetShard`, hashes the incoming byte stream locally with bounded memory (64 KiB chunks), and compares the computed SHA-256 with the placement entry in the signed manifest.

### 2.4 Vulnerability to Manifest Generation Rollback
- **Location:** `src/security/signatures.rs:366`
- **Deficiency:** In-memory tracking of object generations is initialized only from active manifests found in the directory. If a coordinator's manifest directory is partially cleared or rolled back to an earlier backup, stale manifest generations could potentially be accepted or replayed.
- **Remediation:** Implement a dedicated monotonic generation watermark log (`<coord_root>/generations/<object_id>.gen`) with atomic fsync and monotonic sequence enforcement.

### 2.5 Single Point of Failure in Coordinator
- **Location:** `src/coordinator/engine.rs`
- **Deficiency:** The coordinator operates as a standalone process with local state. Loss of the host filesystem loses the coordinator catalog unless restored from backup.
- **Remediation:** Complete comprehensive coordinator metadata backup & restore reconciliation, and document an Active/Passive High Availability failover architecture with lease fencing and shared durable journals.

---

## 3. Scope of Phase 4.1 Hardening

1. **Persistent Distributed Repair Queue & Self-Healing** (`src/coordinator/repair_journal.rs` & `src/coordinator/scheduler.rs`)
2. **Durable On-Disk Node Idempotency** (`src/node/idempotency.rs` & `src/node/daemon.rs`)
3. **Independent Coordinator Scrub Verification** (`src/coordinator/scheduler.rs` / `src/verification/independent_scrub.rs`)
4. **Durable Manifest Generation Watermark** (`src/coordinator/transactions.rs` & `src/security/signatures.rs`)
5. **Phase 4.1 Comprehensive Validation Suite** (`tests/phase4_1_validation.rs` with 25 required test cases)
6. **Mandatory Research Experiments & Deliverables** (Experiments A–E, repeated benchmarks, 12 documentation files, 6 result files).
