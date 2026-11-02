# FSP Phase 4.4: Baseline Architecture Audit & Security Pre-Flight Assessment

This audit establishes the empirical baseline of the **Forever Storage Protocol (FSP)** repository at the initiation of **Phase 4.4 (Physical Multi-Host Staging, Consensus Hardening & Real-World Fault Tolerance)**.

---

## 1. Environment & Toolchain Verification

The baseline audit was executed on the reference development host under sequential `--test-threads=1` verification:

- **Host OS:** Linux Mint 22.3 (Zena) / Ubuntu 24.04 noble LTS
- **Kernel:** `6.14.0-37-generic x86_64`
- **CPU Cores:** 24 physical execution cores
- **Total Physical RAM:** 62.7 GiB
- **Rust Toolchain:** `rustc 1.99.1 (58cf8f9c3 2026-10-13)` / `cargo 1.99.1`
- **Code Formatter:** `cargo fmt --all -- --check` (0 diffs, fully compliant)
- **Linter:** `cargo clippy --all-targets -- -D warnings` (0 warnings, fully clean)
- **Release Build:** `cargo build --release` (clean compilation, binary: `target/release/fsp-sim`)

---

## 2. Verified Test Suite Baseline (150/150 Passed)

Every individual test across the 11 test suites was verified sequentially (`--test-threads=1`):

| Test Suite | Path | Tests Passed | Status |
|:---|:---|:---:|:---:|
| `fsp_sim` Library Units | [`src/lib.rs`](file:///home/rade/Project/Jobs/fsp/src/lib.rs) | 2 | PASSED |
| Determinism Invariants | [`tests/determinism.rs`](file:///home/rade/Project/Jobs/fsp/tests/determinism.rs) | 3 | PASSED |
| Failure Scenarios | [`tests/failure_scenarios.rs`](file:///home/rade/Project/Jobs/fsp/tests/failure_scenarios.rs) | 6 | PASSED |
| Phase 1 Monte Carlo Durability | [`tests/phase1_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase1_validation.rs) | 16 | PASSED |
| Phase 2 Adaptive Repair v2 | [`tests/phase2_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase2_validation.rs) | 9 | PASSED |
| Phase 3 Persistent Storage Engine | [`tests/phase3_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase3_validation.rs) | 20 | PASSED |
| Phase 4 Distributed Cluster | [`tests/phase4_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase4_validation.rs) | 20 | PASSED |
| Phase 4.1 Durable Self-Healing | [`tests/phase4_1_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase4_1_validation.rs) | 25 | PASSED |
| Phase 4.2 Quorum Consensus & HA | [`tests/phase4_2_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase4_2_validation.rs) | 25 | PASSED |
| Phase 4.3 Consensus Safety & Formal Verification | [`tests/phase4_3_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase4_3_validation.rs) | 20 | PASSED |
| Recoverability Subsystem | [`tests/recoverability.rs`](file:///home/rade/Project/Jobs/fsp/tests/recoverability.rs) | 4 | PASSED |
| **Total Verified Baseline Tests** | | **150** | **100% PASSED** |

---

## 3. Structural Security Vulnerability Analysis (P0)

### Vulnerability SEC-P44-01: Stale Leadership Certificate Mutation on Uninformed Storage Node
- **Affected Components:**
  - [`src/coordinator/fencing.rs::NodeEpochFencingTracker`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L230)
  - [`src/coordinator/fencing.rs::FencedOperationToken`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L78)
- **Root Cause Analysis:**
  Phase 4.3 introduced [`LeadershipCertificate`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs#L187) and [`FencedOperationToken`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L78). The storage node validates:
  1. `token.epoch >= highest_seen_epoch`
  2. `token.verify(&leader_key)`
  
  **The Failure Scenario:**
  1. $C_1$ is elected leader in Term 1 and obtains a valid majority `LeadershipCertificate`.
  2. Storage node $N_1$ records `highest_seen_epoch = 1`.
  3. $C_1$ is partitioned from the metadata quorum.
  4. $\{C_2, C_3\}$ elect $C_2$ as the new leader for Term 2 with a new `LeadershipCertificate`.
  5. Storage node $N_1$ has not yet received Term 2 from $C_2$ (due to transient network delay or partition). $N_1$'s `highest_seen_epoch` is still 1.
  6. $C_1$ generates a `FencedOperationToken` for Term 1, signs it with its valid private key, and sends a mutation (`PutShard`, `DeleteShard`) to $N_1$.
  7. $N_1$ evaluates: `token.epoch (1) >= highest_seen_epoch (1)` $\to$ TRUE. `token.verify(C1_key)` $\to$ TRUE.
  8. **$N_1$ executes the mutation!**

- **Critical Insight:**
  A static cryptographic signature on a `LeadershipCertificate` proves that authorization was granted *at some point in the past*. It does not prove that the authorization is *currently valid* at the moment of mutation.

- **Required Remediation:**
  Mutations cannot rely solely on the leader's unilateral signature. Mutating operations must be backed by a **Quorum Mutation Permit** requiring current majority quorum authorization ($\ge 2$ of 3 voter signatures) and commit-index binding, coupled with durable object generation tracking on storage nodes.

---

## 4. Current Deployment Blockers & Operational Limitations

1. **Physical Multi-Host Deployment: NOT EXECUTED.** No bare-metal chassis or physical cluster orchestration is currently provisioned; multi-process localhost networks are used for all distributed testing.
2. **24-Hour Continuous Soak: NOT EXECUTED.** Continuous day-long soak tests remain unexecuted; bounded runs up to 15 minutes have been verified.
3. **Static Quorum Cluster:** Quorum membership is statically hard-coded to 3 nodes ($C_1, C_2, C_3$); dynamic joint-consensus re-configuration is not yet implemented.
