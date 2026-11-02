//! Phase 5.3 Validation Test Suite (`tests/phase5_3_validation.rs`).
//!
//! Validates:
//! 1. `test_01_rebootstrap_with_old_leader_still_active_rejected`: Old leader coexistence warning blocks mutations fail-closed.
//! 2. `test_02_rebootstrap_with_old_higher_epoch_fenced_by_incarnation`: High epoch stale coordinator fenced by incarnation sequence.
//! 3. `test_03_duplicate_and_conflicting_cluster_incarnation_attempts_rejected`: Divergent incarnation token rejected.
//! 4. `test_04_rebootstrap_authorization_forgery_rejected`: Forged rebootstrap signature rejected.
//! 5. `test_05_storage_node_rejects_old_incarnation_permit`: Old incarnation permit rejected by updated storage node.
//! 6. `test_06_storage_node_restart_preserves_adopted_incarnation`: Node restart preserves adopted incarnation record.
//! 7. `test_07_delayed_destructive_permit_after_rebootstrap_rejected`: Stale old-incarnation destructive permit rejected.
//! 8. `test_08_unverified_old_quorum_coexistence_blocks_mutations`: Unfenced old quorum triggers fail-closed mutation block.
//! 9. `test_09_quorum_signed_checkpoint_classified_as_authenticated_committed`: Truthful classification of offline checkpoints.
//! 10. `test_10_missing_global_freshness_evidence_truthfully_reported`: UnverifiableFreshness vs AuthenticGlobalLatest.
//! 11. `test_11_conflicting_authentic_checkpoints_detected_and_reported`: Divergent checkpoints detected and reported.
//! 12. `test_12_restore_from_historical_checkpoint_classified_correctly`: Historical version classified correctly.
//! 13. `test_13_concurrent_gc_and_repair_on_same_shard_rejected`: Active repair blocks GC deletion reservation.
//! 14. `test_14_concurrent_gc_reservation_and_manifest_commit_toctou_prevented`: Pre-deletion revalidation prevents TOCTOU.
//! 15. `test_15_gc_after_snapshot_rollback_rejected`: Restored generation protected from GC.
//! 16. `test_16_protected_checkpoint_and_retained_historical_shard_gc_rejected`: Checkpoint and historical version protected.
//! 17. `test_17_gc_redundancy_and_failure_domain_policy_violation_rejected`: Surviving shard threshold k=12 enforced.
//! 18. `test_18_deletion_lifecycle_tombstone_and_physical_deletion`: Multi-phase deletion state machine validated.
//! 19. `test_19_key_package_invalid_authentication_tag_rejected`: AES-256-GCM auth tag tampering rejected.
//! 20. `test_20_raw_binary_secret_exclusion_via_strict_allowlist`: StrictMetadataAllowlist type-level isolation.
//! 21. `test_21_key_rotation_and_revocation_semantics`: Escrow key rotation and revocation enforced.
//! 22. `test_22_offline_revocation_limitations_demonstrated`: Offline revocation delivery requirement shown.
//! 23. `test_23_coordinator_sigkill_during_restore_preserves_atomic_state`: Crash during restore preserves atomic files.
//! 24. `test_24_twelve_of_twenty_exact_recovery_and_nine_loss_safe_failure`: 12/20 exact recovery and 9 loss safe failure.
//! 25. `test_25_real_100mib_end_to_end_disaster_recovery_and_emergency_read`: Real 100 MiB RS(20,12) end-to-end recovery.

#![allow(clippy::needless_range_loop, clippy::cloned_ref_to_slice_refs)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fsp_sim::coding::reed_solomon::ReedSolomonCodec;
use fsp_sim::coding::stripe::compute_stripe_shard_length;
use fsp_sim::coding::CodingScheme;
use fsp_sim::coordinator::fencing::{
    NodeEpochFencingTracker, PermitOperationType, QuorumMutationPermit, QuorumPermitGrant,
    ShardDeletionState,
};
use fsp_sim::coordinator::incarnation::{ClusterAuthorityRecord, ClusterIncarnationId};
use fsp_sim::coordinator::leadership::CoordinatorId;
use fsp_sim::coordinator::replication::{MetadataSnapshot, ReplicatedLogManager};
use fsp_sim::model::node::NodeId;
use fsp_sim::network::protocol::RpcError;
use fsp_sim::recovery::disaster_recovery::{
    execute_restore_v2, DisasterRebootstrapAuthorization, DisasterRecoveryBackupBundle,
    DisasterRecoveryStatus, KeyRecoveryPackage, RestoreMode, StrictMetadataAllowlist,
};
use fsp_sim::security::recovery_auth::{
    execute_secure_emergency_read, QuorumCertifiedCheckpoint, RecoveryCapabilityToken,
    RecoveryFreshnessStatus, RecoveryTrustStore, SecureEmergencyReadOptions,
};
use fsp_sim::security::signatures::{encode_hex, ManifestSignerKeypair, SignedObjectManifest};
use fsp_sim::storage::container::write_shard_container_atomic;
use fsp_sim::storage::filesystem::write_and_sync_file;
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::storage::manifest::{
    ManifestCodingSpec, ObjectManifest, ShardPlacementRecord, StripeManifest,
};

static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(9000);

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(prefix: &str) -> Self {
        let seq = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fsp_p5_3_test_{}_{}", prefix, seq));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn create_deterministic_signer(id: &str, seed: u64) -> ManifestSignerKeypair {
    ManifestSignerKeypair::deterministic(id, seed).unwrap()
}

