# FSP Phase 4 — Mandatory Repository & Security Baseline Audit

## 1. Executive Summary & Baseline Verification Commands

Prior to implementing any Phase 4 storage, cryptographic, or networking changes, the entire FSP repository (`/home/rade/Project/Jobs/fsp`) covering Phase 0 (Deterministic Simulation), Phase 1 (Monte Carlo Reliability & Adaptive Repair v1), Phase 2 (Blind Detection, Risk Estimation, Adaptive Repair v2, and LRC Research), and Phase 3 (Persistent Storage Engine, Byte-Level Erasure Coding, and 6-State Crash Recovery) was audited and verified.

### 1.1 Environment & Toolchain Metadata

| Property | Recorded Value |
|---|---|
| **Project Directory** | `/home/rade/Project/Jobs/fsp` |
| **Git Status** | Uninitialized working tree (`no-git-commit`; all Phase 0–3 artifacts preserved intact on disk) |
| **Rust Compiler** | `rustc 1.99.1 (58cf8f9c3 2026-10-13)` |
| **Cargo** | `cargo 1.99.1 (5f94df478 2026-08-27)` |
| **OS / Kernel** | `Linux 6.14.0-37-generic x86_64 GNU/Linux` |
| **CPU / RAM** | 24 hardware threads (`x86_64`), 62 GiB RAM |
| **Filesystem** | `ext4` on `/dev/sdb3` (439G total, 283G available) |

### 1.2 Mandatory Baseline Verification Command Results

All four mandatory baseline verification commands were executed on the unmodified Phase 3 repository:

```text
$ cargo fmt --all -- --check
Exit code: 0 (0 formatting violations)

$ cargo clippy --all-targets -- -D warnings
Exit code: 0 (0 warnings, 0 errors across lib, bin, and 6 integration test targets)

$ cargo test --all-targets
Exit code: 0
  - tests/determinism.rs:        3 passed; 0 failed
  - tests/recoverability.rs:     4 passed; 0 failed
  - tests/failure_scenarios.rs:  6 passed; 0 failed
  - tests/phase1_validation.rs: 16 passed; 0 failed
  - tests/phase2_validation.rs:  9 passed; 0 failed
  - tests/phase3_validation.rs: 20 passed; 0 failed
  Total baseline tests:         58 passed; 0 failed

$ cargo build --release
Exit code: 0 (target/release/fsp-sim built cleanly)
```

### 1.3 Independent Verification of Prior Phase 1–3 Artifacts

All historical artifacts and reported claims were cross-checked against on-disk JSON outputs:
- **Phase 1 (`results/phase1/summary.json`, `results/phase1/raw_results.jsonl`):** Verified 52 experiment batches totaling `232,000` trajectories (`225.11s` runtime, `10,692 KB` peak RSS).
- **Phase 2 (`results/phase2/summary.json`, `results/phase2/comparison.json`):** Verified 53 experiment batches + 3 paired 10,000-sample comparisons totaling `137,000` trajectories (`993.63s` runtime, `15,108 KB` peak RSS).
- **Phase 3 (`results/phase3/benchmark_summary.json`, `results/phase3/failure_matrix_results.json`):** Verified reported `RS(20,12)` numbers (`99.22 MiB/s` in-memory encode, `111.13 MiB/s` in-memory degraded decode, `7.57 MiB/s` persistent crash-safe PUT, `29.67 MiB/s` degraded GET, and `11,016 KB` = `10.76 MiB` peak RSS across the 100 MiB lifecycle). All historical files in `results/phase1/`, `results/phase2/`, and `results/phase3/` are preserved untouched.

---

## 2. Deep Technical & Security Audit Findings

### 2.1 Audit Area 1: Reed–Solomon and LRC Generator-Matrix Rank Properties

We conducted an exhaustive algebraic rank audit over $\text{GF}(2^8)$ (irreducible polynomial $x^8 + x^4 + x^3 + x^2 + 1$, `0x11d`) comparing the theoretical recoverability claims in `src/coding/mod.rs` against the actual generator matrices constructed in `src/coding/reed_solomon.rs` and `src/coding/lrc.rs`:

