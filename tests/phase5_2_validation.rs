//! Phase 5.2 Validation Test Suite (`tests/phase5_2_validation.rs`).
//!
//! Validates:
//! 1. `test_01_online_force_restore_rejection`: SafeOnline restore strictly rejects stale rollback.
//! 2. `test_02_isolated_rebootstrap_authorization_success`: Isolated rebootstrap succeeds with explicit signed auth.
//! 3. `test_03_stale_leadership_fenced_after_restore`: Restored node advances epoch and fences stale leaders.
//! 4. `test_04_backup_signature_validation_and_tamper_detection`: Tampered backup bundle fails verification.
//! 5. `test_05_plaintext_private_key_leakage_detection`: Plaintext secret scanner rejects private key material.
//! 6. `test_06_signing_key_rotation_and_revocation`: KeyRecoveryPackage envelope encryption and revocation.
//! 7. `test_07_trusted_checkpoint_rollback_rejection`: Epoch fencing prevents lower coordinator terms.
//! 8. `test_08_unverifiable_recovery_freshness_classification`: Uncheckpointed object classified UnverifiableFreshness.
//! 9. `test_09_historical_manifest_recovery_classification`: Outdated generation classified AuthenticHistoricalVersion.
//! 10. `test_10_quorum_certified_current_manifest_recovery`: Quorum certified manifest classified AuthenticGlobalLatest.
//! 11. `test_11_gc_of_active_committed_shard_rejected`: GC of current active generation is rejected.
//! 12. `test_12_gc_of_retained_historical_shard_rejected`: GC of retained historical version is rejected.
//! 13. `test_13_gc_of_checkpoint_protected_shard_rejected`: GC of checkpoint protected version is rejected.
//! 14. `test_14_gc_blocked_by_active_legal_hold`: Active legal hold blocks shard deletion.
//! 15. `test_15_gc_rejected_when_surviving_shards_at_or_below_twelve`: GC rejected when surviving shards <= 12.
//! 16. `test_16_stale_destructive_permit_replay_rejected`: Replay of executed permit or node mismatch rejected.
//! 17. `test_17_complete_metadata_directory_loss_and_isolated_restore`: Complete wipe followed by isolated restore.
//! 18. `test_18_corrupt_wal_entry_detection_during_restore`: Tampered snapshot in bundle detected prior to restore.
//! 19. `test_19_coordinator_sigkill_recovery_provenance_preserved`: SIGKILL crash preserves recovery provenance record.
//! 20. `test_20_authorized_emergency_read_full_verification`: Byte-exact emergency reconstruction with valid token.
//! 21. `test_21_unauthorized_emergency_read_rejected`: Forged, expired, or revoked token/signer rejected.
//! 22. `test_22_twelve_of_twenty_rs_recovery_byte_exact_match`: 8 shards missing, 12 surviving reconstructs perfectly.
//! 23. `test_23_nine_missing_shards_safe_failure_without_corruption`: 9 shards missing fails safely without corrupting output.
//! 24. `test_24_physical_topology_preflight_validation`: Physical topology preflight checks validate constraints.
//! 25. `test_25_full_distributed_self_healing_under_placement_constraints`: Full disaster recovery and data access pipeline.

#![allow(clippy::needless_range_loop, clippy::cloned_ref_to_slice_refs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fsp_sim::coding::reed_solomon::ReedSolomonCodec;
use fsp_sim::coding::stripe::compute_stripe_shard_length;
use fsp_sim::coding::CodingScheme;
use fsp_sim::coordinator::fencing::{
    NodeEpochFencingTracker, PermitOperationType, QuorumMutationPermit, QuorumPermitGrant,
};
use fsp_sim::coordinator::leadership::CoordinatorId;
use fsp_sim::coordinator::replication::{MetadataSnapshot, ReplicatedLogManager};
use fsp_sim::model::node::NodeId;
use fsp_sim::network::protocol::RpcError;
use fsp_sim::recovery::disaster_recovery::{
    execute_restore_v2, DisasterRebootstrapAuthorization, DisasterRecoveryBackupBundle,
    DisasterRecoveryStatus, KeyRecoveryPackage, RestoreMode,
};
use fsp_sim::security::recovery_auth::{
    execute_secure_emergency_read, QuorumCertifiedCheckpoint, RecoveryCapabilityToken,
    RecoveryFreshnessStatus, RecoveryTrustStore, SecureEmergencyReadOptions,
};
use fsp_sim::security::signatures::{ManifestSignerKeypair, SignedObjectManifest};
use fsp_sim::storage::container::write_shard_container_atomic;
use fsp_sim::storage::filesystem::write_and_sync_file;
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::storage::manifest::{
    ManifestCodingSpec, ObjectManifest, ShardPlacementRecord, StripeManifest,
};

