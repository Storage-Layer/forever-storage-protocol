# Phase 5.2 Dual-Mode Restore Architecture

## 1. Architectural Principles
In Phase 5.1, `execute_disaster_recovery_restore` accepted a boolean `force_rollback` parameter. If set, it overwrote snapshots and WAL journals unconditionally, creating a dangerous loophole. Phase 5.2 completely eliminates unrestricted force overrides by establishing a strict dual-mode architecture:

```
                      Disaster Recovery Bundle Received
                                      |
                     [scan_for_plaintext_secrets()]
                                      |
                       [Verify Signature & Digest]
                                      |
                    +-----------------+-----------------+
                    |                                   |
         RestoreMode::SafeOnline             RestoreMode::IsolatedRebootstrap
                    |                                   |
     [existing_commit <= bundle_commit?]     [Verify DisasterRebootstrapAuth]
                    |                                   |
           Yes: Restore State                [Advance Epoch: Max(Term, Epoch) + 100]
           No:  RollbackRejected                        |
                                             [Write recovery_provenance.rec]
                                                        |
                                             [Atomic Snapshot & WAL Restore]
```

## 2. Mode Specifications

### Mode A: Safe Online Restore (`RestoreMode::SafeOnline`)
- Used for routine metadata restoration while the cluster or surviving quorum is active.
- Verifies Ed25519 bundle signature against `operator_key`.
- Invariant: `existing_commit <= bundle.commit_index`. If local commit is higher, restore aborts with `DisasterRecoveryStatus::RollbackRejected`.
- Invariant: `bundle.coordinator_term >= existing_epoch`. If lower, aborts with `DisasterRecoveryStatus::RebootstrapEpochFencingFailed`.

### Mode B: Isolated Disaster Rebootstrap (`RestoreMode::IsolatedRebootstrap`)
- Used exclusively when the consensus quorum is permanently lost and the node is isolated in maintenance mode.
- Requires explicit `DisasterRebootstrapAuthorization`:
  - `operator_id`: must match bundle operator.
  - `recovery_incarnation_id`: non-empty unique identifier.
  - `authorization_timestamp_unix_secs`: authorization issue time.
  - `justification`: audit explanation.
  - `signature_hex`: Ed25519 signature over canonical signing bytes.
- Fencing Protection: sets `restored_epoch = max(coordinator_term, existing_epoch) + 100`.
- Provenance Durability: writes `recovery_provenance.rec` containing operator ID, incarnation ID, previous commit index, restored commit index, old epoch, new epoch, and operator signature.

## 3. Test Verification
Verified by:
- `test_01_online_force_restore_rejection`
- `test_02_isolated_rebootstrap_authorization_success`
- `test_03_stale_leadership_fenced_after_restore`
- `test_07_trusted_checkpoint_rollback_rejection`
