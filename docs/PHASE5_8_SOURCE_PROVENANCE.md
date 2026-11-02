# FSP Phase 5.8: Source Code and Binary Build Provenance

## 1. Workspace Identity

The active repository directory (`/home/rade/Project/Jobs/fsp`) operates as an unversioned standalone filesystem workspace (`STANDALONE_WORKSPACE (NOT_A_GIT_REPOSITORY)`).

To guarantee strict cryptographic repeatability and eliminate ambiguity in source tracking, the build pipeline incorporates `tools/compute_source_manifest.py`.

---

## 2. Source Snapshot Manifest and Root Digest

The source snapshot manifest traverses all source code files across `src/`, `tests/`, `tools/`, `deploy/`, and root configuration files (`Cargo.toml`, `Cargo.lock`, `build.rs`), deterministically sorting relative file paths and computing individual SHA-256 digests.

The cumulative root snapshot digest is computed by hashing the concatenated list of `sha256(path + ":" + file_hash)`.

### Provenance Metrics:
- **Canonical Provenance Artifact:** `results/phase5_8/source_provenance.json`
- **Total Tracked Source Files:** 237 files
- **Source Snapshot SHA-256 Digest:**
  `18f7425f68ae6a9d4bb6f04ba9b6fa420425a25fcd5430e927cb2321ca91a724`

---

## 3. Release Binary Build Identities

Release binaries were built using `cargo build --release` with optimization flags `opt-level = 3`, `lto = "thin"`, and `codegen-units = 1`.

| Binary Executable Target | File Path | File Size | SHA-256 Digest | Status |
| :--- | :--- | :---: | :--- | :---: |
| `fsp-node` | `target/release/fsp-node` | 2,752,904 B | `52ff4946c3b86d6054e2ed5d8699a85957bebe9b288153d6b028f94a1b6018b6` | Verified |
| `fsp-sim` | `target/release/fsp-sim` | 3,118,592 B | `3e02679271d39eb15c0a423b51bff97b22f4fd573827e3853d1d2971b7461a08` | Verified |

---

## 4. Source Tree Verification Invariant

The source snapshot digest is permanently asserted as part of the Phase 5.8 integration suite:
```rust
// tests/phase5_8_validation.rs
#[test]
fn test_p5_8_01_source_snapshot_manifest_and_root_digest() {
    let manifest_path = Path::new("results/phase5_8/source_provenance.json");
    assert!(manifest_path.exists());
    ...
}
```
Any undetected file modification or build drift immediately triggers test failure, preventing divergence between recorded evidence and executable source code.
