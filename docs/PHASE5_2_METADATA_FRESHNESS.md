# Phase 5.2 Metadata Freshness & Quorum Certified Checkpoints

## 1. Background & Flaw Analysis
In Phase 5.1, offline emergency recovery classified an object as `AuthenticLatestCommitted` if its manifest generation matched the local checkpoint recorded in the trust store. However, in a distributed system, a single node's checkpoint is merely local state. It cannot guarantee that higher generations were not committed by an active quorum elsewhere in the cluster.

## 2. Quorum Certified Checkpoints
Phase 5.2 introduces `QuorumCertifiedCheckpoint`, proving majority coordinator consensus for an object checkpoint:
- **Canonical Message:**
  ```
  FSP-QUORUM-CHECKPOINT-V1
  object=<object_id>
  generation=<generation>
  epoch=<epoch>
  commit_index=<commit_index>
  ```
- **Voter Quorum:** Contains digital signatures from $\ge \lfloor N/2 \rfloor + 1$ independent coordinator voters.
- **Verification:** Evaluated by `verify(&self, voter_keys, threshold)`.

## 3. Freshness Status Classification
`RecoveryFreshnessStatus` categorizes data currency into unambiguous levels:

| Freshness Status | Condition | Meaning |
|---|---|---|
| `AuthenticGlobalLatest` | Manifest generation equals verified `QuorumCertifiedCheckpoint` generation | Provably the global latest committed state across the cluster |
| `AuthenticLocallyCurrent` | Manifest generation equals local checkpoint watermark, but no quorum proof | Matches local node/backup checkpoint; global certainty unverified |
| `AuthenticHistoricalVersion` | Manifest generation is strictly lower than known checkpoint | Authentic data, but newer committed generation exists |
| `UnverifiableFreshness` | Manifest signature is valid, but no durable checkpoint is registered | Data is untampered, but currency cannot be guaranteed |

## 4. Test Verification
Verified by:
- `test_08_unverifiable_recovery_freshness_classification`
- `test_09_historical_manifest_recovery_classification`
- `test_10_quorum_certified_current_manifest_recovery`
- `test_21_unauthorized_emergency_read_rejected`