1. **`ReedSolomonCodec::fsp_default_20_12()` (`RS(20, 12)` in `src/coding/reed_solomon.rs`):**
   - **Construction:** Top $12 \times 12$ rows form $I_{12}$; bottom $8 \times 12$ rows form a Cauchy matrix $C_{p, j} = \frac{1}{x_p \oplus y_j}$ with $x_p = p \in \{0, \dots, 7\}$ and $y_j = 8 + j \in \{8, \dots, 19\}$.
   - **Exhaustive Rank Result:** Because all $20$ evaluation points $X \cup Y$ are pairwise distinct elements of $\text{GF}(2^8)$, Cauchy's determinant theorem guarantees non-singularity of every square minor. Exhaustive evaluation of all $\binom{20}{12} = 125,970$ $12 \times 12$ row-submatrices confirmed **`125,970 / 125,970` (`100.0000%`) have rank $12$** (`0` singular submatrices). Every 8-shard erasure pattern is unconditionally recoverable.

2. **`LrcCodec` (`LRC(20, 12, 4)` and `LRC(22, 12, 4)` in `src/coding/lrc.rs` vs `src/coding/mod.rs`):**
   - **Algebraic Discrepancy Identified:** In `src/coding/mod.rs`, `LrcConfig::is_pattern_decodable` checks the **information-theoretic Maximally Recoverable (MR) combinatorial condition**:
     $$\sum_{j=0}^{g-1} \max(0, e_j - 1) + e_{\text{global}} \le r_g$$
     However, in coding theory (Gopalan et al., 2012; Guruswami et al., 2019), constructing a *strictly Maximally Recoverable (MR)* LRC for $(n=22, k=12, l=4, r_g=7)$ where 100% of combinatorially admissible patterns are simultaneously non-singular requires a finite field larger than $\text{GF}(2^8)$ ($q = 256$). Over $\text{GF}(2^8)$, Phase 3's twisted Cauchy global parity construction (`src/coding/lrc.rs` lines 74–86) exhibits a small fraction of singular submatrices when local XOR parities eliminate one erased data shard per group (producing pairwise XOR difference columns $G_{p, a} \oplus G_{p, b}$):
     - **`LRC(20, 12, 4)` ($r_g = 5, d_{\min} = 7$):** Across all `244,420` combinatorially MR-admissible erasure patterns ($0 \dots 8$ erasures), **`243,980 / 244,420` (`99.8200%`)** have full rank $12$ over $\text{GF}(2^8)$, while `440` patterns (`0.1800%`) have rank $< 12$:
       - $0 \dots 5$ erasures: **`0` singular patterns** (`100.0000%` full rank).
       - $6$ erasures ($d_{\min} - 1 = 6$, total $\binom{20}{14} = 38,760$ patterns): **`1` singular pattern** (`38,759 / 38,760 = 99.9974%` full rank).
       - $7$ erasures (MR-admissible subset): `55` singular patterns.
       - $8$ erasures (MR-admissible subset): `384` singular patterns.
     - **`LRC(22, 12, 4)` ($r_g = 7, d_{\min} = 9$):** Across all `1,685,630` combinatorially MR-admissible erasure patterns ($0 \dots 10$ erasures), **`1,682,999 / 1,685,630` (`99.8439%`)** have full rank $12$ over $\text{GF}(2^8)$, while `2,631` patterns (`0.1561%`) have rank $< 12$:
       - $0 \dots 7$ erasures (including all $\binom{22}{15} = 170,544$ 7-erasure patterns): **`0` singular patterns** (`100.0000%` full rank).
       - $8$ erasures ($d_{\min} - 1 = 8$, total $\binom{22}{14} = 319,770$ patterns): **`8` singular patterns** (`319,762 / 319,770 = 99.9975%` full rank).
       - $9$ erasures (MR-admissible subset): `294` singular patterns.
       - $10$ erasures (MR-admissible subset): `2,329` singular patterns.
   - **Code Bug Identified & Remediation Plan:**
     - While `LrcCodec::select_independent_basis` in `src/coding/lrc.rs` explicitly verifies `sub.rank() == candidate_rows.len()` before decoding (so it never silently returns corrupt data on a singular pattern), `inspect_object_health` (`src/recovery/repair.rs:125`) and `execute_object_repair` (`src/recovery/repair.rs:343`) called `scheme.is_decodable(&healthy_set)` (the combinatorial check) rather than checking the actual $\text{GF}(2^8)$ generator-matrix rank!
     - **Phase 4 P0 Fix:** Add `ErasureCodec::can_decode(&self, surviving_shards: &BTreeSet<ShardId>) -> bool` (which checks the actual $\text{GF}(2^8)$ generator submatrix rank `== k`) and use it in health inspection and repair planning, while also optimizing the `LrcCodec` global parity coefficients so that `LRC(20, 12, 4)` achieves `0` singular 6-erasure patterns (`38,760 / 38,760 = 100%` at $d_{\min}-1$).

