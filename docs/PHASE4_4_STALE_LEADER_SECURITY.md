# Phase 4.4 Stale Leader Security & Consensus Mutation Authority

**Document ID:** `PHASE4_4_STALE_LEADER_SECURITY.md`  
**Protocol Version:** FSP v0.4.4  
**Date:** 2026-11-01  
**Author:** Principal Distributed Systems Architect, Consensus Security Researcher, Formal Verification Specialist  

---

## 1. Vulnerability Analysis: Stale Leader Mutation Partition Attack

In Phase 4.3, leadership transitions were certified via Ed25519-signed [`LeadershipCertificate`](file:///home/rade/Project/Jobs/fsp/src/coordinator/leadership.rs#L120-L160) instances representing majority quorum approval ($Q = 2$ of 3 voters). Once elected, the active coordinator signed [`FencedOperationToken`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L150-L210) instances unilaterally using its private key.

### Root Cause Mechanism
A critical distributed race condition existed when a network partition occurred:
1. Coordinator $C_1$ is elected leader in Term 1 with certificate $Cert_1$.
2. Coordinator $C_1$ is partitioned from the majority of coordinators $\{C_2, C_3\}$, but retains network connectivity to storage node $N_1$.
3. Coordinators $C_2$ and $C_3$ elect $C_2$ as the new leader in Term 2 with certificate $Cert_2$.
4. Storage node $N_1$, having not yet communicated with $C_2$ or $C_3$, still maintains its local epoch high-watermark at Term 1.
5. Deposed coordinator $C_1$ issues a mutating operation to $N_1$ using its valid, unexpired Term 1 signing key.
6. Node $N_1$ checks $epoch_{presented} \ge epoch_{watermark}$ ($1 \ge 1$), validates the cryptographic signature with $C_1$'s public key, and **accepts the stale mutation**, causing a split-brain write and corrupting distributed data consistency.

---

## 2. Cryptographic Remediation: Quorum Mutation Permits

To completely eliminate unilateral leader mutation vulnerability, FSP Phase 4.4 replaces unilateral operation tokens with consensus-authorized **Quorum Mutation Permits** ([`QuorumMutationPermit`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L320-L370)).

### Mathematical Invariant
$$\forall \text{ mutation } M, \quad \text{Valid}(M) \iff \left| \left\{ v \in \mathcal{V} \mid \text{VerifySig}_v(\text{Grant}(M, \text{term}(M), \text{index}(M))) = \text{true} \right\} \right| \ge \left\lfloor \frac{|\mathcal{V}|}{2} \right\rfloor + 1$$

Under this protocol:
1. No coordinator—even an active leader—can mutate a storage node using only its own signing key.
2. Every mutating request must bundle a [`QuorumMutationPermit`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L320-L370) containing at least $\ge 2$ distinct [`QuorumPermitGrant`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L220-L270) signatures from the trusted voter set.
3. Because deposed leader $C_1$ is partitioned from $\{C_2, C_3\}$ (who have advanced to Term 2 and refuse to sign Term 1 grants), $C_1$ cannot assemble a majority permit.
4. When $C_1$ attempts to write to $N_1$, $N_1$ inspects the permit, counts fewer than 2 grants, and strictly rejects the request with:
   ```text
   Unauthorized("insufficient quorum permit grants: 1 < required threshold 2")
   ```

---

## 3. Storage Node Defense-in-Depth Pipeline

Storage nodes enforce a five-stage verification pipeline in [`NodeEpochFencingTracker::verify_and_apply_mutation_permit`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L658-L775):

1. **Quorum Majority Validation:** Verifies that $\ge 2$ grants exist, all belong to trusted cluster voters, and all signatures verify strictly over the canonical message domain tag `FSP-PERMIT-GRANT-V1`.
2. **Epoch Fencing Check:** Ensures $epoch_{permit} \ge epoch_{watermark}$. If greater, the watermark is advanced and fsynced to `epoch.rec`.
3. **Commit Index Monotonicity:** Verifies $commit\_index_{permit} \ge highest\_seen\_commit\_index$. Prevents delayed out-of-order RPC replay.
4. **Target Parameter Integrity:** Asserts exact matching of `object_id`, `shard_id`, and `payload_sha256_hex`.
5. **Durable Object Generation & Idempotency:** Verifies that version is strictly non-decreasing against `*.gen` on disk. If version equals recorded generation, request is accepted only if `request_id` matches the durable idempotency store (`executed_requests.rec`).

---

## 4. Adversarial Verification Test Suite

The 10 adversarial scenarios required by Phase 4.4 were implemented and verified in [`tests/phase4_4_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs):

| Test ID | Test Function | Adversarial Vector | System Decision |
|---|---|---|---|
| `P44-01` | [`test_p4_4_01_stale_leader_stale_storage_node_rejected_by_permit`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L112-L215) | Deposed $C_1$ attempts write to lagging $N_1$ with single grant | **REJECTED** (`insufficient quorum grants`) |
| `P44-02` | [`test_p4_4_02_stale_leader_updated_storage_node_rejected_fenced`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L217-L280) | Deposed $C_1$ attempts write to updated $N_1$ ($T=3 < 5$) | **REJECTED** (`Fenced`) |
| `P44-03` | [`test_p4_4_03_fabricated_higher_leadership_term_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L282-L345) | Rogue leader claims Term 999 with forged Ed25519 keys | **REJECTED** (`crypto signature failure`) |
| `P44-04` | [`test_p4_4_04_previously_valid_certificate_replay_without_permit_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L347-L395) | Valid LeadershipCertificate replayed without mutation permit | **REJECTED** (`empty grants`) |
| `P44-05` | [`test_p4_4_05_delayed_rpc_delivered_after_failover_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L397-L485) | Delayed RPC delivered after commit index advanced | **REJECTED** (`non-monotonic commit index`) |
| `P44-06` | [`test_p4_4_06_coordinator_isolated_from_majority_cannot_obtain_permits`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L487-L500) | Isolated coordinator attempts permit acquisition | **REJECTED** (`majority unreachable`) |
| `P44-07` | [`test_p4_4_07_quorum_loss_during_mutation_fails_closed`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L502-L525) | Voter timeout during replication | **REJECTED** (`fails closed`) |
| `P44-08` | [`test_p4_4_08_storage_node_restart_preserves_commit_index_and_generations`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L527-L615) | Node restarted; stale replay attempted | **PRESERVED** (`idempotency store loaded`) |
| `P44-09` | [`test_p4_4_09_conflicting_object_generation_updates_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L617-L690) | Permit attempts generation rollback ($10 \to 8$) | **REJECTED** (`generation rollback rejected`) |
| `P44-10` | [`test_p4_4_10_repair_operation_during_leadership_transfer_resets_pending`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L692-L715) | Active repair interrupted by failover | **RECONCILED** (`reset to Pending`) |
