# Phase 5.2 Recovery Provenance Architecture

## 1. Requirement & Rationale
When an isolated disaster rebootstrap is performed, the normal consensus chain of custody is broken because majority voter quorums are unreachable. To maintain regulatory auditability, prevent repudiation, and track state transitions across catastrophic failures, FSP requires an immutable on-disk recovery provenance record.

## 2. Format Specification
The provenance record is written atomically to `<target_dir>/recovery_provenance.rec` and fsync'd before any snapshot or WAL state is committed:

```
RECOVERY_PROVENANCE_V1
INCARNATION_ID=<unique_operator_assigned_uuid_or_hash>
OPERATOR_ID=<dr_operator_identity>
TIMESTAMP_UNIX_SECS=<timestamp>
JUSTIFICATION=<mandatory_audit_reason>
PREVIOUS_COMMIT_INDEX=<prior_local_commit_watermark>
RESTORED_COMMIT_INDEX=<bundle_restored_commit_watermark>
PREVIOUS_EPOCH=<prior_local_highest_seen_epoch>
RESTORED_EPOCH=<advanced_fencing_epoch>
RESTORE_MODE=ISOLATED_REBOOTSTRAP
AUTHORIZATION_SIGNATURE=<ed25519_hex_signature>
```

## 3. Crash Invariants
- `recovery_provenance.rec` is written with atomic temporary file swapping and directory sync (`sync_directory`).
- Reopening the storage engine or replicated log manager following SIGKILL termination preserves and validates the provenance file.
- Verified in `test_19_coordinator_sigkill_recovery_provenance_preserved`.
