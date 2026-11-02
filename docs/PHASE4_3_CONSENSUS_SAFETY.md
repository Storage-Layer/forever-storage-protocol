# FSP Phase 4.3: Consensus Safety Specification & Architecture

This document specifies the consensus safety architecture implemented in Phase 4.3 of the **Forever Storage Protocol (FSP)**.

---

## 1. Architectural Foundations

FSP employs a 3-member quorum cluster to govern cluster metadata, leadership epochs, and shard placement policies:
- **Cluster Size ($N$):** 3 nodes (Primary, Standby-1, Standby-2 / Quorum Witness).
- **Majority Quorum Threshold ($Q$):** $\lfloor N/2 \rfloor + 1 = 2$.
- **Consensus Invariant:** Any two quorums of size $\ge 2$ in a 3-node system intersect in at least $2 + 2 - 3 = 1$ node. This mathematical pigeonhole principle guarantees that no two leaders can concurrently hold quorum consensus in the same term or across successive terms without intersecting.

---

## 2. Cryptographic Quorum Election

In Phase 4.3, quorum votes and election certificates are secured with Ed25519 digital signatures.

### 2.1 Quorum Vote Grants
A candidate coordinator solicits votes by sending a [`QuorumVoteRequest`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs#L90) specifying `(candidate_id, proposed_epoch, last_committed_manifest_gen)`.
When a voter grants a vote, it executes [`QuorumState::handle_vote_request_signed`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs#L578) and produces a [`QuorumVoteGrant`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs#L106):
```rust
QuorumVoteGrant {
    voter_id: CoordinatorId,
    epoch: u64,
    candidate_id: CoordinatorId,
    signature_hex: String, // Ed25519 signature over "FSP-QUORUM-VOTE-V1:<epoch>:<candidate_id>:<voter_id>"
}
```

### 2.2 Leadership Certificates
A candidate aggregates vote grants. Once grants from $\ge 2$ distinct trusted voters are collected, it forms a [`LeadershipCertificate`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs#L187):
```rust
LeadershipCertificate {
    epoch: u64,
    leader_id: CoordinatorId,
    grants: Vec<QuorumVoteGrant>,
}
```
The method [`verify_quorum`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs#L201) verifies:
1. `grants.len() >= threshold`.
2. All grants match the certificate's `epoch` and `leader_id`.
3. All `voter_id`s are distinct members registered in the cluster public key ring.
4. Every Ed25519 signature verifies strictly under the respective voter's public key.

---

## 3. Replicated Log & Majority Quorum Commit

Metadata replication between coordinators is managed by [`ReplicatedLogManager`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L341):
1. **Uncommitted Proposal:** When the active leader receives a metadata mutation, it appends a [`LogEntry`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L278) to its local log and writes to `replicated_log.wal` with fsync.
2. **Replication RPC:** The leader dispatches [`AppendEntriesRequest`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs#L320) containing `(term, prev_log_index, prev_log_term, entries, leader_commit_index)`.
3. **Log Matching Check:** Standby replicas verify that the entry at `prev_log_index` matches `prev_log_term`. If mismatched, the request is rejected and conflicting entries are truncated.
4. **Majority Commit Barrier:** The leader only advances `commit_index` when acknowledgments are received from a quorum majority ($\ge 2$ nodes).
5. **Client Visibility:** Uncommitted proposals are never returned to clients as committed manifests or applied states.

---

## 4. Fail-Closed Quorum Loss Semantics

If the active leader experiences a network partition and cannot communicate with $\ge 2$ quorum nodes:
- The leader immediately invokes [`QuorumState::step_down_to_fenced`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs#L561).
- Its role transitions to `CoordinatorRole::Fenced`.
- Its in-memory `LeadershipCertificate` is cleared (`None`).
- All mutating requests (`PutShard`, `DeleteShard`, `CommitManifest`) are blocked.
- Storage nodes cannot be updated by the isolated leader because it cannot generate fresh operation tokens with valid quorum certificates.
