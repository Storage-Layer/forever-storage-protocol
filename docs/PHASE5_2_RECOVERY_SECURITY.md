# Phase 5.2 Recovery Security Architecture

## 1. Overview
In Forever Storage Protocol (FSP), disaster recovery operations must never bypass cryptographic access controls, enable unverified state rollbacks, leak secret material into backups, or allow stale leaders to mutate storage nodes. Phase 5.2 introduces a mathematically sound, tamper-evident security model for all offline and online recovery procedures.

## 2. Threat Model & Mitigations
- **Threat 1: Unauthorized Metadata Rollback**
  - *Risk:* An operator or attacker uses a force flag to overwrite committed coordinator logs with an older snapshot, causing silent loss of committed objects.
  - *Mitigation:* Mode A (`SafeOnline`) strictly rejects any bundle with `bundle.commit_index < existing_commit`. Mode B (`IsolatedRebootstrap`) requires an out-of-band cryptographic authorization token signed by the DR authority, advances the epoch by $+100$, and records unforgeable provenance.
- **Threat 2: Plaintext Private Key Exposure**
  - *Risk:* Backup bundles inadvertently serialize private signing keys or TLS certificate keys.
  - *Mitigation:* `DisasterRecoveryBackupBundle::scan_for_plaintext_secrets` inspects all bundle fields for PEM and Ed25519 seed headers, refusing creation and verification if detected. Key escrow uses authenticated `KeyRecoveryPackage` envelope encryption.
- **Threat 3: Stale Leader Re-Election after Disaster Recovery**
  - *Risk:* A partitioned coordinator from before the crash attempts to execute destructive mutating RPCs against storage nodes.
  - *Mitigation:* Storage nodes maintain an immutable `epoch.rec` watermark. Rebootstrap advances the cluster epoch high-water mark, causing storage nodes to reject stale RPCs with `RpcError::Fenced`.
- **Threat 4: Premature Shard Deletion During Repair**
  - *Risk:* Garbage collection deletes historical generations or uncommitted shards, reducing surviving shard counts below $k=12$.
  - *Mitigation:* `NodeEpochFencingTracker::audit_gc_eligibility` audits reachability across active generations, legal holds, retained version sets, checkpoints, and surviving redundancy thresholds.

## 3. Verification Evidence
All mitigations are verified by automated tests in [phase5_2_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_2_validation.rs).