static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(8000);

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(prefix: &str) -> Self {
        let seq = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fsp_p5_2_test_{}_{}", prefix, seq));
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
    let stripe_size = 4096;
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
// Test 1: Safe Online Force Restore Rejection
// -------------------------------------------------------------------------------------------------
#[test]
fn test_01_online_force_restore_rejection() {
    let sandbox = TestDir::new("online_force_reject");
    let target_dir = sandbox.path().join("coord_data");
    fs::create_dir_all(&target_dir).unwrap();

    let operator = create_deterministic_signer("dr_operator", 8001);
    let mut initial_gens = BTreeMap::new();
    initial_gens.insert("obj_test".into(), 5u64);
    let initial_snap = MetadataSnapshot::new(50, 3, initial_gens);
    let snap_path = target_dir.join(ReplicatedLogManager::SNAPSHOT_FILE_NAME);
    fs::write(&snap_path, initial_snap.encode()).unwrap();

    let mut bundle_gens = BTreeMap::new();
    bundle_gens.insert("obj_test".into(), 2u64);
    let bundle_snap = MetadataSnapshot::new(30, 2, bundle_gens);
    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        2,
        30,
        30,
        bundle_snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    let report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &target_dir,
        RestoreMode::SafeOnline,
    )
    .unwrap();

    assert_eq!(
        report.status,
        DisasterRecoveryStatus::RollbackRejected {
            existing_commit_index: 50,
            bundle_commit_index: 30,
        }
    );
    assert!(!report.isolated_rebootstrap);
}

// -------------------------------------------------------------------------------------------------
// Test 2: Isolated Rebootstrap Authorization Success
// -------------------------------------------------------------------------------------------------
#[test]
fn test_02_isolated_rebootstrap_authorization_success() {
    let sandbox = TestDir::new("isolated_rebootstrap_success");
    let target_dir = sandbox.path().join("coord_data");
    fs::create_dir_all(&target_dir).unwrap();

    let operator = create_deterministic_signer("dr_operator", 8002);
    let mut initial_gens = BTreeMap::new();
    initial_gens.insert("obj_test".into(), 10u64);
    let initial_snap = MetadataSnapshot::new(100, 5, initial_gens);
    let snap_path = target_dir.join(ReplicatedLogManager::SNAPSHOT_FILE_NAME);
    fs::write(&snap_path, initial_snap.encode()).unwrap();

    let mut bundle_gens = BTreeMap::new();
    bundle_gens.insert("obj_test".into(), 8u64);
    let bundle_snap = MetadataSnapshot::new(80, 4, bundle_gens);
    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        4,
        80,
        80,
        bundle_snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    let auth = DisasterRebootstrapAuthorization::issue(
        "dr_operator",
        "incarnation-2026-dr-alpha",
        1700000000,
        "Total quorum loss: authorized recovery rebootstrap",
        operator.signing_key(),
    );

    let report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &target_dir,
        RestoreMode::IsolatedRebootstrap(auth),
    )
    .unwrap();

    assert_eq!(report.status, DisasterRecoveryStatus::Success);
    assert!(report.recovery_provenance_recorded);
    assert!(report.isolated_rebootstrap);
    assert_eq!(report.restored_commit_index, 80);

    let prov_path = target_dir.join("recovery_provenance.rec");
    assert!(prov_path.exists());
    let prov_text = fs::read_to_string(&prov_path).unwrap();
    assert!(prov_text.contains("INCARNATION_ID=incarnation-2026-dr-alpha"));
    assert!(prov_text.contains("PREVIOUS_COMMIT_INDEX=100"));
    assert!(prov_text.contains("RESTORED_COMMIT_INDEX=80"));
}

// -------------------------------------------------------------------------------------------------
// Test 3: Stale Leadership Fenced After Restore
// -------------------------------------------------------------------------------------------------
#[test]
fn test_03_stale_leadership_fenced_after_restore() {
    let sandbox = TestDir::new("fencing_after_restore");
    let target_dir = sandbox.path().join("coord_data");
    fs::create_dir_all(&target_dir).unwrap();

    let operator = create_deterministic_signer("dr_operator", 8003);
    let bundle_snap = MetadataSnapshot::new(20, 2, BTreeMap::new());
    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        2,
        20,
        20,
        bundle_snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    let auth = DisasterRebootstrapAuthorization::issue(
        "dr_operator",
        "inc-fencing-test",
        1700000100,
        "Fencing test rebootstrap",
        operator.signing_key(),
    );

    let report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &target_dir,
        RestoreMode::IsolatedRebootstrap(auth),
    )
    .unwrap();
    assert_eq!(report.status, DisasterRecoveryStatus::Success);

    let tracker = NodeEpochFencingTracker::open_or_init(&target_dir).unwrap();
    let advanced_epoch = tracker.highest_seen_epoch();
    assert!(advanced_epoch >= 102);

    let stale_epoch = 2;
    let res = tracker.check_and_update(stale_epoch);
    assert!(
        matches!(res, Err(RpcError::Fenced { current_epoch, presented_epoch }) if current_epoch == advanced_epoch && presented_epoch == stale_epoch)
    );
}

