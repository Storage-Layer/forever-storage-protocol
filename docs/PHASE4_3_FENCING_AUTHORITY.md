# FSP Phase 4.3: Storage Node Epoch Fencing & Cryptographic Operation Authority

This document details the node-side epoch fencing implementation and request authorization tokens introduced in **FSP Phase 4.3**.

---

## 1. Problem Statement & Historical Vulnerability

Prior to Phase 4.3, storage nodes accepted epoch advances based solely on numerical comparison:
```rust
// Legacy vulnerable code (Phase 4.2):
if presented_epoch > cur {
    write_and_sync_file(&self.file_path, text.as_bytes())?;
    self.highest_seen_epoch.store(presented_epoch, Ordering::SeqCst);
}
```
An attacker, rogue node, or partitioned coordinator could issue a single mutating request with `epoch = 999999`, permanently disabling legitimate cluster leaders by locking them out of the storage node.

Furthermore, mutating RPCs did not cryptographically bind the operation parameters (request ID, object ID, shard ID, version, payload hash) to the leader's keypair.

---

## 2. Hardened Architecture

### 2.1 Storage Node Strict Quorum Enforcement
In Phase 4.3, [`NodeEpochFencingTracker`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L230) supports a strict validation mode:
```rust
pub fn check_and_update_with_certificate(
    &self,
    cert: &LeadershipCertificate,
    voter_keys: &BTreeMap<CoordinatorId, VerifyingKey>,
    threshold: usize,
) -> Result<(), RpcError>
```
Before advancing the node's durable high-water mark `epoch.rec`, the node enforces:
1. `cert.verify_quorum(voter_keys, threshold)`: Validates that the certificate contains $\ge 2$ authentic Ed25519 signatures from authorized cluster quorum voters.
2. `cert.epoch >= self.highest_seen_epoch()`: Prevents regressions.
3. Durable persistence: Fsyncs the new high-water mark to `epoch.rec` and syncs the parent directory before returning success.

In strict mode, any call to uncertified `check_and_update` is immediately aborted with `RpcError::Unauthorized("storage node strict mode requires verified LeadershipCertificate to advance epoch")`.

### 2.2 Fenced Operation Tokens
Mutating operations on storage nodes require a [`FencedOperationToken`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L73):
```rust
pub struct FencedOperationToken {
    pub epoch: u64,
    pub leader_id: CoordinatorId,
    pub request_id: String,
    pub object_id: String,
    pub shard_id: u32,
    pub version: u64,
    pub payload_sha256_hex: String,
    pub timestamp_unix_secs: u64,
    pub signature_hex: String,
}
```
The canonical message for signature generation and verification is domain-separated:
`FSP-FENCED-OP-V1:<epoch>:<leader_id>:<request_id>:<object_id>:<shard_id>:<version>:<payload_sha256_hex>`

### 2.3 Comprehensive Verification Checks
When a storage node receives a fenced operation, [`NodeEpochFencingTracker::verify_fenced_operation`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs#L358) enforces:
1. **Epoch Non-Staleness:** `token.epoch >= highest_seen_epoch`. If lower, returns `RpcError::Fenced`.
2. **Object Binding:** `token.object_id == expected_object_id`.
3. **Shard Binding:** `token.shard_id == expected_shard_id`.
4. **Payload Digest Binding:** `token.payload_sha256_hex == sha256(payload)`. Prevents in-flight tampering.
5. **Cryptographic Signature:** `token.verify(leader_key)` must strictly succeed.

---

## 3. Verification Test Coverage

The following tests in [`tests/phase4_3_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase4_3_validation.rs) verify this subsystem:
- `test_p4_3_01_forged_epoch_without_quorum_cert_rejected`: Confirms uncertified epoch advances are blocked in strict mode.
- `test_p4_3_02_quorum_certificate_with_majority_signatures_accepted`: Confirms valid certificates advance the watermark.
- `test_p4_3_03_stale_leader_mutation_rejected_by_fencing_token`: Confirms older epoch leaders cannot mutate storage.
- `test_p4_3_08_delayed_rpc_replay_rejected_after_failover`: Confirms delayed RPC replay from previous epoch is blocked.
- `test_p4_3_18_storage_node_rejects_tampered_payload_digest_binding`: Confirms payload tampering is rejected.
