# Phase 5.2 Security Review & Threat Remediation Findings

## 1. Executive Summary
Phase 5.2 audited the disaster recovery, garbage collection, and key custody subsystems of FSP. Four security risks were identified and comprehensively remediated:

### Finding 1: Unrestricted Force Override in Metadata Restore (High Severity)
- **Problem:** In Phase 5.1, passing `force_rollback = true` bypassed commit index checks unconditionally without recording operator provenance or fencing stale leaders.
- **Remediation:** Replaced with a dual-mode restore architecture. Unrestricted force overrides were deleted. `RestoreMode::IsolatedRebootstrap` strictly requires signed operator authorization (`DisasterRebootstrapAuthorization`), advances the epoch high-water mark by $+100$, and syncs an unforgeable `recovery_provenance.rec` record.
- **Verification:** Verified by `test_01_online_force_restore_rejection` and `test_02_isolated_rebootstrap_authorization_success`.

### Finding 2: Plaintext Key Material Risk in Backups (High Severity)
- **Problem:** Serialized metadata bundles had no safeguards against containing unencrypted private keys.
- **Remediation:** Enforced that backups only reference public keys. Implemented `scan_for_plaintext_secrets` to inspect all fields for private key headers. Introduced `KeyRecoveryPackage` with envelope encryption (AES-256-GCM) and cryptographic revocation.
- **Verification:** Verified by `test_05_plaintext_private_key_leakage_detection` and `test_06_signing_key_rotation_and_revocation`.

### Finding 3: Recovery Freshness Ambiguity (Medium Severity)
- **Problem:** Objects matching local node checkpoints were labeled `AuthenticLatestCommitted`, conflating local durability with global consensus certainty.
- **Remediation:** Introduced `QuorumCertifiedCheckpoint` requiring majority coordinator signatures. `AuthenticGlobalLatest` is returned only when supported by quorum certification; otherwise `AuthenticLocallyCurrent` or `AuthenticHistoricalVersion` is reported.
- **Verification:** Verified by `test_08_unverifiable_recovery_freshness_classification` and `test_10_quorum_certified_current_manifest_recovery`.

### Finding 4: Unsafe GC Below Erasure-Coding Threshold (High Severity)
- **Problem:** Shard deletions evaluated only generation watermarks without checking surviving shard redundancy or reachability.
- **Remediation:** Implemented `NodeEpochFencingTracker::audit_gc_eligibility` enforcing legal holds, in-flight transactions, active committed generations, retained historical versions, recovery checkpoints, and a minimum redundancy floor ($k=12$).
- **Verification:** Verified by `test_11` through `test_16`.

## 2. Production Readiness Status
**NOT ESTABLISHED.** FSP is an academic distributed preservation protocol. Physical multi-datacenter bare-metal execution and multi-week continuous soaking have not been performed.