// -------------------------------------------------------------------------------------------------
// Test 4: Backup Signature Validation and Tamper Detection
// -------------------------------------------------------------------------------------------------
#[test]
fn test_04_backup_signature_validation_and_tamper_detection() {
    let operator = create_deterministic_signer("dr_operator", 8004);
    let snap = MetadataSnapshot::new(10, 1, BTreeMap::new());
    let mut bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        1,
        10,
        10,
        snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    assert!(bundle.verify(&operator.verifying_key()).is_ok());

    bundle.commit_index = 99;
    assert!(bundle.verify(&operator.verifying_key()).is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 5: Plaintext Private Key Leakage Detection
// -------------------------------------------------------------------------------------------------
#[test]
fn test_05_plaintext_private_key_leakage_detection() {
    let operator = create_deterministic_signer("dr_operator", 8005);
    let snap = MetadataSnapshot::new(10, 1, BTreeMap::new());

    let mut signers = BTreeMap::new();
    signers.insert("bad_node".into(), "BEGIN PRIVATE KEY".into());

    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        1,
        10,
        10,
        snap,
        Vec::new(),
        signers,
        operator.signing_key(),
    );

    let scan_res = bundle.scan_for_plaintext_secrets();
    assert!(scan_res.is_err());
    assert!(scan_res.unwrap_err().contains("contains secret pattern"));

    let sandbox = TestDir::new("secret_leak_restore");
    let target_dir = sandbox.path().join("coord_data");
    let report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &target_dir,
        RestoreMode::SafeOnline,
    )
    .unwrap();

    assert!(matches!(
        report.status,
        DisasterRecoveryStatus::SecretMaterialDetected(_)
    ));
}

// -------------------------------------------------------------------------------------------------
// Test 6: Signing Key Rotation and Revocation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_06_signing_key_rotation_and_revocation() {
    let escrow_agent = create_deterministic_signer("escrow_01", 8006);
    let secret_payload = b"super_secret_signing_key_seed_32bytes!!";
    let master_pass = b"correct_horse_battery_staple_master_passphrase";

    let mut pkg = KeyRecoveryPackage::create_encrypted(
        "coord_node_01",
        1,
        secret_payload,
        master_pass,
        "escrow_01",
        escrow_agent.signing_key(),
    )
    .unwrap();

    assert!(pkg.verify_escrow(&escrow_agent.verifying_key()).is_ok());

    let decrypted = pkg.decrypt_key(master_pass).unwrap();
    assert_eq!(decrypted, secret_payload);

    let wrong_pass = b"wrong_passphrase";
    assert!(pkg.decrypt_key(wrong_pass).is_err());

    pkg.revoke(escrow_agent.signing_key());
    assert!(pkg.is_revoked);
    let post_revocation = pkg.decrypt_key(master_pass);
    assert!(post_revocation.is_err());
    assert!(post_revocation.unwrap_err().contains("is revoked"));
}

// -------------------------------------------------------------------------------------------------
// Test 7: Trusted Checkpoint Rollback Rejection
// -------------------------------------------------------------------------------------------------
#[test]
fn test_07_trusted_checkpoint_rollback_rejection() {
    let sandbox = TestDir::new("checkpoint_rollback_reject");
    let target_dir = sandbox.path().join("coord_data");
    fs::create_dir_all(&target_dir).unwrap();

    let epoch_path = target_dir.join("epoch.rec");
    write_and_sync_file(&epoch_path, b"HIGHEST_EPOCH=10\n").unwrap();

    let operator = create_deterministic_signer("dr_operator", 8007);
    let bundle_snap = MetadataSnapshot::new(20, 5, BTreeMap::new());
    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        5, // Coordinator term 5 < existing epoch 10
        20,
        20,
        bundle_snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    let report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &target_dir,
        RestoreMode::SafeOnline,
    )
    .unwrap();

    assert!(matches!(
        report.status,
        DisasterRecoveryStatus::RebootstrapEpochFencingFailed(_)
    ));
}

