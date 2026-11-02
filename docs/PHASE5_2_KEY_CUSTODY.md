# Phase 5.2 Cryptographic Key Custody & Escrow Architecture

## 1. Problem Statement
Distributed storage backups frequently suffer from catastrophic security breaches when private keys, master signing credentials, or unencrypted authentication tokens are included in database snapshots or metadata dumps. Conversely, losing all private keys in a physical disaster prevents recovering signed manifests or capability tokens.

## 2. Invariants & Implementation
FSP Phase 5.2 establishes two complementary mechanisms:

### A. Plaintext Secret Scanner (`scan_for_plaintext_secrets`)
Every `DisasterRecoveryBackupBundle` is subjected to a deep plaintext secret scanner before serialization, save, or verification.
- **Forbidden Patterns:**
  - `BEGIN PRIVATE KEY`, `BEGIN RSA PRIVATE KEY`, `BEGIN EC PRIVATE KEY`, `BEGIN OPENSSH PRIVATE KEY`, `BEGIN ENCRYPTED PRIVATE KEY`
  - `ED25519 PRIVATE KEY`, `PRIVATE KEY`, `SECRET KEY`, `PRIVATE_KEY`, `SECRET_KEY`
- **Field Auditing:**
  - `operator_id`: must not contain private key fragments.
  - `manifest_signers`: verified to contain only public keys (length $\ge 64$ hex or public identifiers).
  - `snapshot`: full serialized representation scanned.
  - `wal_entries`: each serialized log entry scanned.
- If any forbidden pattern is detected, the bundle creation/restore aborts immediately with `DisasterRecoveryStatus::SecretMaterialDetected`.

### B. Envelope-Encrypted Key Escrow (`KeyRecoveryPackage`)
For disaster recovery scenarios requiring key recovery, keys are encapsulated using standard authenticated envelope encryption:
- **Cipher:** AES-256-GCM with associated authenticated data (AAD).
- **Key Derivation:** SHA-256 of master passphrase or key encryption key.
- **Escrow Authorization:** The escrow agent signs the canonical metadata message:
  ```
  FSP-KEY-ESCROW-V1
  principal=<principal_id>
  version=<key_version>
  algo=AES-256-GCM
  ciphertext=<hex>
  nonce=<hex>
  tag=<hex>
  revoked=<bool>
  ```
- **Revocation:** Key packages support explicit revocation (`revoke()`). Once revoked, `decrypt_key()` strictly rejects decryption attempts.

## 3. Test Verification
Verified by:
- `test_05_plaintext_private_key_leakage_detection`
- `test_06_signing_key_rotation_and_revocation`