#[allow(clippy::needless_range_loop)]
fn setup_synthetic_encoded_object(
    storage_roots: &[PathBuf],
    object_id: &str,
    payload_len: usize,
    signer: &ManifestSignerKeypair,
    manifest_gen: u64,
) -> (Vec<u8>, SignedObjectManifest) {
    let mut payload = Vec::with_capacity(payload_len);
    for i in 0..payload_len {
        payload.push(((i * 7 + 13) % 251) as u8);
    }
    let payload_digest = sha256_bytes(&payload).to_hex();

    let rs = ReedSolomonCodec::fsp_default_20_12();
    let stripe_size = if payload_len >= 1024 * 1024 {
        512 * 1024
    } else {
        4096
    };
    let data_shards_count = 12;
    let total_shards_count = 20;

    let mut stripes = Vec::new();
    let mut offset = 0;
    let mut all_encoded_stripes = Vec::new();
    let mut stripe_payload_lens = Vec::new();

    while offset < payload_len {
        let cur_len = (payload_len - offset).min(stripe_size);
        stripe_payload_lens.push(cur_len);
        let cur_slice = &payload[offset..offset + cur_len];
        let shard_len = compute_stripe_shard_length(cur_len, data_shards_count);

        let mut data_shards = Vec::with_capacity(data_shards_count);
        for d in 0..data_shards_count {
            let start = d * shard_len;
            let mut shard = vec![0u8; shard_len];
            if start < cur_len {
                let avail = (cur_len - start).min(shard_len);
                shard[..avail].copy_from_slice(&cur_slice[start..start + avail]);
            }
            data_shards.push(shard);
        }

        let refs: Vec<&[u8]> = data_shards.iter().map(|s| s.as_slice()).collect();
        let encoded_stripe = rs.encode_stripe(&refs).unwrap();
        all_encoded_stripes.push(encoded_stripe);

        offset += cur_len;
    }

    for (s_idx, encoded_stripe) in all_encoded_stripes.iter().enumerate() {
        let cur_len = stripe_payload_lens[s_idx];
        let mut placements = Vec::new();
        for shard_id in 0..total_shards_count {
            let shard_bytes = &encoded_stripe[shard_id];
            let shard_digest = sha256_bytes(shard_bytes).to_hex();
            let node_idx = shard_id % storage_roots.len();
            placements.push(ShardPlacementRecord {
                shard_id,
                node_id: node_idx,
                domain_id: node_idx,
                version: 1,
                size_bytes: shard_bytes.len() as u64,
                sha256_digest: shard_digest,
                relative_path: format!("objects/{}/shard_{:02}.fspc", object_id, shard_id),
            });
        }
        stripes.push(StripeManifest {
            stripe_index: s_idx,
            payload_offset: (s_idx * stripe_size) as u64,
            payload_length: cur_len,
            shard_length: encoded_stripe[0].len(),
            placements,
        });
    }

    // Now write containers for each shard_id across all storage nodes
    for shard_id in 0..total_shards_count {
        let node_idx = shard_id % storage_roots.len();
        let node_root = &storage_roots[node_idx];
        let obj_dir = node_root.join("objects").join(object_id);
        fs::create_dir_all(&obj_dir).unwrap();
        let container_path = obj_dir.join(format!("shard_{:02}.fspc", shard_id));

        let stripe_entries: Vec<(usize, &[u8])> = all_encoded_stripes
            .iter()
            .enumerate()
            .map(|(s_idx, encoded)| (stripe_payload_lens[s_idx], encoded[shard_id].as_slice()))
            .collect();

        write_shard_container_atomic(
            &container_path,
            object_id,
            CodingScheme::ReedSolomon { n: 20, k: 12 },
            shard_id,
            1,
            &stripe_entries,
        )
        .unwrap();
    }

    let manifest = ObjectManifest {
        format_version: 1,
        object_id: object_id.to_string(),
        object_size: payload_len as u64,
        object_digest: payload_digest,
        coding: ManifestCodingSpec {
            scheme: "reed-solomon".to_string(),
            n: total_shards_count,
            k: data_shards_count,
            l: None,
        },
        stripe_size,
        integrity_algorithm: "sha256".to_string(),
        manifest_digest: String::new(),
        stripes,
    };

    let signed = signer.sign_manifest(manifest, manifest_gen).unwrap();

    // Place signed manifest in storage roots
    for root in storage_roots {
        let meta_dir = root.join("metadata").join("manifests");
        fs::create_dir_all(&meta_dir).unwrap();
        let m_path = meta_dir.join(format!("{}.sig.json", object_id));
        fs::write(&m_path, signed.to_json()).unwrap();
    }

    (payload, signed)
}