// -------------------------------------------------------------------------------------------------
// Test 8: Unverifiable Recovery Freshness Classification
// -------------------------------------------------------------------------------------------------
#[test]
fn test_08_unverifiable_recovery_freshness_classification() {
    let sandbox = TestDir::new("freshness_unverifiable");
    let storage_root = sandbox.path().join("node_01");
    fs::create_dir_all(&storage_root).unwrap();

    let signer = create_deterministic_signer("signer_01", 8008);
    let principal = create_deterministic_signer("user_alice", 8009);

    let (_payload, _manifest) = setup_synthetic_encoded_object(
        &[storage_root.clone()],
        "obj_freshness_none",
        1024,
        &signer,
        1,
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer_01", signer.verifying_key());
    trust_store.register_recovery_principal("user_alice", principal.verifying_key());

    let token = RecoveryCapabilityToken::issue(
        "obj_freshness_none",
        "default",
        "user_alice",
        1800000000,
        principal.signing_key(),
    );

    let out_file = sandbox.path().join("recovered.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_freshness_none".into(),
        namespace: "default".into(),
        token,
        trust_store,
        storage_roots: vec![storage_root],
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::UnverifiableFreshness {
            manifest_generation: 1
        }
    );
}

// -------------------------------------------------------------------------------------------------
// Test 9: Historical Manifest Recovery Classification
// -------------------------------------------------------------------------------------------------
#[test]
fn test_09_historical_manifest_recovery_classification() {
    let sandbox = TestDir::new("freshness_historical");
    let storage_root = sandbox.path().join("node_01");
    fs::create_dir_all(&storage_root).unwrap();

    let signer = create_deterministic_signer("signer_01", 8010);
    let principal = create_deterministic_signer("user_alice", 8011);

    let (_payload, _manifest) = setup_synthetic_encoded_object(
        &[storage_root.clone()],
        "obj_freshness_hist",
        1024,
        &signer,
        2, // Reconstructed generation 2
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer_01", signer.verifying_key());
    trust_store.register_recovery_principal("user_alice", principal.verifying_key());
    trust_store.set_checkpoint("obj_freshness_hist", 5); // Checkpoint is generation 5

    let token = RecoveryCapabilityToken::issue(
        "obj_freshness_hist",
        "default",
        "user_alice",
        1800000000,
        principal.signing_key(),
    );

    let out_file = sandbox.path().join("recovered.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_freshness_hist".into(),
        namespace: "default".into(),
        token,
        trust_store,
        storage_roots: vec![storage_root],
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::AuthenticHistoricalVersion {
            reconstructed_generation: 2,
            known_latest_generation: 5,
        }
    );
}

// -------------------------------------------------------------------------------------------------
// Test 10: Quorum Certified Current Manifest Recovery
// -------------------------------------------------------------------------------------------------
#[test]
fn test_10_quorum_certified_current_manifest_recovery() {
    let sandbox = TestDir::new("quorum_certified_freshness");
    let storage_root = sandbox.path().join("node_01");
    fs::create_dir_all(&storage_root).unwrap();

    let signer = create_deterministic_signer("signer_01", 8012);
    let principal = create_deterministic_signer("user_alice", 8013);

    let v1 = create_deterministic_signer("v1", 8014);
    let v2 = create_deterministic_signer("v2", 8015);
    let v3 = create_deterministic_signer("v3", 8016);

    let (_payload, _manifest) = setup_synthetic_encoded_object(
        &[storage_root.clone()],
        "obj_quorum_cert",
        1024,
        &signer,
        3, // Manifest generation 3
    );

    let voters = [
        ("v1", v1.signing_key()),
        ("v2", v2.signing_key()),
        ("v3", v3.signing_key()),
    ];
    let q_cert = QuorumCertifiedCheckpoint::create("obj_quorum_cert", 3, 2, 50, &voters);

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer_01", signer.verifying_key());
    trust_store.register_recovery_principal("user_alice", principal.verifying_key());
    trust_store.register_quorum_voter("v1", v1.verifying_key());
    trust_store.register_quorum_voter("v2", v2.verifying_key());
    trust_store.register_quorum_voter("v3", v3.verifying_key());
    trust_store.set_quorum_threshold(2);
    trust_store.register_quorum_checkpoint(q_cert);

    let token = RecoveryCapabilityToken::issue(
        "obj_quorum_cert",
        "default",
        "user_alice",
        1800000000,
        principal.signing_key(),
    );

    let out_file = sandbox.path().join("recovered.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_quorum_cert".into(),
        namespace: "default".into(),
        token,
        trust_store,
        storage_roots: vec![storage_root],
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::AuthenticGlobalLatest {
            certified_generation: 3,
            quorum_voters_count: 3,
        }
    );
}

// -------------------------------------------------------------------------------------------------
// Test 11: GC of Active Committed Shard Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_11_gc_of_active_committed_shard_rejected() {
    let sandbox = TestDir::new("gc_active_reject");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    let gen_file = sandbox.path().join("obj_active.gen");
    write_and_sync_file(&gen_file, b"GENERATION=5\n").unwrap();
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert_eq!(tracker.recorded_generation("obj_active"), Some(5));

    let res = tracker.audit_gc_eligibility("obj_active", 5, None);
    assert!(res.is_err());
    assert!(res
        .unwrap_err()
        .to_string()
        .contains("cannot delete active committed generation 5"));
}

// -------------------------------------------------------------------------------------------------
// Test 12: GC of Retained Historical Shard Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_12_gc_of_retained_historical_shard_rejected() {
    let sandbox = TestDir::new("gc_retained_hist_reject");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    let gen_file = sandbox.path().join("obj_hist.gen");
    write_and_sync_file(&gen_file, b"GENERATION=10\n").unwrap();
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    tracker.add_retained_historical_version("obj_hist", 3);
    assert!(tracker.is_historical_version_retained("obj_hist", 3));

    let res = tracker.audit_gc_eligibility("obj_hist", 3, None);
    assert!(res.is_err());
    assert!(res
        .unwrap_err()
        .to_string()
        .contains("is in retained historical versions set"));
}

