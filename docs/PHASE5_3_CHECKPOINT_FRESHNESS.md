# FSP Phase 5.3: Quorum Checkpoint Freshness Model & Counterexample Analysis

## 1. The Offline Quorum Checkpoint Fallacy
In consensus protocols (e.g., Raft, Paxos, Viewstamped Replication), a majority quorum signature over state at term $T$ and commit index $I$ guarantees:
$$\text{State at } \langle T, I \rangle \text{ is committed and cannot be overwritten without intersecting this quorum.}$$

However, Phase 5.3 identifies and formally establishes the following **Offline Freshness Counterexample**:
> **Theorem (Offline Checkpoint Incompleteness):**
> A majority quorum certificate $\mathcal{Q}_T$ proving commit of object generation $G$ at term $T$ does **not** prove that generation $G$ is the globally latest committed state.
>
> *Proof:*
> Suppose a 3-node cluster $C = \{n_1, n_2, n_3\}$ commits generation $G_1$ at term $T=1$ with signatures from $\{n_1, n_2\}$.
> Coordinator leader crashes or is partitioned.
> In term $T=2$, nodes $\{n_2, n_3\}$ form a new quorum, elect a new leader, and commit generation $G_2 > G_1$.
> A catastrophic power failure destroys $n_2$ and $n_3$, leaving only node $n_1$.
> An offline recovery operator discovering the majority checkpoint for $G_1$ from term 1 would observe a valid majority certificate $\{n_1, n_2\}$, yet $G_1$ is stale because $G_2$ was committed in term 2. $\blacksquare$

## 2. Refined Freshness Taxonomy
To eliminate false claims of global currency, Phase 5.3 redesigns [`RecoveryFreshnessStatus`](file:///home/rade/Project/Jobs/fsp/src/security/recovery_auth.rs):

```rust
pub enum RecoveryFreshnessStatus {
    /// Reconstruction is proven to be the global latest state backed by live cluster consensus synchronization proof.
    AuthenticGlobalLatest {
        certified_generation: u64,
        quorum_voters_count: usize,
    },
    /// Offline majority-signed checkpoint verifies committed status at term T, but subsequent unverified commits may exist.
    AuthenticatedCommittedCheckpoint {
        certified_generation: u64,
        quorum_voters_count: usize,
        unverified_subsequent_commits_possible: bool,
    },
    /// Reconstruction matches local checkpoint watermark, unverified globally.
    AuthenticLocallyCurrent {
        reconstructed_generation: u64,
        local_checkpoint_generation: u64,
    },
    /// Reconstruction is cryptographically authentic, but a newer durable generation exists.
    AuthenticHistoricalVersion {
        reconstructed_generation: u64,
        known_latest_generation: u64,
    },
    /// Manifest signature is valid, but no durable checkpoint is available.
    UnverifiableFreshness { manifest_generation: u64 },
    /// Divergent, conflicting authentic checkpoints detected from different or partitioned quorums.
    ConflictingAuthorityEvidence {
        object_id: String,
        conflicting_generations: Vec<u64>,
    },
}
```

## 3. Conflict Detection Semantics
When multiple checkpoints are registered in [`RecoveryTrustStore`](file:///home/rade/Project/Jobs/fsp/src/security/recovery_auth.rs):
- If two valid majority checkpoints exist for the same object with differing generation numbers, commit indices, or terms, the trust store records a divergence.
- Emergency read fails safe with `RecoveryFreshnessStatus::ConflictingAuthorityEvidence`, prompting operators to inspect physical logs rather than silently accepting a stale or forked state.

Verified by tests:
- [`test_09_quorum_signed_checkpoint_classified_as_authenticated_committed`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L550-L615)
- [`test_10_missing_global_freshness_evidence_truthfully_reported`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L617-L690)
- [`test_11_conflicting_authentic_checkpoints_detected_and_reported`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L692-L765)
- [`test_12_restore_from_historical_checkpoint_classified_correctly`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L767-L825)
