# FSP Phase 5.3: Cluster Incarnation Model & Storage Node Authority Binding

## 1. System Invariants & Mathematical Formulation
Let $\mathcal{C}$ be a storage cluster identified by logical name $C_{\text{id}}$.
A cluster incarnation is defined as an immutable tuple:
$$\mathcal{I} = \langle C_{\text{id}}, \sigma, \tau \rangle$$
where:
- $C_{\text{id}} \in \Sigma^*$ is the logical cluster identifier.
- $\sigma \in \mathbb{N}_{\ge 1}$ is a strictly monotonic sequence counter.
- $\tau \in \{0, 1\}^{256}$ is a collision-resistant cryptographic entropy token.

### Evolution Operator
The transition from incarnation $\mathcal{I}_k = \langle C, k, \tau_k \rangle$ to $\mathcal{I}_{k+1}$ requires external entropy $e$:
$$\tau_{k+1} = \text{SHA-256}(C \parallel \tau_k \parallel (k+1) \parallel e)$$
$$\mathcal{I}_{k+1} = \langle C, k+1, \tau_{k+1} \rangle$$

## 2. Cluster Authority Record
A cluster incarnation transition is valid if and only if certified by a [`ClusterAuthorityRecord`](file:///home/rade/Project/Jobs/fsp/src/coordinator/incarnation.rs):
```rust
pub struct ClusterAuthorityRecord {
    pub cluster_id: String,
    pub incarnation_id: ClusterIncarnationId,
    pub recovery_authority_pubkey_hex: String,
    pub authorized_voter_set: BTreeSet<CoordinatorId>,
    pub lineage_digest_hex: String,
    pub transition_timestamp_unix_secs: u64,
    pub operational_isolation_attested: bool,
    pub old_quorum_fenced: bool,
    pub justification: String,
    pub authorization_signature_hex: String,
}
```

The signature $\Sigma_{\text{auth}}$ covers the canonical domain-tagged bytes:
$$\text{Msg} = \text{"FSP-CLUSTER-AUTHORITY-V1"} \parallel C \parallel \mathcal{I} \parallel \text{Lineage} \parallel T \parallel \text{Isolation} \parallel \text{Fenced} \parallel \text{Voters}$$

## 3. Storage Node Fencing & Binding Rules
Every storage node maintains a durable tracker [`NodeEpochFencingTracker`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs) persisted in `<storage_root>/incarnation.rec`.

When receiving a mutating request with permit $P$ claiming incarnation $\mathcal{I}_P = \langle C_P, \sigma_P, \tau_P \rangle$ against node state $\mathcal{I}_N = \langle C_N, \sigma_N, \tau_N \rangle$:
1. **Cluster ID Verification:** If $C_P \neq C_N$, reject with `RpcError::Unauthorized("cluster ID mismatch")`.
2. **Superseded Incarnation Rejection:** If $\sigma_P < \sigma_N$, reject with `RpcError::Fenced { current: \sigma_N, presented: \sigma_P }`.
3. **Premature Incarnation Rejection:** If $\sigma_P > \sigma_N$, reject with `RpcError::Unauthorized("node has not adopted higher incarnation")`.
4. **Token Integrity Check:** If $\tau_P \neq \tau_N$, reject with `RpcError::Unauthorized("incarnation token mismatch - divergent authority")`.

This strictly prevents any coordinator holding an older incarnation from ever mutating storage node shards, regardless of numerical epoch counters.

Verified by tests:
- [`test_02_rebootstrap_with_old_higher_epoch_fenced_by_incarnation`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L282-L340)
- [`test_05_storage_node_rejects_old_incarnation_permit`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L372-L440)
- [`test_06_storage_node_restart_preserves_adopted_incarnation`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L442-L485)