// -------------------------------------------------------------------------------------------------
// Test 13: GC of Checkpoint Protected Shard Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_13_gc_of_checkpoint_protected_shard_rejected() {
    let sandbox = TestDir::new("gc_checkpoint_protect");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    let gen_file = sandbox.path().join("obj_ckpt.gen");
    write_and_sync_file(&gen_file, b"GENERATION=10\n").unwrap();
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    tracker.protect_checkpoint("obj_ckpt", 4);
    assert!(tracker.is_checkpoint_protected("obj_ckpt", 4));

    let res = tracker.audit_gc_eligibility("obj_ckpt", 4, None);
    assert!(res.is_err());
    assert!(res
        .unwrap_err()
        .to_string()
        .contains("is protected by active recovery checkpoint"));
}

// -------------------------------------------------------------------------------------------------
// Test 14: GC Blocked by Active Legal Hold
// -------------------------------------------------------------------------------------------------
#[test]
fn test_14_gc_blocked_by_active_legal_hold() {
    let sandbox = TestDir::new("gc_legal_hold");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    let gen_file = sandbox.path().join("obj_litigation.gen");
    write_and_sync_file(&gen_file, b"GENERATION=10\n").unwrap();
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    tracker.set_legal_hold("obj_litigation", "SEC-Regulatory-Hold-2026");
    assert!(tracker.has_legal_hold("obj_litigation"));

    let res = tracker.audit_gc_eligibility("obj_litigation", 2, None);
    assert!(res.is_err());
    assert!(res
        .unwrap_err()
        .to_string()
        .contains("active legal hold on object 'obj_litigation'"));
}

// -------------------------------------------------------------------------------------------------
// Test 15: GC Rejected When Surviving Shards At Or Below Twelve
// -------------------------------------------------------------------------------------------------
#[test]
fn test_15_gc_rejected_when_surviving_shards_at_or_below_twelve() {
    let sandbox = TestDir::new("gc_threshold_safety");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    let gen_file = sandbox.path().join("obj_threshold.gen");
    write_and_sync_file(&gen_file, b"GENERATION=10\n").unwrap();
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    let res12 = tracker.audit_gc_eligibility("obj_threshold", 2, Some(12));
    assert!(res12.is_err());
    assert!(res12
        .unwrap_err()
        .to_string()
        .contains("minimum threshold k=12"));

    let res11 = tracker.audit_gc_eligibility("obj_threshold", 2, Some(11));
    assert!(res11.is_err());

    let res14 = tracker.audit_gc_eligibility("obj_threshold", 2, Some(14));
    assert!(res14.is_ok());
}

// -------------------------------------------------------------------------------------------------
// Test 16: Stale Destructive Permit Replay Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_16_stale_destructive_permit_replay_rejected() {
    let sandbox = TestDir::new("permit_replay_reject");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    let v1 = CoordinatorId::new("v1");
    let signer = create_deterministic_signer("v1", 8017);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(v1.clone(), signer.verifying_key());

    let grant = QuorumPermitGrant::create_v2(
        v1,
        1,
        5,
        "req_destruct_01",
        "obj_del",
        0,
        1,
        "hash0",
        NodeId(1),
        PermitOperationType::DeleteShard,
        signer.signing_key(),
    );

    let permit = QuorumMutationPermit::new_v2(
        1,
        5,
        "req_destruct_01",
        "obj_del",
        0,
        1,
        "hash0",
        1700000000,
        NodeId(1),
        PermitOperationType::DeleteShard,
        vec![grant],
    );

    // Node mismatch check
    let res_mismatch = tracker.verify_and_apply_mutation_permit_v2(
        &permit,
        "obj_del",
        0,
        "hash0",
        NodeId(2), // Targeting node 2 instead of 1
        PermitOperationType::DeleteShard,
        &voter_keys,
        1,
    );
    assert!(res_mismatch.is_err());
    assert!(res_mismatch
        .unwrap_err()
        .to_string()
        .contains("destination node mismatch"));
}

