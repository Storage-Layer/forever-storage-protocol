# FSP Phase 3 — Baseline Repository Audit & Architectural Gap Analysis

## 1. Executive Summary

Before implementing the Phase 3 persistent storage engine, we inspected the entire `/home/rade/Project/Jobs/fsp` repository, verified all Phase 0, Phase 1, and Phase 2 documentation and experiment artifacts (`results/phase1/*` and `results/phase2/*`), and executed the mandatory baseline build and test suite.

### 1.1 Mandatory Baseline Verification Results
Executed on `2026-10-28`:

| Command | Exit Code | Output / Result |
|---|---|---|
| `cargo fmt --all -- --check` | `0` | Clean formatting across all source and test files. |
| `cargo clippy --all-targets -- -D warnings` | `0` | 0 warnings, 0 errors across library, binary, and 5 integration test targets. |
| `cargo test --all-targets` | `0` | **38 / 38 tests passed** (`3` in `tests/determinism.rs`, `6` in `tests/failure_scenarios.rs`, `16` in `tests/phase1_validation.rs`, `9` in `tests/phase2_validation.rs`, `4` in `tests/recoverability.rs`). |
| `cargo build --release` | `0` | Optimized `release` binary compiled cleanly. |

### 1.2 Independent Verification of Reported Phase 1 & Phase 2 Claims
- **38/38 passing tests:** Independently verified via `cargo test --all-targets`.
- **369,000 cumulative simulation trajectories:**
  - Phase 1 ([results/phase1/summary.json](file:///home/rade/Project/Jobs/fsp/results/phase1/summary.json)): 52 batches totaling `232,000` trajectories.
  - Phase 2 ([results/phase2/summary.json](file:///home/rade/Project/Jobs/fsp/results/phase2/summary.json) + [results/phase2/comparison.json](file:///home/rade/Project/Jobs/fsp/results/phase2/comparison.json)): 53 batches (`107,000` trajectories) + 3 paired 10k comparisons (`30,000` trajectories) = `137,000` trajectories.
  - Cumulative total: $232,000 + 137,000 = 369,000$ trajectories.
- **Adaptive v2 vs. Blind Fixed-Repair Baseline:** Verified in [results/phase2/comparison.json](file:///home/rade/Project/Jobs/fsp/results/phase2/comparison.json) line 5 (`StrategyC_FspAdaptiveV2` vs `StrategyB_FixedRepairSuspect_T18`, 20% churn, $N=10,000$ pairs): mean bandwidth reduction of `-1168.27 B/s` (`526.7 B/s` vs `1695.0 B/s`, `-68.9%`, 95% CI `[-1182.77, -1153.77]`), while loss probability difference was `-0.0200%` (95% CI `[-0.0592%, +0.0192%]`, `statistically_significant_95: false`).
- **Logical-Only Nature of Phase 0–2 Coding:** Verified in [src/coding/mod.rs](file:///home/rade/Project/Jobs/fsp/src/coding/mod.rs) and [src/model/shard.rs](file:///home/rade/Project/Jobs/fsp/src/model/shard.rs): Phase 0–2 represents shards strictly as logical metadata structs (`ShardReplica { shard_id, node_id, size_bytes, is_valid }`) without encoding, storing, hashing, or reconstructing actual payload bytes.

---

## 2. Inventory of Existing Modules & Reusability Assessment

| Module | Current Responsibility | Phase 3 Reusability & Adaptation Plan |
|---|---|---|
| [`src/model/shard.rs`](file:///home/rade/Project/Jobs/fsp/src/model/shard.rs), [`src/model/node.rs`](file:///home/rade/Project/Jobs/fsp/src/model/node.rs), [`src/model/failure_domain.rs`](file:///home/rade/Project/Jobs/fsp/src/model/failure_domain.rs) | Strongly-typed identifiers (`ShardId`, `NodeId`, `FailureDomainId`) and logical node/replica state | **Direct Reuse:** `ShardId`, `NodeId`, and `FailureDomainId` will be shared across both the discrete-event simulator and the real persistent storage engine. |
| [`src/coding/mod.rs`](file:///home/rade/Project/Jobs/fsp/src/coding/mod.rs) | Logical `CodingScheme` (`ReedSolomon { n, k }`, `Lrc(LrcConfig)`), Gopalan distance bounds, MR decodability check, and `RepairMethod` (`LocalGroup` vs `GlobalReconstruction`) | **Extend in Place:** Keep existing `LrcConfig`, `CodingScheme`, and `RepairMethod` intact for simulation compatibility, and add byte-level submodules: `coding::gf256`, `coding::reed_solomon`, `coding::lrc`, `coding::stripe`, and `coding::codec`. |
| [`src/detection/mod.rs`](file:///home/rade/Project/Jobs/fsp/src/detection/mod.rs) | Blind heartbeat failure detector (`ObservedNodeState`, `ClusterObservationView`, `DomainCorrelationSignal`) | **Direct Reuse:** Used by the Phase 3 Adaptive Repair v2 storage adapter (`src/recovery/repair.rs`) to feed observed node health into repair planning without ground-truth oracle access. |
| [`src/reliability/risk.rs`](file:///home/rade/Project/Jobs/fsp/src/reliability/risk.rs) & [`src/strategies/adaptive_v2.rs`](file:///home/rade/Project/Jobs/fsp/src/strategies/adaptive_v2.rs) | `estimate_object_risk` and `FspAdaptiveV2Policy::evaluate_blind` | **Direct Reuse:** `FspAdaptiveV2Policy::evaluate_blind` takes observable shard health, silence ages, Bayesian posteriors, domain concentration, and bandwidth budgets, and emits `BlindRepairDecision { shards_to_repair, repairs_to_cancel }`. |
| [`src/simulation/*`](file:///home/rade/Project/Jobs/fsp/src/simulation/mod.rs) | Discrete-event simulation engine, Monte Carlo runner, multi-object contention | **Preserve Unchanged:** Simulator remains 100% platform-independent and decoupled from filesystem I/O. |

---

## 3. Architectural Gap Analysis (Simulation vs. Real Storage Engine)

1. **Absence of Byte-Level Erasure Coding:**
   - Phase 0–2 checks `surviving_shards.len() >= k` (or `LrcConfig::is_pattern_decodable`), but performs no $\text{GF}(2^8)$ matrix multiplication or Gaussian/Gauss-Jordan matrix inversion.
   - **Phase 3 Solution:** Implement systematic Cauchy Reed–Solomon $\text{RS}(n, k)$ over $\text{GF}(2^8)$ (primitive polynomial $x^8 + x^4 + x^3 + x^2 + 1 = \text{0x11d}$) and systematic Maximally Recoverable $\text{LRC}(22, 12, 4)$ / $\text{LRC}(20, 12, 4)$ over $\text{GF}(2^8)$ with verified full-rank generator submatrices.
2. **Absence of Bounded-Memory Streaming Stripe Processing:**
   - Objects of $100\text{ MB}+$ cannot be buffered entirely in RAM if memory is bounded.
   - **Phase 3 Solution:** Process input streams (`std::io::Read`) and output streams (`std::io::Write`) one stripe at a time (default `stripe_size = 1,048,576` bytes = `1 MiB` of data per stripe, yielding $k=12$ data chunks of $\lceil \text{stripe\_size} / k \rceil = 87,384$ bytes each, rounded up to a multiple of 8 bytes). Peak working memory per stripe is bounded to $< 4\text{ MiB}$ regardless of total object size.
3. **Absence of Cryptographic Integrity & Canonical Manifests:**
   - Phase 0–2 uses a boolean flag `ShardReplica::is_valid`.
   - **Phase 3 Solution:** Implement FIPS 180-4 SHA-256 cryptographic hashing (`src/storage/integrity.rs`, verified against NIST test vectors) computing:
     - Full object SHA-256 digest (`object_digest`)
     - Per-stripe, per-shard SHA-256 digest (`shard_digests`)
     - Canonical JSON manifest with strict pre-allocation schema validation and digest verification.
4. **Absence of Crash-Safe Atomic Transactions:**
   - **Phase 3 Solution:** Implement an explicit 6-state transaction log and staging protocol (`Preparing -> Writing -> Verifying -> Committing -> Committed / Aborted` in `src/storage/transaction.rs`) using staging files (`.tmp.<tx_id>`), `File::sync_data()` / `File::sync_all()`, atomic `std::fs::rename` into immutable shard slots, parent directory `sync_all()`, and an atomic manifest commit point.

---

## 4. Dependency & Cryptographic / Algebraic Implementation Evaluation

Inspection of `Cargo.toml` and the local offline build environment shows that `fsp-sim` currently compiles with **zero external crate dependencies** (`[dependencies]` is empty), ensuring deterministic builds, zero supply-chain attack surface, and sub-second incremental compilation.

- **SHA-256 (`src/storage/integrity.rs`):** Implemented directly according to NIST FIPS PUB 180-4 (64-round compression function with standard initial hash values $H^{(0)}$ and round constants $K_0 \dots K_{63}$) and verified against official NIST FIPS 180-4 test vectors (empty string, `"abc"`, 448-bit multi-block message, and 1,000,000 `'a'` stream).
- **Galois Field $\text{GF}(2^8)$ & Cauchy Reed–Solomon / LRC (`src/coding/`):** Implemented directly over $\text{GF}(2^8) \cong \mathbb{F}_2[x] / (x^8 + x^4 + x^3 + x^2 + 1)$ using precomputed logarithm, anti-logarithm, multiplicative inverse, and $256 \times 256$ multiplication tables (`64 KiB`, L1/L2 cache-resident).
  - **Justification for Direct Implementation:** Standard external crates (such as `reed-solomon-erasure`) do not support Local Reconstruction Codes ($\text{LRC}(22, 12, 4)$) where local group XOR parities and global Cauchy/Vandermonde $\text{GF}(2^8)$ parities coexist in a unified generator matrix $G \in \text{GF}(2^8)^{n \times k}$. Implementing the unified $\text{GF}(2^8)$ linear code engine directly allows exact inspection of the generator matrix, explicit Gauss–Jordan submatrix inversion for both RS and LRC, and independent algebraic test-vector verification.
