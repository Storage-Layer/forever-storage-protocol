# FSP Phase 5.1 — Quorum Mutation Permit Replay Protection

## 1. Threat Analysis
In distributed consensus storage architectures, mutating requests sent to storage nodes (such as writing or deleting shards) must carry cryptographic proof of coordinator authority. In Phase 5 and Phase 5.1, we analyzed three permit replay threats:
1. **Cross-Node Permit Replay:** An attacker intercepts a valid `PutShard` or `DeleteShard` permit targeted at Node 1 and replays it against Node 2.
2. **Cross-Operation Permit Replay:** An attacker takes a permit issued for benign shard staging (`StageShard`) and presents it to authorize destructive deletion (`DeleteShard`).
3. **Failover Delayed Replay:** An old coordinator partitioned from quorum attempts to replay an older permit against a node that has already advanced to a higher epoch.

## 2. QuorumMutationPermit V2 Security Architecture
Implemented in [src/coordinator/fencing.rs](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs), [`QuorumMutationPermit`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs) enforces strict 5-dimensional binding:
- **Epoch & Commit Index:** Sealed by majority voter signatures; rejected if epoch or commit index is stale.
- **Node ID Binding (`target_node: NodeId`):** The permit and all constituent grants explicitly name the target storage node. If presented to any other node, it is rejected immediately.
- **Operation Type Binding (`operation_type: PermitOperationType`):** Explicitly specifies `StageShard`, `CommitManifest`, `DeleteShard`, or `GarbageCollectShard`. A permit cannot be re-purposed for a different operation.
- **Payload SHA-256 Digest:** Cryptographically matches the exact byte sequence of the shard.
- **Idempotency & Replay Cache:** Storage nodes record executed request IDs in `executed_requests.rec`.

## 3. Storage Node Enforcement Pipeline
When a storage node evaluates [`verify_and_apply_mutation_permit_v2`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs):
1. Verifies `permit.target_node == Some(expected_node_id)`.
2. Verifies `permit.operation_type == Some(expected_operation)`.
3. Verifies majority quorum voter signatures.
4. Enforces epoch high-water mark monotonically.
5. Enforces commit index monotonicity.
6. Enforces object generation monotonicity and request idempotency.

## 4. Verification Evidence
- [tests/phase5_1_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_1_validation.rs) (`test_23_quorum_permit_node_mismatch_rejected`): Confirms that presenting a Node 1 permit to Node 2 fails with `destination node mismatch`.
- [tests/phase5_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_validation.rs) (`test_p5_01`, `test_p5_02`, `test_p5_03`): Confirms cross-operation and cross-node replay rejection.