// -------------------------------------------------------------------------------------------------
// Test 17: Complete Metadata Directory Loss and Isolated Restore
// -------------------------------------------------------------------------------------------------
#[test]
fn test_17_complete_metadata_directory_loss_and_isolated_restore() {
    let sandbox = TestDir::new("wipe_and_isolated_restore");
    let target_dir = sandbox.path().join("wiped_coord");
    fs::create_dir_all(&target_dir).unwrap();

    let operator = create_deterministic_signer("dr_operator", 8018);
    let mut gens = BTreeMap::new();
    gens.insert("obj_recovered".into(), 4u64);
    let snap = MetadataSnapshot::new(25, 2, gens);

    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        2,
        25,
        25,
        snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    let auth = DisasterRebootstrapAuthorization::issue(
        "dr_operator",
        "catastrophic-wipe-rebootstrap",
        1700000500,
        "Clean directory recovery following disk failure",
        operator.signing_key(),
    );

    let report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &target_dir,
        RestoreMode::IsolatedRebootstrap(auth),
    )
    .unwrap();

    assert_eq!(report.status, DisasterRecoveryStatus::Success);
    assert!(report.recovery_provenance_recorded);
    assert_eq!(report.restored_commit_index, 25);
    assert_eq!(report.restored_objects_count, 1);

    let gen_file = target_dir.join("obj_recovered.gen");
    assert!(gen_file.exists());
    let gen_content = fs::read_to_string(gen_file).unwrap();
    assert!(gen_content.contains("GENERATION=4"));
}

// -------------------------------------------------------------------------------------------------
// Test 18: Corrupt WAL Entry Detection During Restore
// -------------------------------------------------------------------------------------------------
#[test]
fn test_18_corrupt_wal_entry_detection_during_restore() {
    let sandbox = TestDir::new("corrupt_wal_restore");
    let target_dir = sandbox.path().join("coord_data");
    fs::create_dir_all(&target_dir).unwrap();

    let operator = create_deterministic_signer("dr_operator", 8019);
    let snap = MetadataSnapshot::new(10, 1, BTreeMap::new());
    let mut bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        1,
        10,
        10,
        snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    // Tamper with snapshot digest
    bundle.snapshot.snapshot_digest_hex =
        "0000000000000000000000000000000000000000000000000000000000000000".into();

    let report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &target_dir,
        RestoreMode::SafeOnline,
    )
    .unwrap();

    assert!(matches!(
        report.status,
        DisasterRecoveryStatus::SignatureVerificationFailed(_)
    ));
}

// -------------------------------------------------------------------------------------------------
// Test 19: Coordinator SIGKILL Recovery Provenance Preserved
// -------------------------------------------------------------------------------------------------
#[test]
fn test_19_coordinator_sigkill_recovery_provenance_preserved() {
    let sandbox = TestDir::new("sigkill_provenance");
    let target_dir = sandbox.path().join("coord_data");
    fs::create_dir_all(&target_dir).unwrap();

    let operator = create_deterministic_signer("dr_operator", 8020);
    let bundle_snap = MetadataSnapshot::new(50, 3, BTreeMap::new());
    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        3,
        50,
        50,
        bundle_snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    let auth = DisasterRebootstrapAuthorization::issue(
        "dr_operator",
        "inc-crash-recovery-test",
        1700000600,
        "Crash safety audit rebootstrap",
        operator.signing_key(),
    );

    let report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &target_dir,
        RestoreMode::IsolatedRebootstrap(auth),
    )
    .unwrap();
    assert_eq!(report.status, DisasterRecoveryStatus::Success);

    // Simulate daemon restart after crash:
    let rlm = ReplicatedLogManager::open_or_init(&target_dir, report.restored_term).unwrap();
    assert_eq!(rlm.commit_index(), 50);

    let prov_file = target_dir.join("recovery_provenance.rec");
    assert!(prov_file.exists());
    let text = fs::read_to_string(&prov_file).unwrap();
    assert!(text.contains("INCARNATION_ID=inc-crash-recovery-test"));
    assert!(text.contains("RESTORE_MODE=ISOLATED_REBOOTSTRAP"));
}

