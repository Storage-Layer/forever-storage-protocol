# FSP Phase 3 — Real Reconstruction, Repair & Adaptive v2 Integration Design

## 1. Verifying Object Reconstruction (`src/recovery/reconstruct.rs`)

Object reconstruction (`GET` / `RECONSTRUCT`) operates stripe-by-stripe with bounded memory:

1. **Manifest Loading & Pre-Allocation Check:**
   Reads `<root>/metadata/manifests/<object_id>.manifest.json` and runs `ObjectManifest::validate()`, verifying `manifest_digest` and all structural bounds.
2. **Per-Stripe Shard Probing & Version Resolution:**
   For each stripe $s \in \{0, \dots, N_{\text{stripes}} - 1\}$:
   - Iterates through all $n$ `ShardPlacementRecord`s.
   - Skips any node currently marked offline/unavailable in the storage backend.
   - Resolves shard file candidates in `node-XX/objects/<object_id>/`:
     - If stale versions (e.g., `.v0.fsp` alongside `.v1.fsp`) or conflicting duplicate files exist (Scenarios I & J), only the shard file matching the manifest's `version`, exact `size_bytes`, and exact `sha256_digest` is accepted as `HealthyVerified`.
     - Any truncated, byte-corrupted, or mismatched-digest shard is flagged as corrupt and excluded before erasure decoding.
3. **Fast-Path vs. Erasure Decoding:**
   - **Fast-Path:** If all $k$ systematic data shards (`shard_id` $0 \dots k-1$) are `HealthyVerified`, their verified bytes are concatenated and truncated to `payload_length` without matrix inversion.
   - **Decoding Path:** If 1 or more data shards are missing/corrupt:
     - For $\text{RS}(20, 12)$: Requires at least $k = 12$ distinct `HealthyVerified` shards; invokes `ReedSolomonCodec::decode_stripe`.
     - For $\text{LRC}(22, 12, 4)$ / $\text{LRC}(20, 12, 4)$: Verifies `LrcConfig::is_pattern_decodable` on the set of `HealthyVerified` shards; invokes `LrcCodec::decode_stripe`.
4. **End-to-End Object Digest Verification:**
   Streams reconstructed stripe payloads to a temporary output file `.tmp` while updating a streaming `Sha256Hasher`. After the final stripe, verifies `computed_object_digest == manifest.object_digest` before atomically renaming the output file to the caller's target path.

---

## 2. Verifying Shard Repair (`src/recovery/repair.rs`)

Shard repair restores missing or corrupted shards on disk without rewriting healthy shards:

1. **Health Inspection (`inspect_object`):**
   Audits every shard of every stripe, classifying each placement into:
   - `Healthy`: Present on an online node with matching byte length and SHA-256 digest.
   - `Missing`: Shard file does not exist on an online node.
   - `Corrupted`: Shard file exists on an online node, but is truncated, oversized, or fails SHA-256 verification.
   - `NodeOffline`: Host node directory is unreachable (`ObservedNodeState::Suspect` or `ObservedNodeState::AssumedLost`).
2. **Source Selection & Minimal-Amplification Reconstruction:**
   - For $\text{LRC}(22, 12, 4)$ / $\text{LRC}(20, 12, 4)$: If a single shard in a local group is unhealthy and its $l = 4$ local group peers are `Healthy`, `LrcCodec::reconstruct_specific_shards` executes **local 4-shard XOR reconstruction**, reading only $4 \times \text{shard\_length}$ bytes (`RepairMethod::LocalGroup`).
   - Otherwise (multi-shard erasure or $\text{RS}(20, 12)$), selects $k = 12$ linearly independent `Healthy` shards and reconstructs all damaged shards in the stripe in a single pass (`RepairMethod::GlobalReconstruction`).
3. **Pre-Publication & Post-Publication Verification:**
   - Verifies that the reconstructed shard bytes match `placement.sha256_digest` in memory.
   - Writes the shard to `<node-dir>/objects/<object_id>/stripe-XXXXXX.shard-YY.v<V>.fsp.repair.tmp` and calls `File::sync_all()`.
   - Re-reads the `.repair.tmp` file from disk and verifies its SHA-256 digest.
   - Atomically renames `.repair.tmp` to the canonical shard filename, removes any stale older version files in that slot, and syncs the parent directory.
   - Re-verifies the published shard file on disk before incrementing `shards_repaired`.

---

## 3. Phase 2 `FspAdaptiveV2Policy` Integration Without Oracle Leakage

[`AdaptiveStorageRepairPlanner`](file:///home/rade/Project/Jobs/fsp/src/recovery/repair.rs) connects the persistent storage engine to Phase 2 [`FspAdaptiveV2Policy`](file:///home/rade/Project/Jobs/fsp/src/strategies/adaptive_v2.rs) and [`FailureDetector`](file:///home/rade/Project/Jobs/fsp/src/detection/mod.rs):

- **No `PhysicalNodeState` Oracle Access:** The storage repair planner never reads simulator ground truth. Instead, it combines:
  1. **Direct Disk Verification on Online Nodes:** Missing or SHA-256-corrupted shard files on reachable nodes are immediately classified as confirmed permanent shard losses with posterior $\hat{p}_{\text{perm}} = 1.0$.
  2. **Heartbeat Observation on Unreachable Nodes:** Shards hosted on unreachable nodes query `ClusterObservationView` (`ObservedNodeState::Suspect` vs `ObservedNodeState::AssumedLost`) to obtain silence duration $\Delta t_{\text{silent}}$ and Bayesian permanent-failure posterior $\hat{p}_{\text{perm}}(\Delta t_{\text{silent}})$.
- **Decision Flow:**
  - Invokes `FspAdaptiveV2Policy::evaluate_blind(...)`, which calls `estimate_object_risk(...)` (`src/reliability/risk.rs`).
  - Returns `BlindRepairDecision { shards_to_repair, repairs_to_cancel }`.
  - Only shards selected by `shards_to_repair` are reconstructed and written to replacement/target node directories.