### 2.2 Audit Area 2: Custom SHA-256 vs. Independent Standard Implementation

- In Phase 3 (`src/storage/integrity.rs`), SHA-256 was implemented from scratch following NIST FIPS 180-4 because no external crates were declared in `Cargo.toml`.
- **Differential Verification:** We added the standard RustCrypto `sha2 = "=0.10.9"` crate and differentially tested `src/storage/integrity.rs` against `sha2::Sha256` and `/usr/bin/sha256sum` across:
  - NIST FIPS 180-4 vectors (`""`, `"abc"`, `"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"`),
  - Exact padding boundary lengths (`0`, `1`, `55`, `56`, `57`, `63`, `64`, `65`, `119`, `120`, `128` bytes),
  - Chunked streaming splits across prime step sizes (`1`, `7`, `64`, `4096`, `65537` bytes),
  - 1,000 randomized payloads up to `1 MiB`.
- **Result:** 100% bit-for-bit agreement between `src/storage/integrity.rs`, `sha2::Sha256`, and `/usr/bin/sha256sum`.

### 2.3 Audit Area 3: Filesystem Path Checks, Symlink Races, and TOCTOU Vulnerabilities

- **Current Behavior (`src/storage/filesystem.rs`):**
  - `validate_safe_identifier` and `validate_safe_relative_path` strictly whitelist `[A-Za-z0-9_.-]` and reject `..`, leading `/`, `\`, and NUL bytes.
  - `resolve_safe_relative_path` walks each component and calls `ensure_not_symlink` (`fs::symlink_metadata`).
- **Identified Vulnerability (TOCTOU Window):**
  - `ensure_not_symlink(&path)` followed by `File::create(&path)` or `File::open(&path)` is a two-step check-then-use sequence. If a concurrent local process with write permissions inside the storage root replaces a file or intermediate directory with a symlink between `symlink_metadata()` and `open()`, `File::create` / `File::open` would follow the symlink.
- **Phase 4 Remediation:**
  - Harden Unix file opens/writes in `src/storage/filesystem.rs` and `src/storage/container.rs` using `std::os::unix::fs::OpenOptionsExt::custom_flags(libc::O_NOFOLLOW)` (`0o400000` on Linux `x86_64`) so the kernel atomically rejects symbolic links at the `openat(2)` syscall boundary (`ELOOP`), and enforce `0o600` / `0o700` permissions on sensitive identity/key files.

### 2.4 Audit Area 4: Interrupted Transaction Recovery & Crash Durability

- **Current Behavior (`src/storage/transaction.rs` & `src/recovery/recovery_manager.rs`):**
  - Phase 3 implements a 6-state journal (`Preparing -> Writing -> Verifying -> Committing -> Committed | Aborted`) where `<object_id>.manifest.json` atomic rename into `metadata/manifests/` is the single commit point.
- **Identified Durability Gaps:**
  1. **Un-fsynced Newly Created Parent Directory (`src/storage/transaction.rs:356`):** During `Committing`, when `fs::create_dir_all(parent)` creates `<root>/node-XX/objects/<object_id>` for the first time, `sync_directory(parent)` syncs `<object_id>/` but does **not** sync `<root>/node-XX/objects/`. Across a physical machine power loss (`ext4`), the directory entry for `<object_id>` inside `objects/` is only guaranteed durable if `objects/` is also `fsync`ed.
  2. **Un-fsynced Parent Directory During Rollback (`src/recovery/recovery_manager.rs:115`):** When rolling back an interrupted `Committing` transaction, `fs::remove_file(&full)` unlinks partial shard files without `fsync`ing their parent directory before marking the transaction `Aborted`.
  3. **Process Crash vs. Machine Power Loss:** Subprocess `std::process::exit(137)` (`SIGKILL`-equivalent) validates process termination safety (userspace buffers lost, OS kernel page cache preserved), whereas true power-loss safety requires `fsync` on every modified file and every parent directory where an entry is created, renamed, or unlinked.
- **Phase 4 Remediation:** Add `sync_parent_chain` so creating `<object_id>` directories and unlinking rolled-back shard files explicitly `fsync`s the parent directories on disk.

### 2.5 Audit Area 5: Same-Object Concurrency & Race Conditions

- **Identified Critical Vulnerability in Phase 3:**
  - `StorageEngine` in Phase 3 has **no per-object concurrency control**.
  - **Race 1 (Concurrent `PUT` + `PUT` on same `object_id`):** Two concurrent `PUT` calls for the same `object_id` write to distinct staging directories (`metadata/staging/tx-1` and `tx-2`), but during `Committing` both move files to the exact same destination paths (`node-XX/objects/<object_id>/...`). If one transaction aborts or crashes during `Committing`, its rollback deletes the target shard paths, destroying the concurrent transaction's committed shards.
  - **Race 2 (Concurrent `REPAIR` + `REPAIR` or `PUT` + `REPAIR`):** `execute_object_repair` (`src/recovery/repair.rs:411`) writes to a deterministic temporary path `target_full.with_extension("fsp.repair.tmp")` without a unique repair operation nonce or per-object lock. Two concurrent repairs on the same object overwrite and unlink each other's `.fsp.repair.tmp` files mid-verification, causing spurious `ShardIntegrityFailure` or `ENOENT` errors.
- **Phase 4 Remediation:**
  - Implement a two-layer **Per-Object Concurrency Guard** (`ObjectLockManager`):
    1. In-process per-`object_id` lock table (`Mutex<BTreeMap<String, Arc<RwLock<()>>>>`) supporting shared reader locks (`GET`, `VERIFY`) or exclusive writer/repair locks (`PUT`, `REPAIR`, `DELETE`), and
    2. Cross-process filesystem lock files (`metadata/locks/<object_id>.lock` opened with `O_CREAT | O_EXCL` or advisory flock + unique per-operation `.tmp.<op_id>` staging names) so concurrent mutations never clobber temporary or committed files.

---

## 3. Dependency Audit for Phase 4

To support Ed25519 manifest signatures, mutual TLS 1.3 (`mTLS`), and differential SHA-256 verification without custom cryptographic inventions, the following audited crates from the local Cargo cache were verified and locked in `Cargo.toml`:

| Crate | Locked Version | Purpose in Phase 4 |
|---|---|---|
| `ed25519-dalek` | `=2.2.0` | RFC 8032 Ed25519 manifest signing, keypair generation, and strict signature verification |
| `sha2` | `=0.10.9` | Independent RustCrypto SHA-256 implementation for differential verification |
| `rustls` | `=0.23.45` | TLS 1.3 client & server state machines with mutual certificate authentication (`mTLS`) |
| `rustls-pki-types` | `=1.15.1` | X.509 certificate and private key DER types for `rustls` |
| `rustls-pemfile` | `=2.2.0` | PEM parsing for node, coordinator, and CA certificates/keys |
| `ring` | `=0.17.14` | Cryptographic provider for `rustls` TLS 1.3 cipher suites and X.509 signature verification |

---

## 4. Additional Defect Found During Phase 4 Implementation

- **LRC mutual recursion (stack overflow):** `LrcCodec::repair_single_shard` fell back to `reconstruct_specific_shards(&[missing], ...)`, whose single-target fast path called `repair_single_shard` again. Whenever a single shard had to be repaired from a local group with >= 2 erasures, this recursed infinitely. Found by `test_p4_14` (distributed LRC repair). Fixed by splitting out `reconstruct_specific_shards_global`, which the fallback now calls directly. Phase 3 tests did not exercise this path.