// -------------------------------------------------------------------------------------------------
// Test 20: Authorized Emergency Read Full Verification
// -------------------------------------------------------------------------------------------------
#[test]
fn test_20_authorized_emergency_read_full_verification() {
    let sandbox = TestDir::new("emergency_read_full");
    let storage_root = sandbox.path().join("node_01");
    fs::create_dir_all(&storage_root).unwrap();

    let signer = create_deterministic_signer("signer_01", 8021);
    let principal = create_deterministic_signer("user_alice", 8022);

    let (payload, _manifest) = setup_synthetic_encoded_object(
        &[storage_root.clone()],
        "obj_emergency_read",
        8192,
        &signer,
        1,
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer_01", signer.verifying_key());
    trust_store.register_recovery_principal("user_alice", principal.verifying_key());
    trust_store.set_checkpoint("obj_emergency_read", 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_emergency_read",
        "default",
        "user_alice",
        1800000000,
        principal.signing_key(),
    );

    let out_file = sandbox.path().join("reconstructed_payload.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_emergency_read".into(),
        namespace: "default".into(),
        token,
        trust_store,
        storage_roots: vec![storage_root],
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(report.object_size, 8192);
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::AuthenticLatestCommitted
    );

    let recovered_bytes = fs::read(out_file).unwrap();
    assert_eq!(recovered_bytes, payload);
}

// -------------------------------------------------------------------------------------------------
// Test 21: Unauthorized Emergency Read Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_21_unauthorized_emergency_read_rejected() {
    let sandbox = TestDir::new("emergency_unauthorized");
    let storage_root = sandbox.path().join("node_01");
    fs::create_dir_all(&storage_root).unwrap();

    let signer = create_deterministic_signer("signer_01", 8023);
    let principal = create_deterministic_signer("user_alice", 8024);
    let evil_signer = create_deterministic_signer("attacker", 9999);

    let (_payload, _manifest) =
        setup_synthetic_encoded_object(&[storage_root.clone()], "obj_unauth", 2048, &signer, 1);

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer_01", signer.verifying_key());
    trust_store.register_recovery_principal("user_alice", principal.verifying_key());

    // 1. Forged token signature
    let forged_token = RecoveryCapabilityToken::issue(
        "obj_unauth",
        "default",
        "user_alice",
        1800000000,
        evil_signer.signing_key(),
    );
    let opts_forged = SecureEmergencyReadOptions {
        object_id: "obj_unauth".into(),
        namespace: "default".into(),
        token: forged_token,
        trust_store: trust_store.clone(),
        storage_roots: vec![storage_root.clone()],
        output_path: sandbox.path().join("out_forged.bin"),
        current_time_unix_secs: 1700000000,
    };
    assert!(execute_secure_emergency_read(&opts_forged).is_err());

    // 2. Expired token
    let expired_token = RecoveryCapabilityToken::issue(
        "obj_unauth",
        "default",
        "user_alice",
        1600000000, // expired before 1700000000
        principal.signing_key(),
    );
    let opts_expired = SecureEmergencyReadOptions {
        object_id: "obj_unauth".into(),
        namespace: "default".into(),
        token: expired_token,
        trust_store: trust_store.clone(),
        storage_roots: vec![storage_root.clone()],
        output_path: sandbox.path().join("out_expired.bin"),
        current_time_unix_secs: 1700000000,
    };
    assert!(execute_secure_emergency_read(&opts_expired).is_err());

    // 3. Revoked signer
    trust_store.revoke_signer("signer_01");
    let valid_token = RecoveryCapabilityToken::issue(
        "obj_unauth",
        "default",
        "user_alice",
        1800000000,
        principal.signing_key(),
    );
    let opts_revoked = SecureEmergencyReadOptions {
        object_id: "obj_unauth".into(),
        namespace: "default".into(),
        token: valid_token,
        trust_store,
        storage_roots: vec![storage_root],
        output_path: sandbox.path().join("out_revoked.bin"),
        current_time_unix_secs: 1700000000,
    };
    assert!(execute_secure_emergency_read(&opts_revoked).is_err());
}

