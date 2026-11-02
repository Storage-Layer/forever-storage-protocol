# FSP Phase 5.1 — Secure Emergency Read Protocol & Security Architecture

## 1. Executive Summary & Threat Model
In prior phases, offline emergency recovery allowed reading and reconstructing shards directly from storage node roots without checking credentials or authenticating object manifests. In adversarial environments or multi-tenant deployments, unauthenticated emergency recovery represents a severe vulnerability:
1. **Unauthorized Access:** Rogue actors with local or read-only filesystem access could reconstruct sensitive tenant data without proof of ownership or authorization.
2. **Untrusted / Rollback Manifest Injection:** Decoding with an unverified or outdated manifest could cause the recovery of stale, deleted, or attacker-manipulated data.
3. **Corrupted Shard Poisoning:** Corrupted or tampered shards could corrupt the Cauchy Reed-Solomon decoding pipeline, yielding invalid data or silent data corruption.
4. **Namespace Isolation Breaches:** Emergency recovery could allow crossing tenant boundaries if recovery capability tokens are not cryptographically bound to a specific tenant namespace.

In Phase 5.1, we implemented the **Secure Emergency Read Protocol** in [src/security/recovery_auth.rs](file:///home/rade/Project/Jobs/fsp/src/security/recovery_auth.rs) to eliminate these security gaps.

## 2. Cryptographic Capability Token Architecture
Every emergency read requires a cryptographically valid [`RecoveryCapabilityToken`](file:///home/rade/Project/Jobs/fsp/src/security/recovery_auth.rs):
- **Object ID Binding:** The token explicitly names the exact object allowed for recovery.
- **Namespace Scoping:** Scoped to an explicit tenant namespace (e.g., `tenant_finance`).
- **Principal Identity:** Bound to an authorized recovery administrator identity.
- **Expiration Timestamp:** Explicit Unix timestamp; expired tokens are rejected immediately.
- **Ed25519 Signature:** Signed using RFC 8032 Ed25519 under domain tag `FSP-RECOVERY-TOKEN-V1`.

```rust
pub struct RecoveryCapabilityToken {
    pub object_id: String,
    pub namespace: String,
    pub principal_id: String,
    pub expires_at_unix_secs: u64,
    pub signature_hex: String,
}
```

## 3. Recovery Freshness Determination
The recovery engine discovers signed object manifests ([`SignedObjectManifest`](file:///home/rade/Project/Jobs/fsp/src/security/signatures.rs)) and evaluates their currency against the [`RecoveryTrustStore`](file:///home/rade/Project/Jobs/fsp/src/security/recovery_auth.rs):
- **`AuthenticLatestCommitted`**: The manifest's generation matches or exceeds the highest known verified durable checkpoint.
- **`AuthenticHistoricalVersion`**: The manifest is authentic and correctly signed, but an anti-rollback checkpoint indicates a strictly newer version exists in the cluster.
- **`UnverifiableFreshness`**: The manifest is cryptographically authentic, but no durable checkpoint is available in the trust store to guarantee latest currency.

## 4. Multi-Stripe Shard Verification & Cauchy RS(20,12) Reconstruction
Reconstruction proceeds in an adversarial-resistant pipeline:
1. **Per-Shard SHA-256 Digest Verification:** Every candidate shard file (`.fspc` or `.shard`) is hashed and compared against the signed manifest placement table.
2. **Automatic Bitrot & Corruption Filtering:** Corrupt or tampered shards are rejected, `corrupted_shards_detected` metric is incremented, and alternate valid shards are gathered.
3. **Cauchy Reed-Solomon RS(20,12) Decoding:** Requires at least 12 valid shards. Full stripes are decoded using Cauchy generator matrices in $\text{GF}(2^8)$.
4. **Byte-Level Stripe Trimming:** Decoded stripes are truncated to `stripe_info.payload_length`, preventing zero-padding bytes from polluting subsequent stripes.
5. **Full Payload Digest Verification:** The final reconstructed payload's SHA-256 digest is strictly verified against `manifest.object_digest` before atomic disk sync.

## 5. Experimental Verification Matrix
The secure emergency read protocol was validated against 14 test cases in [tests/phase5_1_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_1_validation.rs):
- `test_01`: Authenticated recovery succeeds and verifies 100% byte match.
- `test_02`: Expired tokens are denied (`Token expired`).
- `test_03`: Forged signatures are denied (`Cryptographic signature verification failed`).
- `test_04`: Unknown principals are denied (`Unknown or untrusted recovery principal`).
- `test_05`: Namespace mismatches are denied (`Token namespace mismatch`).
- `test_06`: Object ID mismatches are denied (`Token object_id mismatch`).
- `test_07`: Tampered manifest signatures are denied (`No cryptographically valid SignedObjectManifest found`).
- `test_08`: Untrusted manifest signers are denied (`No cryptographically valid SignedObjectManifest found`).
- `test_09`: Shard hash mismatches are filtered out, alternative shards decoded.
- `test_10`: < 12 surviving shards rejected safely without partial file creation.
- `test_11`, `test_12`, `test_13`: Freshness classification verified across all 3 variants.
- `test_14`: Multi-stripe exact byte trimming verified without padding leakage.
