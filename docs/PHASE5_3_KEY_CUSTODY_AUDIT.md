# FSP Phase 5.3: Cryptographic Key Custody & Type-Level Isolation Audit

## 1. Threat Model & Audit Scope
A catastrophic failure mode in distributed storage systems is the accidental leakage of private signing keys, consensus credentials, or decryption secrets into:
- Unencrypted disaster recovery metadata backups.
- Publicly readable object manifests.
- WAL replication logs and audit telemetry.

### Vulnerability of Regex-Based String Scanners
Phase 5.2 introduced `scan_for_plaintext_secrets` to inspect backup bundles for strings like `BEGIN PRIVATE KEY`. However, as a black-box text scanner:
1. **Raw Binary Secrets:** Raw 32-byte Ed25519 seed bytes or binary AES keys without ASCII PEM wrappers pass through undetected.
2. **Obfuscated or Non-Standard Hex:** Encoded binary structures could embed raw private keys in custom JSON or serialized fields.

## 2. Type-Level Allowlist Architecture
Phase 5.3 implements **Type-Level Secret Isolation** via [`StrictMetadataAllowlist`](file:///home/rade/Project/Jobs/fsp/src/recovery/disaster_recovery.rs):

```rust
pub struct StrictMetadataAllowlist {
    pub operator_id: String,
    pub coordinator_term: u64,
    pub last_log_index: u64,
    pub commit_index: u64,
    pub snapshot: MetadataSnapshot,
    pub wal_entries: Vec<LogEntry>,
    /// Strictly public verifying keys (32-byte Ed25519 public keys in 64-char hex)
    pub public_manifest_verifying_keys: BTreeMap<String, String>,
}
```

### Safety by Construction
- `StrictMetadataAllowlist` only accepts public verifying keys (`pubkey_hex.len() == 64` validated with `decode_hex_exact::<32>`).
- No private key struct, seed, or secret material exists in the type definition.
- Creating a backup bundle via `DisasterRecoveryBackupBundle::create_from_allowlist` guarantees that private keys cannot be included, eliminating leakage by construction.

## 3. Escrow Envelope Encryption & Offline Revocation Realities
Private keys destined for emergency escrow are isolated into [`KeyRecoveryPackage`](file:///home/rade/Project/Jobs/fsp/src/recovery/disaster_recovery.rs):
- AES-256-GCM envelope encryption.
- Unique nonce derived from principal ID, key version, and payload length.
- 16-byte Poly1305 authentication tag. Any tampering causes immediate decryption abort.

### The Offline Revocation Reality
An important operational reality is documented and enforced:
> **Operational Finding:**
> Offline storage nodes cannot verify cryptographic key revocation without receiving updated revocation evidence (CRL or updated Trust Store).
> Therefore, physical access controls and isolated networks must be preserved during disaster recovery staging until revocation manifests are distributed.

Verified by tests:
- [`test_19_key_package_invalid_authentication_tag_rejected`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L1062-L1085)
- [`test_20_raw_binary_secret_exclusion_via_strict_allowlist`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L1087-L1130)
- [`test_21_key_rotation_and_revocation_semantics`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L1132-L1170)
- [`test_22_offline_revocation_limitations_demonstrated`](file:///home/rade/Project/Jobs/fsp/tests/phase5_3_validation.rs#L1172-L1195)
