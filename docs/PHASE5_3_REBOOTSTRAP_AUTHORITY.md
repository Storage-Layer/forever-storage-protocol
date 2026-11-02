# FSP Phase 5.3: Disaster Rebootstrap Authority & Invalidation Architecture

## 1. Executive Summary & Problem Formulation
In previous iterations (Phase 5.1 & Phase 5.2), emergency coordinator rebootstrap relied on an ad-hoc static numerical epoch increment:
$$\text{restored\_epoch} = \max(\text{bundle.term}, \text{local.epoch}) + 100$$
While this static offset provided a simple defense against immediate restarts, it suffered from a fundamental distributed systems vulnerability:
1. **Unbounded Stale Epochs:** If a partitioned or severed coordinator had previously advanced its epoch to $\ge 500$, adding $100$ to an older backup at term $5$ leaves the rebootstrap coordinator at epoch $105$, which remains strictly fenced out and superseded by the partitioned leader.
2. **Cluster Identity Collision:** A restored coordinator operating without explicit cluster incarnation binding cannot prove whether it belongs to the active physical lineage or a divergent partition.
3. **Old-Quorum Coexistence:** When surviving storage nodes are partitioned, an unverified old quorum might continue issuing mutation permits concurrently with the newly rebootstrap coordinator.

## 2. Cryptographic Rebootstrap Authorization
Phase 5.3 replaces static epoch jumping with explicit, cryptographically authenticated rebootstrap authority transitions using [`DisasterRebootstrapAuthorization`](file:///home/rade/Project/Jobs/fsp/src/recovery/disaster_recovery.rs):

```rust
pub struct DisasterRebootstrapAuthorization {
    pub operator_id: String,
    pub recovery_incarnation_id: String,
    pub authorization_timestamp_unix_secs: u64,
    pub justification: String,
    pub signature_hex: String,
    pub target_cluster_id: Option<String>,
    pub target_incarnation: Option<ClusterIncarnationId>,
    pub operational_isolation_attested: bool,
}
```

### Verification Pipeline
When executing [`execute_restore_v2`](file:///home/rade/Project/Jobs/fsp/src/recovery/disaster_recovery.rs) under `RestoreMode::IsolatedRebootstrap`:
1. **Operator Signature Verification:** The Ed25519 signature of the operator is verified against trusted disaster recovery credentials.
2. **Operator Identity Matching:** `auth.operator_id` must match `bundle.operator_id`.
3. **Incarnation Adoption:** If a `target_incarnation` is provided, storage nodes atomically persist `incarnation.rec` and record full audit provenance in `recovery_provenance.rec`.
4. **Coexistence Fail-Closed Policy:** If `operational_isolation_attested` or `old_quorum_fenced` is `false`, the storage node enters `coexistence_warning_active` mode, strictly rejecting all subsequent write mutations.

## 3. Provenance Audit Trails
Upon successful rebootstrap restore, the node durably syncs `recovery_provenance.rec` containing:
- `INCARNATION_ID`: Monotonic cluster incarnation identifier.
- `OPERATOR_ID`: Authenticated operator key identity.
- `TIMESTAMP_UNIX_SECS`: Exact UTC timestamp of transition.
- `RESTORED_COMMIT_INDEX` vs `PREVIOUS_COMMIT_INDEX`.
- `RESTORED_EPOCH` vs `PREVIOUS_EPOCH`.
- `OPERATIONAL_ISOLATION_ATTESTED`: Boolean flag confirming physical isolation.
- `AUTHORIZATION_SIGNATURE`: Cryptographic proof of transition authority.

Verified by automated tests:
- [`test_01_rebootstrap_with_old_leader_still_active_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L190-L280)
- [`test_04_rebootstrap_authorization_forgery_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L340-L370)