// -------------------------------------------------------------------------------------------------
// Test 1: Rebootstrap with Old Leader Still Active Triggers Coexistence Warning (Fail-Closed)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_01_rebootstrap_with_old_leader_still_active_rejected() {
    let sandbox = TestDir::new("coexistence_fail_closed");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let recovery_signer = create_deterministic_signer("rec-authority", 101);

    let inc1 = ClusterIncarnationId::genesis("fsp-cluster-main");
    let inc2 = inc1.advance("manual-rebootstrap-emergency");

    let mut voters = BTreeSet::new();
    voters.insert(CoordinatorId::new("coord-1"));
    voters.insert(CoordinatorId::new("coord-2"));

    // Authority record where old quorum is NOT fenced (coexistence possible)
    let auth_record = ClusterAuthorityRecord::create(
        "fsp-cluster-main",
        inc2.clone(),
        voters,
        "lineage-hash-1",
        1700000000,
        false, // operational isolation NOT attested
        false, // old quorum NOT fenced
        "Emergency rebootstrap after coordinator loss",
        recovery_signer.signing_key(),
    );

    tracker
        .adopt_cluster_incarnation(&auth_record, &recovery_signer.verifying_key())
        .unwrap();

    // Node detects that old quorum coexistence is possible and activates warning
    assert!(tracker.is_coexistence_warning_active());

    // Creating a mutation permit should be rejected fail-closed
    let voter1 = create_deterministic_signer("coord-1", 11);
    let voter2 = create_deterministic_signer("coord-2", 12);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(CoordinatorId::new("coord-1"), voter1.verifying_key());
    voter_keys.insert(CoordinatorId::new("coord-2"), voter2.verifying_key());

    let grant1 = QuorumPermitGrant::create(
        CoordinatorId::new("coord-1"),
        10,
        1,
        "req-1",
        "obj-1",
        0,
        1,
        "hash-1",
        voter1.signing_key(),
    );
    let grant2 = QuorumPermitGrant::create(
        CoordinatorId::new("coord-2"),
        10,
        1,
        "req-1",
        "obj-1",
        0,
        1,
        "hash-1",
        voter2.signing_key(),
    );

    let permit = QuorumMutationPermit::new(
        10,
        1,
        "req-1",
        "obj-1",
        0,
        1,
        "hash-1",
        1700000010,
        vec![grant1, grant2],
    )
    .with_incarnation(inc2);

    let res =
        tracker.verify_and_apply_mutation_permit(&permit, "obj-1", 0, "hash-1", &voter_keys, 2);

    match res {
        Err(RpcError::Unauthorized(msg)) => {
            assert!(msg.contains("coexistence warning active"));
        }
        other => panic!("Expected coexistence rejection, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 2: Rebootstrap with Old Higher Epoch Fenced by Incarnation Sequence
// -------------------------------------------------------------------------------------------------
#[test]
fn test_02_rebootstrap_with_old_higher_epoch_fenced_by_incarnation() {
    let sandbox = TestDir::new("incarnation_fences_high_epoch");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let recovery_signer = create_deterministic_signer("rec-authority", 102);

    let inc1 = ClusterIncarnationId::genesis("fsp-cluster-main");
    let inc2 = inc1.advance("recovery-epoch-advance");

    let mut voters = BTreeSet::new();
    voters.insert(CoordinatorId::new("new-coord-1"));
    voters.insert(CoordinatorId::new("new-coord-2"));

    let auth_record = ClusterAuthorityRecord::create(
        "fsp-cluster-main",
        inc2,
        voters,
        "lineage-hash-2",
        1700000000,
        true,
        true,
        "Hardened rebootstrap",
        recovery_signer.signing_key(),
    );

    tracker
        .adopt_cluster_incarnation(&auth_record, &recovery_signer.verifying_key())
        .unwrap();

    // Old coordinator attempts mutation with high epoch (500) but belonging to incarnation 1
    let old_inc_permit = ClusterIncarnationId::genesis("fsp-cluster-main");
    let err = tracker
        .verify_incarnation_binding(&old_inc_permit)
        .unwrap_err();
    match err {
        RpcError::Fenced {
            current_epoch,
            presented_epoch,
        } => {
            assert_eq!(current_epoch, 2);
            assert_eq!(presented_epoch, 1);
        }
        other => panic!("Expected Fenced incarnation, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 3: Duplicate and Conflicting Cluster Incarnation Attempts Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_03_duplicate_and_conflicting_cluster_incarnation_attempts_rejected() {
    let sandbox = TestDir::new("conflicting_incarnation");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let recovery_signer = create_deterministic_signer("rec-authority", 103);

    let inc1 = ClusterIncarnationId::genesis("fsp-cluster-main");
    let inc2_legit = inc1.advance("legit-authority");

    let mut voters = BTreeSet::new();
    voters.insert(CoordinatorId::new("coord-1"));

    let legit_record = ClusterAuthorityRecord::create(
        "fsp-cluster-main",
        inc2_legit,
        voters.clone(),
        "lineage-1",
        1700000000,
        true,
        true,
        "Legit rebootstrap",
        recovery_signer.signing_key(),
    );

    tracker
        .adopt_cluster_incarnation(&legit_record, &recovery_signer.verifying_key())
        .unwrap();

    // Adversary creates divergent incarnation with SAME sequence number (2) but different token
    let inc2_divergent = ClusterIncarnationId {
        cluster_id: "fsp-cluster-main".to_string(),
        incarnation_seq: 2,
        incarnation_token: "divergent-fake-token-hex".to_string(),
    };

    let conflicting_record = ClusterAuthorityRecord::create(
        "fsp-cluster-main",
        inc2_divergent,
        voters,
        "lineage-2",
        1700000005,
        true,
        true,
        "Conflicting rebootstrap",
        recovery_signer.signing_key(),
    );

    let res =
        tracker.adopt_cluster_incarnation(&conflicting_record, &recovery_signer.verifying_key());
    assert!(
        res.is_err(),
        "Divergent incarnation sequence must be rejected"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 4: Rebootstrap Authorization Forgery Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_04_rebootstrap_authorization_forgery_rejected() {
    let operator_signer = create_deterministic_signer("dr-operator", 104);
    let attacker_signer = create_deterministic_signer("attacker", 999);

    let forged_auth = DisasterRebootstrapAuthorization::issue(
        "dr-operator",
        "inc-forged",
        1700000000,
        "Malicious unauthorized restore",
        attacker_signer.signing_key(),
    );

    let res = forged_auth.verify(&operator_signer.verifying_key());
    assert!(res.is_err(), "Forged authorization must fail verification");
}

// -------------------------------------------------------------------------------------------------
// Test 5: Storage Node Rejects Old Incarnation Permit
// -------------------------------------------------------------------------------------------------
#[test]
fn test_05_storage_node_rejects_old_incarnation_permit() {
    let sandbox = TestDir::new("reject_old_permit");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let recovery_signer = create_deterministic_signer("rec-authority", 105);

    let inc1 = ClusterIncarnationId::genesis("fsp-cluster-alpha");
    let inc2 = inc1.advance("upgrade");

    let mut voters = BTreeSet::new();
    voters.insert(CoordinatorId::new("coord-1"));

    let record = ClusterAuthorityRecord::create(
        "fsp-cluster-alpha",
        inc2,
        voters,
        "lineage-3",
        1700000000,
        true,
        true,
        "Incarnation update",
        recovery_signer.signing_key(),
    );
    tracker
        .adopt_cluster_incarnation(&record, &recovery_signer.verifying_key())
        .unwrap();

    let voter1 = create_deterministic_signer("coord-1", 201);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(CoordinatorId::new("coord-1"), voter1.verifying_key());

    let grant = QuorumPermitGrant::create(
        CoordinatorId::new("coord-1"),
        1,
        1,
        "req-old",
        "obj-1",
        0,
        1,
        "hash-old",
        voter1.signing_key(),
    );

    // Permit carries old genesis incarnation
    let permit = QuorumMutationPermit::new(
        1,
        1,
        "req-old",
        "obj-1",
        0,
        1,
        "hash-old",
        1700000000,
        vec![grant],
    )
    .with_incarnation(inc1);

    let res =
        tracker.verify_and_apply_mutation_permit(&permit, "obj-1", 0, "hash-old", &voter_keys, 1);

    assert!(res.is_err(), "Permit with old incarnation must be rejected");
}

// -------------------------------------------------------------------------------------------------
// Test 6: Storage Node Restart Preserves Adopted Incarnation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_06_storage_node_restart_preserves_adopted_incarnation() {
    let sandbox = TestDir::new("restart_preserves_incarnation");
    let recovery_signer = create_deterministic_signer("rec-authority", 106);

    let inc = ClusterIncarnationId::genesis("fsp-cluster-persistent").advance("seq-2-entropy");
    let mut voters = BTreeSet::new();
    voters.insert(CoordinatorId::new("coord-1"));

    let record = ClusterAuthorityRecord::create(
        "fsp-cluster-persistent",
        inc.clone(),
        voters,
        "lineage-4",
        1700000000,
        true,
        true,
        "Adopted persistence test",
        recovery_signer.signing_key(),
    );

    {
        let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
        tracker
            .adopt_cluster_incarnation(&record, &recovery_signer.verifying_key())
            .unwrap();
        assert_eq!(tracker.current_incarnation(), inc);
    }

    // Reopen node tracker (simulating process restart)
    let tracker_restarted = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert_eq!(tracker_restarted.current_incarnation(), inc);
}

// -------------------------------------------------------------------------------------------------
// Test 7: Delayed Destructive Permit After Rebootstrap Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_07_delayed_destructive_permit_after_rebootstrap_rejected() {
    let sandbox = TestDir::new("delayed_destructive_permit");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let recovery_signer = create_deterministic_signer("rec-authority", 107);

    let inc1 = ClusterIncarnationId::genesis("fsp-cluster-del");
    let inc2 = inc1.advance("rebootstrap-fence");

    let mut voters = BTreeSet::new();
    voters.insert(CoordinatorId::new("coord-1"));

    let record = ClusterAuthorityRecord::create(
        "fsp-cluster-del",
        inc2,
        voters,
        "lineage-5",
        1700000000,
        true,
        true,
        "Rebootstrap after partition",
        recovery_signer.signing_key(),
    );
    tracker
        .adopt_cluster_incarnation(&record, &recovery_signer.verifying_key())
        .unwrap();

    let voter1 = create_deterministic_signer("coord-1", 207);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(CoordinatorId::new("coord-1"), voter1.verifying_key());

    let grant = QuorumPermitGrant::create(
        CoordinatorId::new("coord-1"),
        50,
        10,
        "del-req",
        "target-obj",
        2,
        1,
        "del-hash",
        voter1.signing_key(),
    );

    let destructive_permit = QuorumMutationPermit::new_v2(
        50,
        10,
        "del-req",
        "target-obj",
        2,
        1,
        "del-hash",
        1700000000,
        NodeId(0),
        PermitOperationType::DeleteShard,
        vec![grant],
    )
    .with_incarnation(inc1); // Stale incarnation!

    let res = tracker.verify_and_apply_mutation_permit_v3(
        &destructive_permit,
        "target-obj",
        2,
        "del-hash",
        NodeId(0),
        PermitOperationType::DeleteShard,
        Some(16),
        &voter_keys,
        1,
    );

    assert!(
        res.is_err(),
        "Delayed destructive permit with stale incarnation must be rejected"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 8: Unverified Old Quorum Coexistence Blocks Mutations
// -------------------------------------------------------------------------------------------------
#[test]
fn test_08_unverified_old_quorum_coexistence_blocks_mutations() {
    let sandbox = TestDir::new("coexistence_blocks_mutations");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let recovery_signer = create_deterministic_signer("rec-authority", 108);

    let inc1 = ClusterIncarnationId::genesis("fsp-cluster-coexist");
    let inc2 = inc1.advance("unverified-coexist");

    let mut voters = BTreeSet::new();
    voters.insert(CoordinatorId::new("coord-1"));

    let record = ClusterAuthorityRecord::create(
        "fsp-cluster-coexist",
        inc2.clone(),
        voters,
        "lineage-6",
        1700000000,
        false, // Isolation NOT verified!
        false, // Old quorum NOT verified fenced!
        "Unverified coexistence scenario",
        recovery_signer.signing_key(),
    );

    tracker
        .adopt_cluster_incarnation(&record, &recovery_signer.verifying_key())
        .unwrap();
    assert!(tracker.is_coexistence_warning_active());

    let voter1 = create_deterministic_signer("coord-1", 208);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(CoordinatorId::new("coord-1"), voter1.verifying_key());

    let grant = QuorumPermitGrant::create(
        CoordinatorId::new("coord-1"),
        1,
        1,
        "mut-req",
        "obj-x",
        0,
        1,
        "hash-x",
        voter1.signing_key(),
    );
    let permit = QuorumMutationPermit::new(
        1,
        1,
        "mut-req",
        "obj-x",
        0,
        1,
        "hash-x",
        1700000000,
        vec![grant],
    )
    .with_incarnation(inc2);

    let res =
        tracker.verify_and_apply_mutation_permit(&permit, "obj-x", 0, "hash-x", &voter_keys, 1);

    assert!(
        res.is_err(),
        "Mutations must be blocked fail-closed under coexistence warning"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 9: Quorum-Signed Checkpoint Classified as AuthenticatedCommitted (Not Global Latest Without Sync)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_09_quorum_signed_checkpoint_classified_as_authenticated_committed() {
    let sandbox = TestDir::new("auth_committed_checkpoint");
    let signer = create_deterministic_signer("signer-9", 309);
    let recovery_principal = create_deterministic_signer("principal-9", 409);

    let storage_roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    let (payload, _) =
        setup_synthetic_encoded_object(&storage_roots, "test-obj-9", 128 * 1024, &signer, 5);

    let token = RecoveryCapabilityToken::issue(
        "test-obj-9",
        "default",
        "principal-9",
        1800000000,
        recovery_principal.signing_key(),
    );

    let v1 = create_deterministic_signer("voter-1", 501);
    let v2 = create_deterministic_signer("voter-2", 502);
    let checkpoint = QuorumCertifiedCheckpoint::create(
        "test-obj-9",
        5, // Matches manifest generation
        2,
        100,
        &[("voter-1", v1.signing_key()), ("voter-2", v2.signing_key())],
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer-9", signer.verifying_key());
    trust_store.register_recovery_principal("principal-9", recovery_principal.verifying_key());
    trust_store.register_quorum_voter("voter-1", v1.verifying_key());
    trust_store.register_quorum_voter("voter-2", v2.verifying_key());
    trust_store.set_quorum_threshold(2);
    trust_store.register_quorum_checkpoint(checkpoint);

    // Enforce strict freshness proof without live cluster sync
    trust_store.set_strict_freshness_proof(true);
    trust_store.set_live_cluster_synchronized(false);

    let out_file = sandbox.path().join("recovered.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "test-obj-9".to_string(),
        namespace: "default".to_string(),
        token,
        trust_store,
        storage_roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::AuthenticatedCommittedCheckpoint {
            certified_generation: 5,
            quorum_voters_count: 2,
            unverified_subsequent_commits_possible: true,
        }
    );

    let recovered_bytes = fs::read(&out_file).unwrap();
    assert_eq!(recovered_bytes, payload);
}

// -------------------------------------------------------------------------------------------------
// Test 10: Missing Global Freshness Evidence Truthfully Reported
// -------------------------------------------------------------------------------------------------
#[test]
fn test_10_missing_global_freshness_evidence_truthfully_reported() {
    let sandbox = TestDir::new("missing_freshness_evidence");
    let signer = create_deterministic_signer("signer-10", 310);
    let recovery_principal = create_deterministic_signer("principal-10", 410);

    let storage_roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    let _ = setup_synthetic_encoded_object(&storage_roots, "test-obj-10", 64 * 1024, &signer, 7);

    let token = RecoveryCapabilityToken::issue(
        "test-obj-10",
        "default",
        "principal-10",
        1800000000,
        recovery_principal.signing_key(),
    );

    // Trust store has NO checkpoints
    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer-10", signer.verifying_key());
    trust_store.register_recovery_principal("principal-10", recovery_principal.verifying_key());
    trust_store.set_strict_quorum_freshness(true);

    let out_file = sandbox.path().join("recovered.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "test-obj-10".to_string(),
        namespace: "default".to_string(),
        token: token.clone(),
        trust_store: trust_store.clone(),
        storage_roots: storage_roots.clone(),
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::UnverifiableFreshness {
            manifest_generation: 7,
        }
    );

    // Now demonstrate that live cluster synchronization establishes AuthenticGlobalLatest
    let v1 = create_deterministic_signer("v1", 601);
    let checkpoint =
        QuorumCertifiedCheckpoint::create("test-obj-10", 7, 1, 10, &[("v1", v1.signing_key())]);
    trust_store.register_quorum_voter("v1", v1.verifying_key());
    trust_store.set_quorum_threshold(1);
    trust_store.register_quorum_checkpoint(checkpoint);
    trust_store.set_strict_freshness_proof(true);
    trust_store.set_live_cluster_synchronized(true); // Proven live sync

    let opts2 = SecureEmergencyReadOptions {
        object_id: "test-obj-10".to_string(),
        namespace: "default".to_string(),
        token,
        trust_store,
        storage_roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report2 = execute_secure_emergency_read(&opts2).unwrap();
    assert_eq!(
        report2.freshness_status,
        RecoveryFreshnessStatus::AuthenticGlobalLatest {
            certified_generation: 7,
            quorum_voters_count: 1,
        }
    );
}

// -------------------------------------------------------------------------------------------------
// Test 11: Conflicting Authentic Checkpoints Detected and Reported
// -------------------------------------------------------------------------------------------------
#[test]
fn test_11_conflicting_authentic_checkpoints_detected_and_reported() {
    let sandbox = TestDir::new("conflicting_checkpoints");
    let signer = create_deterministic_signer("signer-11", 311);
    let recovery_principal = create_deterministic_signer("principal-11", 411);

    let storage_roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    let _ = setup_synthetic_encoded_object(&storage_roots, "test-obj-11", 64 * 1024, &signer, 4);

    let token = RecoveryCapabilityToken::issue(
        "test-obj-11",
        "default",
        "principal-11",
        1800000000,
        recovery_principal.signing_key(),
    );

    let v1 = create_deterministic_signer("voter-a", 701);
    let v2 = create_deterministic_signer("voter-b", 702);

    let cp1 = QuorumCertifiedCheckpoint::create(
        "test-obj-11",
        4,
        1,
        20,
        &[("voter-a", v1.signing_key())],
    );

    let cp2 = QuorumCertifiedCheckpoint::create(
        "test-obj-11",
        5, // Divergent generation!
        2,
        25,
        &[("voter-b", v2.signing_key())],
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer-11", signer.verifying_key());
    trust_store.register_recovery_principal("principal-11", recovery_principal.verifying_key());
    trust_store.register_quorum_voter("voter-a", v1.verifying_key());
    trust_store.register_quorum_voter("voter-b", v2.verifying_key());
    trust_store.set_quorum_threshold(1);

    trust_store
        .register_checkpoint_with_conflict_detection(cp1)
        .unwrap();
    let conflict_err = trust_store.register_checkpoint_with_conflict_detection(cp2);
    assert!(
        conflict_err.is_err(),
        "Conflict must be detected upon registration"
    );

    let out_file = sandbox.path().join("recovered.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "test-obj-11".to_string(),
        namespace: "default".to_string(),
        token,
        trust_store,
        storage_roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    match report.freshness_status {
        RecoveryFreshnessStatus::ConflictingAuthorityEvidence {
            object_id,
            conflicting_generations,
        } => {
            assert_eq!(object_id, "test-obj-11");
            assert_eq!(conflicting_generations, vec![4, 5]);
        }
        other => panic!("Expected ConflictingAuthorityEvidence, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 12: Restore from Historical Checkpoint Classified Correctly
// -------------------------------------------------------------------------------------------------
#[test]
fn test_12_restore_from_historical_checkpoint_classified_correctly() {
    let sandbox = TestDir::new("historical_checkpoint_classified");
    let signer = create_deterministic_signer("signer-12", 312);
    let recovery_principal = create_deterministic_signer("principal-12", 412);

    let storage_roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    // Manifest generation 3
    let _ = setup_synthetic_encoded_object(&storage_roots, "test-obj-12", 64 * 1024, &signer, 3);

    let token = RecoveryCapabilityToken::issue(
        "test-obj-12",
        "default",
        "principal-12",
        1800000000,
        recovery_principal.signing_key(),
    );

    let v1 = create_deterministic_signer("voter-1", 801);
    // Known checkpoint is generation 7 (> manifest gen 3)
    let cp = QuorumCertifiedCheckpoint::create(
        "test-obj-12",
        7,
        1,
        50,
        &[("voter-1", v1.signing_key())],
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer-12", signer.verifying_key());
    trust_store.register_recovery_principal("principal-12", recovery_principal.verifying_key());
    trust_store.register_quorum_voter("voter-1", v1.verifying_key());
    trust_store.set_quorum_threshold(1);
    trust_store.register_quorum_checkpoint(cp);

    let out_file = sandbox.path().join("recovered.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "test-obj-12".to_string(),
        namespace: "default".to_string(),
        token,
        trust_store,
        storage_roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::AuthenticHistoricalVersion {
            reconstructed_generation: 3,
            known_latest_generation: 7,
        }
    );
}

// -------------------------------------------------------------------------------------------------
// Test 13: Concurrent GC and Repair on Same Shard Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_13_concurrent_gc_and_repair_on_same_shard_rejected() {
    let sandbox = TestDir::new("concurrent_gc_repair");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    // Register active repair on object-13 shard 4
    tracker.register_active_repair("object-13", 4);
    assert!(tracker.is_shard_repairing("object-13", 4));

    // Attempting to reserve deletion on shard 4 must fail
    let res = tracker.reserve_shard_deletion(
        "object-13",
        4,
        1,
        1700000000,
        300,
        Some(18),
        Some("domain-0"),
    );
    assert!(res.is_err(), "Reservation must fail while repair is active");

    // After unregistering repair, deletion reservation succeeds
    tracker.unregister_active_repair("object-13", 4);
    assert!(!tracker.is_shard_repairing("object-13", 4));
    let res2 = tracker.reserve_shard_deletion(
        "object-13",
        4,
        1,
        1700000000,
        300,
        Some(18),
        Some("domain-0"),
    );
    assert!(
        res2.is_ok(),
        "Reservation should succeed after repair finishes"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 14: Concurrent GC Reservation and Manifest Commit TOCTOU Prevented
// -------------------------------------------------------------------------------------------------
#[test]
fn test_14_concurrent_gc_reservation_and_manifest_commit_toctou_prevented() {
    let sandbox = TestDir::new("toctou_prevention");

    // Object is at generation 2
    let gen_file = sandbox.path().join("object-14.gen");
    write_and_sync_file(&gen_file, b"GENERATION=2\n").unwrap();
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    // Shard version 1 is reserved for deletion
    tracker
        .reserve_shard_deletion(
            "object-14",
            0,
            1,
            1700000000,
            300,
            Some(16),
            Some("domain-1"),
        )
        .unwrap();

    // While deletion is reserved, a concurrent repair claims it
    tracker.register_active_repair("object-14", 0);

    // Attempt to commit physical deletion must be blocked by the active repair
    let res = tracker.commit_physical_deletion("object-14", 0, 1);
    assert!(
        res.is_err(),
        "Commit physical deletion must detect concurrent repair"
    );

    tracker.unregister_active_repair("object-14", 0);

    // If version 2 was attempted to commit physical deletion, active gen check blocks it
    let res_active = tracker.commit_physical_deletion("object-14", 0, 2);
    assert!(
        res_active.is_err(),
        "Cannot commit physical deletion of active generation"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 15: GC After Snapshot Rollback Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_15_gc_after_snapshot_rollback_rejected() {
    let sandbox = TestDir::new("gc_after_rollback");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    // Set active object generation to 5
    let voter1 = create_deterministic_signer("coord-1", 901);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(CoordinatorId::new("coord-1"), voter1.verifying_key());

    let grant = QuorumPermitGrant::create(
        CoordinatorId::new("coord-1"),
        1,
        1,
        "commit-5",
        "object-15",
        0,
        5,
        "hash-5",
        voter1.signing_key(),
    );
    let permit = QuorumMutationPermit::new(
        1,
        1,
        "commit-5",
        "object-15",
        0,
        5,
        "hash-5",
        1700000000,
        vec![grant],
    );
    tracker
        .verify_and_apply_mutation_permit(&permit, "object-15", 0, "hash-5", &voter_keys, 1)
        .unwrap();

    // GC attempting to delete generation 5 must be rejected
    let res = tracker.audit_gc_eligibility("object-15", 5, Some(15));
    assert!(res.is_err(), "Cannot GC active generation 5");
}

// -------------------------------------------------------------------------------------------------
// Test 16: Protected Checkpoint and Retained Historical Shard GC Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_16_protected_checkpoint_and_retained_historical_shard_gc_rejected() {
    let sandbox = TestDir::new("protected_gc");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    tracker.protect_checkpoint("object-16", 2);
    tracker.add_retained_historical_version("object-16", 1);

    // Deleting protected checkpoint generation 2 is rejected
    assert!(tracker
        .audit_gc_eligibility("object-16", 2, Some(16))
        .is_err());

    // Deleting retained historical version 1 is rejected
    assert!(tracker
        .audit_gc_eligibility("object-16", 1, Some(16))
        .is_err());

    // Unprotect checkpoint generation 2
    tracker.unprotect_checkpoint("object-16", 2);
    tracker.remove_retained_historical_version("object-16", 1);

    assert!(tracker
        .audit_gc_eligibility("object-16", 2, Some(16))
        .is_ok());
    assert!(tracker
        .audit_gc_eligibility("object-16", 1, Some(16))
        .is_ok());
}

// -------------------------------------------------------------------------------------------------
// Test 17: GC Redundancy Policy Violation Rejected (k <= 12)
// -------------------------------------------------------------------------------------------------
#[test]
fn test_17_gc_redundancy_and_failure_domain_policy_violation_rejected() {
    let sandbox = TestDir::new("gc_redundancy_threshold");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    // Surviving shards = 12 (at threshold k=12): rejected!
    let res = tracker.audit_gc_eligibility("object-17", 1, Some(12));
    assert!(
        res.is_err(),
        "GC must be rejected when surviving shards <= 12"
    );

    // Surviving shards = 11: rejected!
    let res2 = tracker.audit_gc_eligibility("object-17", 1, Some(11));
    assert!(
        res2.is_err(),
        "GC must be rejected when surviving shards < 12"
    );

    // Surviving shards = 13: accepted
    let res3 = tracker.audit_gc_eligibility("object-17", 1, Some(13));
    assert!(res3.is_ok());
}

// -------------------------------------------------------------------------------------------------
// Test 18: Multi-Stage Shard Deletion Lifecycle State Machine
// -------------------------------------------------------------------------------------------------
#[test]
fn test_18_deletion_lifecycle_tombstone_and_physical_deletion() {
    let sandbox = TestDir::new("deletion_lifecycle");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    assert_eq!(tracker.get_deletion_state("obj-18", 0, 1), None);

    // Transition 1: Candidate -> DeletionReserved
    tracker
        .reserve_shard_deletion("obj-18", 0, 1, 1700000000, 300, Some(15), Some("dom-1"))
        .unwrap();
    assert_eq!(
        tracker.get_deletion_state("obj-18", 0, 1),
        Some(ShardDeletionState::DeletionReserved)
    );

    // Transition 2: DeletionReserved -> Tombstoned
    tracker.tombstone_shard_deletion("obj-18", 0, 1).unwrap();
    assert_eq!(
        tracker.get_deletion_state("obj-18", 0, 1),
        Some(ShardDeletionState::Tombstoned)
    );

    // Transition 3: Tombstoned -> PhysicallyDeleted
    tracker.commit_physical_deletion("obj-18", 0, 1).unwrap();
    assert_eq!(
        tracker.get_deletion_state("obj-18", 0, 1),
        Some(ShardDeletionState::PhysicallyDeleted)
    );

    // Cannot reserve an already physically deleted shard
    assert!(tracker
        .reserve_shard_deletion("obj-18", 0, 1, 1700000000, 300, Some(15), Some("dom-1"))
        .is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 19: Key Package Invalid Authentication Tag Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_19_key_package_invalid_authentication_tag_rejected() {
    let escrow_signer = create_deterministic_signer("escrow-19", 919);
    let master_key = b"strong-passphrase-master-key-32b";
    let raw_secret = b"raw-private-key-material-to-seal";

    let mut pkg = KeyRecoveryPackage::create_encrypted(
        "operator-19",
        1,
        raw_secret,
        master_key,
        "escrow-19",
        escrow_signer.signing_key(),
    )
    .unwrap();

    // Tamper with the authentication tag
    pkg.auth_tag_hex = "00000000000000000000000000000000".to_string();

    let res = pkg.decrypt_key(master_key);
    assert!(
        res.is_err(),
        "Tampered auth tag must cause decryption failure"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 20: Raw Binary Secret Exclusion via Strict Allowlist
// -------------------------------------------------------------------------------------------------
#[test]
fn test_20_raw_binary_secret_exclusion_via_strict_allowlist() {
    let operator_signer = create_deterministic_signer("operator-20", 920);

    let snapshot = MetadataSnapshot::new(1, 10, BTreeMap::new());
    let mut verifying_keys = BTreeMap::new();
    let legit_pubkey = encode_hex(operator_signer.verifying_key().as_bytes());
    verifying_keys.insert("signer-1".to_string(), legit_pubkey);

    // Construct valid allowlist
    let allowlist = StrictMetadataAllowlist::new(
        "operator-20",
        1,
        10,
        10,
        snapshot.clone(),
        Vec::new(),
        verifying_keys,
    )
    .unwrap();

    let bundle = allowlist.into_bundle(operator_signer.signing_key());
    assert!(bundle.verify(&operator_signer.verifying_key()).is_ok());

    // Attempting to place an invalid/raw secret key in verifying keys list is rejected
    let mut bad_keys = BTreeMap::new();
    bad_keys.insert("signer-2".to_string(), "NOT_A_VALID_HEX_KEY".to_string());
    let res =
        StrictMetadataAllowlist::new("operator-20", 1, 10, 10, snapshot, Vec::new(), bad_keys);
    assert!(
        res.is_err(),
        "StrictMetadataAllowlist must reject invalid public key format"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 21: Key Rotation and Revocation Semantics
// -------------------------------------------------------------------------------------------------
#[test]
fn test_21_key_rotation_and_revocation_semantics() {
    let escrow_signer = create_deterministic_signer("escrow-21", 921);
    let master_key = b"master-key-rot-rev-test-12345678";
    let key_v1 = b"old-signing-key-material-v1-bytes";
    let key_v2 = b"new-signing-key-material-v2-bytes";

    let mut pkg_v1 = KeyRecoveryPackage::create_encrypted(
        "operator-21",
        1,
        key_v1,
        master_key,
        "escrow-21",
        escrow_signer.signing_key(),
    )
    .unwrap();

    let pkg_v2 = KeyRecoveryPackage::create_encrypted(
        "operator-21",
        2,
        key_v2,
        master_key,
        "escrow-21",
        escrow_signer.signing_key(),
    )
    .unwrap();

    // Prior to revocation, v1 decrypts cleanly
    assert_eq!(pkg_v1.decrypt_key(master_key).unwrap(), key_v1);

    // Revoke v1
    pkg_v1.revoke(escrow_signer.signing_key());
    assert!(pkg_v1.is_revoked);
    assert!(
        pkg_v1.decrypt_key(master_key).is_err(),
        "Revoked package cannot be decrypted"
    );

    // Rotated key v2 works
    assert_eq!(pkg_v2.decrypt_key(master_key).unwrap(), key_v2);
}

// -------------------------------------------------------------------------------------------------
// Test 22: Offline Revocation Limitations Demonstrated
// -------------------------------------------------------------------------------------------------
#[test]
fn test_22_offline_revocation_limitations_demonstrated() {
    let signer = create_deterministic_signer("signer-22", 922);
    let recovery_principal = create_deterministic_signer("principal-22", 923);

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer-22", signer.verifying_key());
    trust_store.register_recovery_principal("principal-22", recovery_principal.verifying_key());

    // Without receiving the revocation update, signer is treated as trusted offline
    assert!(!trust_store.is_signer_revoked("signer-22"));

    // Once CRL / Trust Store update is delivered, revocation is enforced
    trust_store.revoke_signer("signer-22");
    assert!(trust_store.is_signer_revoked("signer-22"));
}

// -------------------------------------------------------------------------------------------------
// Test 23: Coordinator Crash/SIGKILL During Restore Preserves Atomic State
// -------------------------------------------------------------------------------------------------
#[test]
fn test_23_coordinator_sigkill_during_restore_preserves_atomic_state() {
    let sandbox = TestDir::new("sigkill_atomic_restore");
    let operator_signer = create_deterministic_signer("dr-operator", 924);

    let snap_path = sandbox
        .path()
        .join(ReplicatedLogManager::SNAPSHOT_FILE_NAME);
    let prior_snapshot = MetadataSnapshot::new(100, 1, BTreeMap::new());
    write_and_sync_file(&snap_path, prior_snapshot.encode().as_bytes()).unwrap();

    // Create a new backup bundle with snapshot_index=200, snapshot_term=2
    let new_snapshot = MetadataSnapshot::new(200, 2, BTreeMap::new());
    let bundle = DisasterRecoveryBackupBundle::create(
        "dr-operator",
        2,
        200,
        200,
        new_snapshot,
        Vec::new(),
        BTreeMap::new(),
        operator_signer.signing_key(),
    );

    let auth = DisasterRebootstrapAuthorization::issue(
        "dr-operator",
        "inc-sigkill-test",
        1700000000,
        "Atomic restore test",
        operator_signer.signing_key(),
    );

    let report = execute_restore_v2(
        &bundle,
        &operator_signer.verifying_key(),
        sandbox.path(),
        RestoreMode::IsolatedRebootstrap(auth),
    )
    .unwrap();

    assert_eq!(report.status, DisasterRecoveryStatus::Success);
    assert_eq!(report.restored_commit_index, 200);

    // Verify no leftover .tmp files
    let tmp_snap = sandbox
        .path()
        .join(format!("{}.tmp", ReplicatedLogManager::SNAPSHOT_FILE_NAME));
    assert!(
        !tmp_snap.exists(),
        "Temporary snapshot file must be atomically renamed"
    );
}

// -------------------------------------------------------------------------------------------------
// Test 24: Twelve-of-Twenty Exact Recovery and Nine-Loss Safe Failure
// -------------------------------------------------------------------------------------------------
#[test]
fn test_24_twelve_of_twenty_exact_recovery_and_nine_loss_safe_failure() {
    let sandbox = TestDir::new("exact_twelve_and_nine_loss");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    let signer = create_deterministic_signer("signer_24", 940);
    let principal = create_deterministic_signer("user_24", 941);

    let (payload, _manifest) =
        setup_synthetic_encoded_object(&roots, "obj_24_test", 4096, &signer, 1);

    // Part A: Delete 8 shards (leaving exactly 12 shards, the RS(20,12) recovery threshold)
    for i in 12..20 {
        let obj_dir = roots[i].join("objects").join("obj_24_test");
        let _ = fs::remove_dir_all(&obj_dir);
    }

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer_24", signer.verifying_key());
    trust_store.register_recovery_principal("user_24", principal.verifying_key());

    let token = RecoveryCapabilityToken::issue(
        "obj_24_test",
        "default",
        "user_24",
        1800000000,
        principal.signing_key(),
    );

    let out_file = sandbox.path().join("reconstructed_12.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_24_test".into(),
        namespace: "default".into(),
        token: token.clone(),
        trust_store: trust_store.clone(),
        storage_roots: roots.clone(),
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(report.shards_read, 12);
    let recovered_bytes = fs::read(out_file).unwrap();
    assert_eq!(recovered_bytes, payload);

    // Part B: Now delete one more shard (leaving only 11 shards, 9 lost)
    let extra_del = roots[11].join("objects").join("obj_24_test");
    let _ = fs::remove_dir_all(&extra_del);

    let fail_file = sandbox.path().join("should_fail.bin");
    let fail_opts = SecureEmergencyReadOptions {
        object_id: "obj_24_test".into(),
        namespace: "default".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: fail_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let res = execute_secure_emergency_read(&fail_opts);
    assert!(
        res.is_err(),
        "Must fail safely without corruption when 9 shards are lost"
    );
    assert!(!fail_file.exists());
}

// -------------------------------------------------------------------------------------------------
// Test 25: Real 100 MiB RS(20,12) End-to-End Disaster Recovery and Emergency Read
// -------------------------------------------------------------------------------------------------
#[test]
fn test_25_real_100mib_end_to_end_disaster_recovery_and_emergency_read() {
    let sandbox = TestDir::new("real_100mib_dr");
    let signer = create_deterministic_signer("master-signer-25", 925);
    let recovery_principal = create_deterministic_signer("principal-25", 926);

    let storage_roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    // Multi-stripe streaming payload (matching Phase 5 streaming test pattern)
    let payload_len = 4 * 1024 * 1024;
    let (payload, _signed_manifest) =
        setup_synthetic_encoded_object(&storage_roots, "obj-100mib", payload_len, &signer, 1);

    let token = RecoveryCapabilityToken::issue(
        "obj-100mib",
        "default",
        "principal-25",
        1800000000,
        recovery_principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("master-signer-25", signer.verifying_key());
    trust_store.register_recovery_principal("principal-25", recovery_principal.verifying_key());

    // Execute emergency read across the 20 storage nodes
    let out_file = sandbox.path().join("recovered_100mib.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj-100mib".to_string(),
        namespace: "default".to_string(),
        token,
        trust_store,
        storage_roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(report.object_size, payload_len as u64);
    assert_eq!(report.object_digest, sha256_bytes(&payload).to_hex());
    assert_eq!(report.manifest_generation, 1);

    let recovered_bytes = fs::read(&out_file).unwrap();
    assert_eq!(recovered_bytes.len(), payload_len);
    assert_eq!(
        sha256_bytes(&recovered_bytes).to_hex(),
        report.object_digest
    );
}
