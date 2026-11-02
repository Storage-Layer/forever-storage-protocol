# FSP Phase 5.3: Distributed Disaster Recovery & Emergency Reconstruction

## 1. Overview of Recovery Topologies
FSP operates a 20-node distributed storage architecture structured across 5 failure domains with RS(20,12) byte-level Reed-Solomon erasure coding.
Under catastrophic coordinator loss, surviving storage nodes remain independently accessible via the **Secure Emergency Read Protocol**.

```
Storage Roots:   Node 00..03 [Zone A]  Node 04..07 [Zone B]  Node 08..11 [Zone C]
                 Node 12..15 [Zone D]  Node 16..19 [Zone E]
Erasure Coding:  k = 12 data shards, m = 8 parity shards per stripe
Fault Tolerance: Up to 8 nodes or 2 entire failure zones can fail simultaneously.
```

## 2. Emergency Read Execution
[`execute_secure_emergency_read`](file:///home/rade/Project/Jobs/fsp/src/security/recovery_auth.rs) operates offline without a live coordinator:
1. **Token Verification:** Validates `RecoveryCapabilityToken` signed by an authorized recovery principal, checking object ID, namespace, and expiration.
2. **Manifest Discovery:** Scans storage node roots to locate the highest cryptographically valid `SignedObjectManifest`.
3. **Freshness Assessment:** Evaluates manifest generation against `RecoveryTrustStore` checkpoints, determining whether state is committed, historical, or conflicting.
4. **Per-Shard Integrity & Bitrot Rejection:** Gathers container shards (`.fspc`), computing SHA-256 digests and discarding any tampered or corrupted blocks.
5. **RS(20,12) Multi-Stripe Decoding:** Uses Cauchy/Vandermonde matrices to reconstruct original data stripes from any $\ge 12$ surviving shards.
6. **Payload Digest Verification:** Reconstructed data stream is trimmed to exact byte size and compared against the manifest's `object_digest`.

## 3. Catastrophic Loss Falsification Test
In test [`test_24_twelve_of_twenty_exact_recovery_and_nine_loss_safe_failure`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs):
- With exactly 8 shards deleted (12 surviving): reconstruction succeeds byte-for-byte.
- With 9 shards deleted (11 surviving): reconstruction aborts cleanly with `Insufficient valid surviving shards`, producing zero corrupt output files.

Full telemetry documented in [`results/phase5_3/distributed_recovery.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_3/distributed_recovery.json).