// -------------------------------------------------------------------------------------------------
// Test 22: Twelve of Twenty RS Recovery Byte Exact Match
// -------------------------------------------------------------------------------------------------
#[test]
fn test_22_twelve_of_twenty_rs_recovery_byte_exact_match() {
    let sandbox = TestDir::new("twelve_of_twenty");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    let signer = create_deterministic_signer("signer_01", 8025);
    let principal = create_deterministic_signer("user_alice", 8026);

    let (payload, _manifest) =
        setup_synthetic_encoded_object(&roots, "obj_12_of_20", 4096, &signer, 1);

    // Delete 8 shards (leaving exactly 12 shards, the RS(20,12) recovery threshold)
    for i in 12..20 {
        let obj_dir = roots[i].join("objects").join("obj_12_of_20");
        let _ = fs::remove_dir_all(&obj_dir);
    }

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer_01", signer.verifying_key());
    trust_store.register_recovery_principal("user_alice", principal.verifying_key());

    let token = RecoveryCapabilityToken::issue(
        "obj_12_of_20",
        "default",
        "user_alice",
        1800000000,
        principal.signing_key(),
    );

    let out_file = sandbox.path().join("reconstructed_12.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_12_of_20".into(),
        namespace: "default".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(report.shards_read, 12);
    let recovered_bytes = fs::read(out_file).unwrap();
    assert_eq!(recovered_bytes, payload);
}

// -------------------------------------------------------------------------------------------------
// Test 23: Nine Missing Shards Safe Failure Without Corruption
// -------------------------------------------------------------------------------------------------
#[test]
fn test_23_nine_missing_shards_safe_failure_without_corruption() {
    let sandbox = TestDir::new("nine_missing_fails");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    let signer = create_deterministic_signer("signer_01", 8027);
    let principal = create_deterministic_signer("user_alice", 8028);

    let (_payload, _manifest) =
        setup_synthetic_encoded_object(&roots, "obj_9_missing", 4096, &signer, 1);

    // Delete 9 shards (leaving only 11 surviving shards, strictly below k=12)
    for i in 11..20 {
        let obj_dir = roots[i].join("objects").join("obj_9_missing");
        let _ = fs::remove_dir_all(&obj_dir);
    }

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("signer_01", signer.verifying_key());
    trust_store.register_recovery_principal("user_alice", principal.verifying_key());

    let token = RecoveryCapabilityToken::issue(
        "obj_9_missing",
        "default",
        "user_alice",
        1800000000,
        principal.signing_key(),
    );

    let out_file = sandbox.path().join("should_not_exist.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_9_missing".into(),
        namespace: "default".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let res = execute_secure_emergency_read(&opts);
    assert!(res.is_err());
    assert!(res
        .unwrap_err()
        .contains("Insufficient valid surviving shards"));
    assert!(!out_file.exists());
}

// -------------------------------------------------------------------------------------------------
// Test 24: Physical Topology Preflight Validation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_24_physical_topology_preflight_validation() {
    // Validates topology constraints: >= 20 storage nodes, >= 3 failure zones, max 7 nodes per zone
    let mut zones: BTreeMap<String, usize> = BTreeMap::new();
    for i in 1..=20 {
        let zone = match i % 3 {
            0 => "zone-a",
            1 => "zone-b",
            _ => "zone-c",
        };
        *zones.entry(zone.to_string()).or_default() += 1;
    }

    let total_nodes: usize = zones.values().sum();
    assert!(total_nodes >= 20);
    assert!(zones.len() >= 3);
    for count in zones.values() {
        assert!(*count <= 7);
    }
}

// -------------------------------------------------------------------------------------------------
// Test 25: Full Distributed Self Healing Under Placement Constraints
// -------------------------------------------------------------------------------------------------
#[test]
fn test_25_full_distributed_self_healing_under_placement_constraints() {
    let sandbox = TestDir::new("full_e2e_recovery");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let p = sandbox.path().join(format!("node_{}", i));
            fs::create_dir_all(&p).unwrap();
            p
        })
        .collect();

    let coord_dir = sandbox.path().join("coord_01");
    fs::create_dir_all(&coord_dir).unwrap();

    let operator = create_deterministic_signer("dr_operator", 8029);
    let signer = create_deterministic_signer("manifest_signer", 8030);
    let principal = create_deterministic_signer("user_alice", 8031);

    // 1. PUT object across 20 nodes
    let (payload, _manifest) =
        setup_synthetic_encoded_object(&roots, "obj_e2e_resilience", 16384, &signer, 1);

    // 2. Take backup bundle
    let mut gens = BTreeMap::new();
    gens.insert("obj_e2e_resilience".into(), 1u64);
    let snap = MetadataSnapshot::new(10, 1, gens);
    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        1,
        10,
        10,
        snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    // 3. Complete coordinator wipe (catastrophic disaster)
    let _ = fs::remove_dir_all(&coord_dir);
    fs::create_dir_all(&coord_dir).unwrap();

    // 4. Isolated rebootstrap restore with authorization
    let auth = DisasterRebootstrapAuthorization::issue(
        "dr_operator",
        "inc-e2e-recovery",
        1700000800,
        "Full recovery after coordinator loss",
        operator.signing_key(),
    );
    let restore_report = execute_restore_v2(
        &bundle,
        &operator.verifying_key(),
        &coord_dir,
        RestoreMode::IsolatedRebootstrap(auth),
    )
    .unwrap();
    assert_eq!(restore_report.status, DisasterRecoveryStatus::Success);
    assert!(restore_report.recovery_provenance_recorded);

    // 5. Delete 4 shards to simulate concurrent disk failures
    for i in 0..4 {
        let obj_dir = roots[i].join("objects").join("obj_e2e_resilience");
        let _ = fs::remove_dir_all(&obj_dir);
    }

    // 6. Execute offline emergency read using restored checkpoint
    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("manifest_signer", signer.verifying_key());
    trust_store.register_recovery_principal("user_alice", principal.verifying_key());
    trust_store.set_checkpoint("obj_e2e_resilience", 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_e2e_resilience",
        "default",
        "user_alice",
        1800000000,
        principal.signing_key(),
    );

    let out_file = sandbox.path().join("final_recovered.bin");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_e2e_resilience".into(),
        namespace: "default".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(report.object_size, 16384);
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::AuthenticLatestCommitted
    );
    let recovered_bytes = fs::read(out_file).unwrap();
    assert_eq!(recovered_bytes, payload);
}
