//! Phase 5.1 Comprehensive Validation & Disaster Recovery Test Suite (`tests/phase5_1_validation.rs`).
//!
//! Validates:
//! 1. Secure Emergency Read with valid cryptographic principal token, signed manifest, and matching payload.
//! 2. Expired RecoveryCapabilityToken is strictly rejected.
//! 3. Forged RecoveryCapabilityToken signature is strictly rejected.
//! 4. Unknown or untrusted recovery principal is strictly rejected.
//! 5. Namespace mismatch between token and requested emergency read is strictly rejected.
//! 6. Object ID mismatch between token and requested emergency read is strictly rejected.
//! 7. Tampered manifest signature or content is strictly rejected.
//! 8. Untrusted manifest signer is strictly rejected.
//! 9. Per-shard SHA-256 mismatch is detected, corrupt shard discarded, and healthy shard used.
//! 10. Insufficient surviving shards (< 12) fails with clear error and does not corrupt output.
//! 11. Freshness classification: matches latest checkpoint -> AuthenticLatestCommitted.
//! 12. Freshness classification: below latest checkpoint -> AuthenticHistoricalVersion.
//! 13. Freshness classification: no checkpoint in trust store -> UnverifiableFreshness.
//! 14. Multi-stripe payload reconstruction with exact byte-level trimming to object_size.
//! 15. DisasterRecoveryBackupBundle creation, snapshot serialization, and Ed25519 signature verification.
//! 16. DisasterRecoveryBackupBundle tamper detection (digest or signature mismatch).
//! 17. Disaster recovery restore workflow: atomic snapshot and WAL replay into clean coordinator directory.
//! 18. Disaster recovery restore anti-rollback guard: rejects older commit index unless forced.
//! 19. Disaster recovery restore forced rollback override succeeds when requested.
//! 20. QuorumMutationPermit GC / DeleteShard rejected if generation <= retention watermark.
//! 21. QuorumMutationPermit GC / DeleteShard rejected if generation is protected by recovery checkpoint.
//! 22. QuorumMutationPermit GC / DeleteShard rejected if attempting to delete current active generation.
//! 23. QuorumMutationPermit cross-node destination mismatch rejected.
//! 24. Observability recovery metrics collection and Prometheus exposition text rendering.
//! 25. End-to-end disaster recovery pipeline: PUT object -> take backup -> coordinator loss -> restore -> verify.

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
use fsp_sim::coordinator::replication::{
    LogEntry, MetadataSnapshot, ReplicatedLogManager, ReplicationRecord,
};
use fsp_sim::model::node::NodeId;
use fsp_sim::observability::MetricsCollector;
use fsp_sim::recovery::disaster_recovery::{
    execute_disaster_recovery_restore, DisasterRecoveryBackupBundle, DisasterRecoveryStatus,
};
use fsp_sim::security::recovery_auth::{
    execute_secure_emergency_read, RecoveryCapabilityToken, RecoveryFreshnessStatus,
    RecoveryTrustStore, SecureEmergencyReadOptions,
};
use fsp_sim::security::signatures::{encode_hex, ManifestSignerKeypair, SignedObjectManifest};
use fsp_sim::storage::container::write_shard_container_atomic;
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::storage::manifest::{
    ManifestCodingSpec, ObjectManifest, ShardPlacementRecord, StripeManifest,
};

static TEST_DIR_COUNTER: AtomicU64 = AtomicU64::new(6000);

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(prefix: &str) -> Self {
        let seq = TEST_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("fsp_p5_1_test_{}_{}", prefix, seq));
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
// Test 1: Authenticated Emergency Read Success
// -------------------------------------------------------------------------------------------------
#[test]
fn test_01_secure_emergency_read_authenticated_success() {
    let sandbox = TestDir::new("emergency_auth_ok");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9001);
    let principal_signer = create_deterministic_signer("recovery_admin", 9002);

    let (original_payload, _signed_manifest) =
        setup_synthetic_encoded_object(&roots, "obj_critical_01", 8192, &signer, 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_critical_01",
        "default_ns",
        "recovery_admin",
        1700000000 + 3600,
        principal_signer.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal_signer.verifying_key());
    trust_store.set_checkpoint("obj_critical_01", 1);

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_critical_01".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report =
        execute_secure_emergency_read(&opts).expect("secure emergency read should succeed");
    assert_eq!(report.object_id, "obj_critical_01");
    assert_eq!(report.object_size, 8192);
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::AuthenticLatestCommitted
    );

    let read_back = fs::read(&out_file).unwrap();
    assert_eq!(read_back, original_payload);
}

// -------------------------------------------------------------------------------------------------
// Test 2: Expired Capability Token Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_02_secure_emergency_read_expired_token_rejected() {
    let sandbox = TestDir::new("expired_token");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9011);
    let principal_signer = create_deterministic_signer("recovery_admin", 9012);

    setup_synthetic_encoded_object(&roots, "obj_expired_02", 4096, &signer, 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_expired_02",
        "default_ns",
        "recovery_admin",
        1700000000, // Expired at 1700000000
        principal_signer.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal_signer.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_expired_02".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000100, // 100 seconds later
    };

    let err = execute_secure_emergency_read(&opts).unwrap_err();
    assert!(err.contains("Token expired"));
}

// -------------------------------------------------------------------------------------------------
// Test 3: Forged Capability Token Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_03_secure_emergency_read_forged_token_rejected() {
    let sandbox = TestDir::new("forged_token");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9021);
    let principal_signer = create_deterministic_signer("recovery_admin", 9022);
    let adversary = create_deterministic_signer("adversary", 9023);

    setup_synthetic_encoded_object(&roots, "obj_forged_03", 4096, &signer, 1);

    // Adversary signs token claiming to be recovery_admin
    let token = RecoveryCapabilityToken::issue(
        "obj_forged_03",
        "default_ns",
        "recovery_admin",
        1700005000,
        adversary.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal_signer.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_forged_03".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let err = execute_secure_emergency_read(&opts).unwrap_err();
    assert!(err.contains("Cryptographic signature verification failed"));
}

// -------------------------------------------------------------------------------------------------
// Test 4: Unknown Recovery Principal Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_04_secure_emergency_read_unknown_principal_rejected() {
    let sandbox = TestDir::new("unknown_principal");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9031);
    let stranger = create_deterministic_signer("stranger", 9032);

    setup_synthetic_encoded_object(&roots, "obj_unknown_04", 4096, &signer, 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_unknown_04",
        "default_ns",
        "stranger",
        1700005000,
        stranger.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    // "stranger" is NOT registered in trust_store

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_unknown_04".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let err = execute_secure_emergency_read(&opts).unwrap_err();
    assert!(err.contains("Unknown or untrusted recovery principal"));
}

// -------------------------------------------------------------------------------------------------
// Test 5: Namespace Mismatch Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_05_secure_emergency_read_namespace_mismatch_rejected() {
    let sandbox = TestDir::new("namespace_mismatch");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9041);
    let principal = create_deterministic_signer("recovery_admin", 9042);

    setup_synthetic_encoded_object(&roots, "obj_ns_05", 4096, &signer, 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_ns_05",
        "tenant_finance",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_ns_05".into(),
        namespace: "tenant_engineering".into(), // Namespace mismatch!
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let err = execute_secure_emergency_read(&opts).unwrap_err();
    assert!(err.contains("Token namespace mismatch"));
}

// -------------------------------------------------------------------------------------------------
// Test 6: Object ID Mismatch Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_06_secure_emergency_read_object_id_mismatch_rejected() {
    let sandbox = TestDir::new("obj_id_mismatch");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9051);
    let principal = create_deterministic_signer("recovery_admin", 9052);

    setup_synthetic_encoded_object(&roots, "obj_target_06", 4096, &signer, 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_other_token",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_target_06".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let err = execute_secure_emergency_read(&opts).unwrap_err();
    assert!(err.contains("Token object_id mismatch"));
}

// -------------------------------------------------------------------------------------------------
// Test 7: Tampered Manifest Signature Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_07_secure_emergency_read_tampered_manifest_rejected() {
    let sandbox = TestDir::new("tampered_manifest");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9061);
    let principal = create_deterministic_signer("recovery_admin", 9062);

    let (_payload, mut signed) =
        setup_synthetic_encoded_object(&roots, "obj_tampered_07", 4096, &signer, 1);

    // Tamper with manifest signature
    signed.signature_hex = format!("{:0<128}", "f");
    for root in &roots {
        let p = root
            .join("metadata")
            .join("manifests")
            .join("obj_tampered_07.sig.json");
        fs::write(&p, signed.to_json()).unwrap();
    }

    let token = RecoveryCapabilityToken::issue(
        "obj_tampered_07",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_tampered_07".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let err = execute_secure_emergency_read(&opts).unwrap_err();
    assert!(err.contains("No cryptographically valid SignedObjectManifest found"));
}

// -------------------------------------------------------------------------------------------------
// Test 8: Untrusted Manifest Signer Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_08_secure_emergency_read_untrusted_signer_rejected() {
    let sandbox = TestDir::new("untrusted_signer");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let rogue_signer = create_deterministic_signer("rogue_authority", 9071);
    let principal = create_deterministic_signer("recovery_admin", 9072);

    setup_synthetic_encoded_object(&roots, "obj_untrusted_08", 4096, &rogue_signer, 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_untrusted_08",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    // Only trust genuine authority, NOT rogue_authority
    let genuine_authority = create_deterministic_signer("genuine_authority", 9073);
    trust_store.register_manifest_signer("genuine_authority", genuine_authority.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_untrusted_08".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let err = execute_secure_emergency_read(&opts).unwrap_err();
    assert!(err.contains("No cryptographically valid SignedObjectManifest found"));
}

// -------------------------------------------------------------------------------------------------
// Test 9: Shard Hash Mismatch Detected and Filtered
// -------------------------------------------------------------------------------------------------
#[test]
fn test_09_secure_emergency_read_shard_hash_mismatch_detected_and_filtered() {
    let sandbox = TestDir::new("shard_hash_mismatch");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9081);
    let principal = create_deterministic_signer("recovery_admin", 9082);

    let (payload, _signed) =
        setup_synthetic_encoded_object(&roots, "obj_corrupt_09", 4096, &signer, 1);

    // Corrupt shard 0 and shard 1 in node 0 and node 1
    let shard0_path = roots[0]
        .join("objects")
        .join("obj_corrupt_09")
        .join("shard_00.fspc");
    let mut s0 = fs::read(&shard0_path).unwrap();
    let last = s0.len() - 1;
    s0[last] ^= 0xAA;
    fs::write(&shard0_path, s0).unwrap();

    let token = RecoveryCapabilityToken::issue(
        "obj_corrupt_09",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_corrupt_09".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert!(report.corrupted_shards_detected >= 1);
    let read_back = fs::read(&out_file).unwrap();
    assert_eq!(read_back, payload);
}

// -------------------------------------------------------------------------------------------------
// Test 10: Insufficient Shards Fails
// -------------------------------------------------------------------------------------------------
#[test]
fn test_10_secure_emergency_read_insufficient_shards_fails() {
    let sandbox = TestDir::new("insufficient_shards");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9091);
    let principal = create_deterministic_signer("recovery_admin", 9092);

    setup_synthetic_encoded_object(&roots, "obj_insufficient_10", 4096, &signer, 1);

    // Delete 10 nodes (leaving only 10 shards < 12 required)
    for root in roots.iter().take(10) {
        let obj_dir = root.join("objects").join("obj_insufficient_10");
        let _ = fs::remove_dir_all(&obj_dir);
    }

    let token = RecoveryCapabilityToken::issue(
        "obj_insufficient_10",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_insufficient_10".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let err = execute_secure_emergency_read(&opts).unwrap_err();
    assert!(err.contains("Insufficient valid surviving shards"));
}

// -------------------------------------------------------------------------------------------------
// Test 11: Freshness Authentic Latest
// -------------------------------------------------------------------------------------------------
#[test]
fn test_11_secure_emergency_read_freshness_authentic_latest() {
    let sandbox = TestDir::new("freshness_latest");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9101);
    let principal = create_deterministic_signer("recovery_admin", 9102);

    setup_synthetic_encoded_object(&roots, "obj_latest_11", 4096, &signer, 5);

    let token = RecoveryCapabilityToken::issue(
        "obj_latest_11",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());
    trust_store.set_checkpoint("obj_latest_11", 5);

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_latest_11".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(
        report.freshness_status,
        RecoveryFreshnessStatus::AuthenticLatestCommitted
    );
}

// -------------------------------------------------------------------------------------------------
// Test 12: Freshness Historical Version
// -------------------------------------------------------------------------------------------------
#[test]
fn test_12_secure_emergency_read_freshness_historical_version() {
    let sandbox = TestDir::new("freshness_historical");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9111);
    let principal = create_deterministic_signer("recovery_admin", 9112);

    // Object on disk is generation 3
    setup_synthetic_encoded_object(&roots, "obj_hist_12", 4096, &signer, 3);

    let token = RecoveryCapabilityToken::issue(
        "obj_hist_12",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());
    // Known latest committed checkpoint is 7
    trust_store.set_checkpoint("obj_hist_12", 7);

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_hist_12".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    match report.freshness_status {
        RecoveryFreshnessStatus::AuthenticHistoricalVersion {
            reconstructed_generation,
            known_latest_generation,
        } => {
            assert_eq!(reconstructed_generation, 3);
            assert_eq!(known_latest_generation, 7);
        }
        other => panic!("expected AuthenticHistoricalVersion, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 13: Freshness Unverifiable
// -------------------------------------------------------------------------------------------------
#[test]
fn test_13_secure_emergency_read_freshness_unverifiable() {
    let sandbox = TestDir::new("freshness_unverifiable");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9121);
    let principal = create_deterministic_signer("recovery_admin", 9122);

    setup_synthetic_encoded_object(&roots, "obj_unverifiable_13", 4096, &signer, 2);

    let token = RecoveryCapabilityToken::issue(
        "obj_unverifiable_13",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_unverifiable_13".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file,
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    match report.freshness_status {
        RecoveryFreshnessStatus::UnverifiableFreshness {
            manifest_generation,
        } => {
            assert_eq!(manifest_generation, 2);
        }
        other => panic!("expected UnverifiableFreshness, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 14: Multi-Stripe Byte Trimming
// -------------------------------------------------------------------------------------------------
#[test]
fn test_14_secure_emergency_read_multi_stripe_byte_trimming() {
    let sandbox = TestDir::new("multi_stripe_trim");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let signer = create_deterministic_signer("coord_authority", 9131);
    let principal = create_deterministic_signer("recovery_admin", 9132);

    // Exact non-stripe-aligned payload: 10,000 bytes across multiple 4096 stripes
    let (payload, _signed) =
        setup_synthetic_encoded_object(&roots, "obj_multi_14", 10_000, &signer, 1);

    let token = RecoveryCapabilityToken::issue(
        "obj_multi_14",
        "default_ns",
        "recovery_admin",
        1700005000,
        principal.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("coord_authority", signer.verifying_key());
    trust_store.register_recovery_principal("recovery_admin", principal.verifying_key());

    let out_file = sandbox.path().join("recovered.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_multi_14".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(report.object_size, 10_000);

    let read_back = fs::read(&out_file).unwrap();
    assert_eq!(read_back.len(), 10_000);
    assert_eq!(read_back, payload);
}

// -------------------------------------------------------------------------------------------------
// Test 15: Metadata Backup Bundle Creation & Signature
// -------------------------------------------------------------------------------------------------
#[test]
fn test_15_metadata_backup_bundle_creation_and_signature() {
    let operator = create_deterministic_signer("dr_operator", 9141);
    let mut gens = BTreeMap::new();
    gens.insert("obj_a".into(), 1u64);
    gens.insert("obj_b".into(), 4u64);

    let snapshot = MetadataSnapshot::new(100, 3, gens);
    let wal_entries = vec![LogEntry::new(
        3,
        101,
        ReplicationRecord::GenerationWatermarkAdvanced {
            object_id: "obj_a".into(),
            generation: 2,
        },
    )];
    let mut signers = BTreeMap::new();
    signers.insert("coord_01".into(), "abcd".into());

    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        3,
        101,
        101,
        snapshot,
        wal_entries,
        signers,
        operator.signing_key(),
    );

    assert!(bundle.verify(&operator.verifying_key()).is_ok());
}

// -------------------------------------------------------------------------------------------------
// Test 16: Metadata Backup Bundle Tamper Detection
// -------------------------------------------------------------------------------------------------
#[test]
fn test_16_metadata_backup_bundle_tamper_detection() {
    let operator = create_deterministic_signer("dr_operator", 9151);
    let mut gens = BTreeMap::new();
    gens.insert("obj_a".into(), 1u64);

    let snapshot = MetadataSnapshot::new(50, 2, gens);
    let mut bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        2,
        50,
        50,
        snapshot,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    // Tamper with commit index
    bundle.commit_index = 999;
    let err = bundle.verify(&operator.verifying_key()).unwrap_err();
    assert!(err.contains("Backup bundle digest mismatch"));
}

// -------------------------------------------------------------------------------------------------
// Test 17: Disaster Recovery Restore Success
// -------------------------------------------------------------------------------------------------
#[test]
fn test_17_disaster_recovery_restore_success() {
    let sandbox = TestDir::new("dr_restore_ok");
    let target_dir = sandbox.path().join("recovered_coord");

    let operator = create_deterministic_signer("dr_operator", 9161);
    let mut gens = BTreeMap::new();
    gens.insert("obj_res_01".into(), 10u64);

    let snapshot = MetadataSnapshot::new(20, 2, gens);
    let wal_entries = vec![LogEntry::new(
        2,
        21,
        ReplicationRecord::GenerationWatermarkAdvanced {
            object_id: "obj_res_01".into(),
            generation: 11,
        },
    )];

    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        2,
        21,
        21,
        snapshot,
        wal_entries,
        BTreeMap::new(),
        operator.signing_key(),
    );

    let report =
        execute_disaster_recovery_restore(&bundle, &operator.verifying_key(), &target_dir, false)
            .unwrap();

    assert_eq!(report.status, DisasterRecoveryStatus::Success);
    assert_eq!(report.restored_term, 2);
    assert_eq!(report.restored_commit_index, 20); // Snapshot index is 20
    assert_eq!(report.restored_objects_count, 1);
}

// -------------------------------------------------------------------------------------------------
// Test 18: Disaster Recovery Restore Stale Rollback Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_18_disaster_recovery_restore_stale_rollback_rejected() {
    let sandbox = TestDir::new("dr_rollback_reject");
    let target_dir = sandbox.path().join("active_coord");
    fs::create_dir_all(&target_dir).unwrap();

    // Existing directory already at commit index 50
    let mut gens = BTreeMap::new();
    gens.insert("obj_x".into(), 5u64);
    let existing_snap = MetadataSnapshot::new(50, 4, gens);
    let snap_path = target_dir.join(ReplicatedLogManager::SNAPSHOT_FILE_NAME);
    fs::write(&snap_path, existing_snap.encode()).unwrap();

    let operator = create_deterministic_signer("dr_operator", 9171);

    // Stale bundle at commit index 30
    let mut stale_gens = BTreeMap::new();
    stale_gens.insert("obj_x".into(), 2u64);
    let stale_snap = MetadataSnapshot::new(30, 2, stale_gens);

    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        2,
        30,
        30,
        stale_snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    let report =
        execute_disaster_recovery_restore(&bundle, &operator.verifying_key(), &target_dir, false)
            .unwrap();

    match report.status {
        DisasterRecoveryStatus::RollbackRejected {
            existing_commit_index,
            bundle_commit_index,
        } => {
            assert_eq!(existing_commit_index, 50);
            assert_eq!(bundle_commit_index, 30);
        }
        other => panic!("expected RollbackRejected, got {:?}", other),
    }
}

// -------------------------------------------------------------------------------------------------
// Test 19: Disaster Recovery Restore Force Override
// -------------------------------------------------------------------------------------------------
#[test]
fn test_19_disaster_recovery_restore_force_override() {
    let sandbox = TestDir::new("dr_force_override");
    let target_dir = sandbox.path().join("coord_override");
    fs::create_dir_all(&target_dir).unwrap();

    let existing_snap = MetadataSnapshot::new(50, 4, BTreeMap::new());
    let snap_path = target_dir.join(ReplicatedLogManager::SNAPSHOT_FILE_NAME);
    fs::write(&snap_path, existing_snap.encode()).unwrap();

    let operator = create_deterministic_signer("dr_operator", 9181);
    let override_snap = MetadataSnapshot::new(30, 2, BTreeMap::new());

    let bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        2,
        30,
        30,
        override_snap,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    let report =
        execute_disaster_recovery_restore(&bundle, &operator.verifying_key(), &target_dir, true)
            .unwrap();

    assert_eq!(report.status, DisasterRecoveryStatus::Success);
}

// -------------------------------------------------------------------------------------------------
// Test 20: Quorum Permit GC Protected by Retention Watermark
// -------------------------------------------------------------------------------------------------
#[test]
fn test_20_quorum_permit_gc_protected_by_retention_watermark() {
    let sandbox = TestDir::new("retention_watermark");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    // Set retention watermark for obj_retain: generations <= 3 are retained
    tracker.set_retention_watermark("obj_retain", 3);

    let v1 = CoordinatorId::new("v1");
    let signer = create_deterministic_signer("v1", 9191);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(v1.clone(), signer.verifying_key());

    let grant = QuorumPermitGrant::create_v2(
        v1,
        1,
        10,
        "req_gc_01",
        "obj_retain",
        0,
        2, // Generation 2 <= retention watermark 3!
        "hash0",
        NodeId(1),
        PermitOperationType::GarbageCollectShard,
        signer.signing_key(),
    );

    let permit = QuorumMutationPermit::new_v2(
        1,
        10,
        "req_gc_01",
        "obj_retain",
        0,
        2,
        "hash0",
        1700000000,
        NodeId(1),
        PermitOperationType::GarbageCollectShard,
        vec![grant],
    );

    let err = tracker
        .verify_and_apply_mutation_permit_v2(
            &permit,
            "obj_retain",
            0,
            "hash0",
            NodeId(1),
            PermitOperationType::GarbageCollectShard,
            &voter_keys,
            1,
        )
        .unwrap_err();

    let err_str = format!("{:?}", err);
    assert!(err_str.contains("retention watermark"));
}

// -------------------------------------------------------------------------------------------------
// Test 21: Quorum Permit GC Protected by Recovery Checkpoint
// -------------------------------------------------------------------------------------------------
#[test]
fn test_21_quorum_permit_gc_protected_by_recovery_checkpoint() {
    let sandbox = TestDir::new("checkpoint_protection");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    // Protect recovery checkpoint (obj_ckpt, generation 4)
    tracker.protect_checkpoint("obj_ckpt", 4);
    assert!(tracker.is_checkpoint_protected("obj_ckpt", 4));

    let v1 = CoordinatorId::new("v1");
    let signer = create_deterministic_signer("v1", 9201);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(v1.clone(), signer.verifying_key());

    let grant = QuorumPermitGrant::create_v2(
        v1,
        1,
        10,
        "req_gc_02",
        "obj_ckpt",
        0,
        4, // Attempting to delete checkpointed generation 4!
        "hash0",
        NodeId(1),
        PermitOperationType::DeleteShard,
        signer.signing_key(),
    );

    let permit = QuorumMutationPermit::new_v2(
        1,
        10,
        "req_gc_02",
        "obj_ckpt",
        0,
        4,
        "hash0",
        1700000000,
        NodeId(1),
        PermitOperationType::DeleteShard,
        vec![grant],
    );

    let err = tracker
        .verify_and_apply_mutation_permit_v2(
            &permit,
            "obj_ckpt",
            0,
            "hash0",
            NodeId(1),
            PermitOperationType::DeleteShard,
            &voter_keys,
            1,
        )
        .unwrap_err();

    let err_str = format!("{:?}", err);
    assert!(err_str.contains("protected by active recovery checkpoint"));
}

// -------------------------------------------------------------------------------------------------
// Test 22: Quorum Permit GC Cannot Delete Active Committed Generation
// -------------------------------------------------------------------------------------------------
#[test]
fn test_22_quorum_permit_gc_cannot_delete_active_committed_generation() {
    let sandbox = TestDir::new("active_gen_delete");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    let v1 = CoordinatorId::new("v1");
    let signer = create_deterministic_signer("v1", 9211);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(v1.clone(), signer.verifying_key());

    // Advance active generation to 5 via a StageShard permit
    let stage_grant = QuorumPermitGrant::create_v2(
        v1.clone(),
        1,
        5,
        "req_stage_01",
        "obj_active",
        0,
        5,
        "hash0",
        NodeId(1),
        PermitOperationType::StageShard,
        signer.signing_key(),
    );
    let stage_permit = QuorumMutationPermit::new_v2(
        1,
        5,
        "req_stage_01",
        "obj_active",
        0,
        5,
        "hash0",
        1700000000,
        NodeId(1),
        PermitOperationType::StageShard,
        vec![stage_grant],
    );
    tracker
        .verify_and_apply_mutation_permit_v2(
            &stage_permit,
            "obj_active",
            0,
            "hash0",
            NodeId(1),
            PermitOperationType::StageShard,
            &voter_keys,
            1,
        )
        .unwrap();

    // Now attempt to delete generation 5 (which is the current active generation)
    let del_grant = QuorumPermitGrant::create_v2(
        v1,
        1,
        6,
        "req_del_active",
        "obj_active",
        0,
        5,
        "hash0",
        NodeId(1),
        PermitOperationType::DeleteShard,
        signer.signing_key(),
    );
    let del_permit = QuorumMutationPermit::new_v2(
        1,
        6,
        "req_del_active",
        "obj_active",
        0,
        5,
        "hash0",
        1700000000,
        NodeId(1),
        PermitOperationType::DeleteShard,
        vec![del_grant],
    );

    let err = tracker
        .verify_and_apply_mutation_permit_v2(
            &del_permit,
            "obj_active",
            0,
            "hash0",
            NodeId(1),
            PermitOperationType::DeleteShard,
            &voter_keys,
            1,
        )
        .unwrap_err();

    let err_str = format!("{:?}", err);
    assert!(err_str.contains("cannot delete active committed generation"));
}

// -------------------------------------------------------------------------------------------------
// Test 23: Quorum Permit Destination Node Mismatch Rejected
// -------------------------------------------------------------------------------------------------
#[test]
fn test_23_quorum_permit_node_mismatch_rejected() {
    let sandbox = TestDir::new("node_mismatch");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    tracker.check_and_update(1).unwrap();

    let v1 = CoordinatorId::new("v1");
    let signer = create_deterministic_signer("v1", 9221);
    let mut voter_keys = BTreeMap::new();
    voter_keys.insert(v1.clone(), signer.verifying_key());

    // Permit issued specifically to NodeId(1)
    let grant = QuorumPermitGrant::create_v2(
        v1,
        1,
        5,
        "req_node_bind",
        "obj_bound",
        0,
        1,
        "hash0",
        NodeId(1),
        PermitOperationType::StageShard,
        signer.signing_key(),
    );
    let permit = QuorumMutationPermit::new_v2(
        1,
        5,
        "req_node_bind",
        "obj_bound",
        0,
        1,
        "hash0",
        1700000000,
        NodeId(1),
        PermitOperationType::StageShard,
        vec![grant],
    );

    // Present permit against NodeId(2)
    let err = tracker
        .verify_and_apply_mutation_permit_v2(
            &permit,
            "obj_bound",
            0,
            "hash0",
            NodeId(2), // Mismatch!
            PermitOperationType::StageShard,
            &voter_keys,
            1,
        )
        .unwrap_err();

    let err_str = format!("{:?}", err);
    assert!(err_str.contains("destination node mismatch"));
}

// -------------------------------------------------------------------------------------------------
// Test 24: Observability Recovery Metrics Exposition
// -------------------------------------------------------------------------------------------------
#[test]
fn test_24_observability_recovery_metrics_exposition() {
    let collector = MetricsCollector::new();

    collector
        .recovery
        .emergency_reads_active
        .store(2, Ordering::SeqCst);
    collector
        .recovery
        .emergency_reads_total
        .fetch_add(5, Ordering::SeqCst);
    collector
        .recovery
        .recovery_freshness_authentic_latest
        .fetch_add(4, Ordering::SeqCst);
    collector
        .recovery
        .last_metadata_backup_timestamp_unix_secs
        .store(1700001234, Ordering::SeqCst);

    let output = collector.render_prometheus_metrics();
    assert!(output.contains("fsp_recovery_emergency_reads_active 2"));
    assert!(output.contains("fsp_recovery_emergency_reads_total{status=\"total\"} 5"));
    assert!(output
        .contains("fsp_recovery_freshness_classifications_total{status=\"authentic_latest\"} 4"));
    assert!(output.contains("fsp_recovery_backup_last_timestamp_seconds 1700001234"));
}

// -------------------------------------------------------------------------------------------------
// Test 25: End-to-End Disaster Recovery Pipeline
// -------------------------------------------------------------------------------------------------
#[test]
fn test_25_end_to_end_disaster_recovery_pipeline() {
    let sandbox = TestDir::new("e2e_dr_pipeline");
    let roots: Vec<PathBuf> = (0..20)
        .map(|i| {
            let r = sandbox.path().join(format!("node_{:02}", i));
            fs::create_dir_all(&r).unwrap();
            r
        })
        .collect();

    let coord_signer = create_deterministic_signer("primary_coordinator", 9241);
    let operator = create_deterministic_signer("dr_operator", 9242);
    let recovery_admin = create_deterministic_signer("admin_user", 9243);

    // 1. Stage object across 20 nodes with generation 1
    let (payload, signed_manifest) =
        setup_synthetic_encoded_object(&roots, "obj_e2e_pipeline", 16384, &coord_signer, 1);

    // 2. Build metadata snapshot and WAL backup bundle
    let mut gens = BTreeMap::new();
    gens.insert(
        "obj_e2e_pipeline".into(),
        signed_manifest.manifest_generation,
    );

    let snapshot = MetadataSnapshot::new(10, 1, gens);
    let wal_entries = vec![LogEntry::new(
        1,
        11,
        ReplicationRecord::GenerationWatermarkAdvanced {
            object_id: "obj_e2e_pipeline".into(),
            generation: 1,
        },
    )];

    let mut signers = BTreeMap::new();
    signers.insert(
        "primary_coordinator".into(),
        encode_hex(coord_signer.verifying_key().as_bytes()),
    );

    let backup_bundle = DisasterRecoveryBackupBundle::create(
        "dr_operator",
        1,
        11,
        11,
        snapshot,
        wal_entries,
        signers,
        operator.signing_key(),
    );

    let backup_file = sandbox.path().join("cluster_backup.bundle");
    backup_bundle.save_to_file(&backup_file).unwrap();

    // 3. Simulate total coordinator loss by restoring into a new coordinator target directory
    let new_coord_dir = sandbox.path().join("restored_coordinator");
    let restore_report = execute_disaster_recovery_restore(
        &backup_bundle,
        &operator.verifying_key(),
        &new_coord_dir,
        false,
    )
    .unwrap();

    assert_eq!(restore_report.status, DisasterRecoveryStatus::Success);
    assert_eq!(restore_report.restored_commit_index, 10);
    assert_eq!(restore_report.restored_objects_count, 1);

    // 4. Execute secure emergency read against surviving storage nodes
    let token = RecoveryCapabilityToken::issue(
        "obj_e2e_pipeline",
        "default_ns",
        "admin_user",
        1700005000,
        recovery_admin.signing_key(),
    );

    let mut trust_store = RecoveryTrustStore::new();
    trust_store.register_manifest_signer("primary_coordinator", coord_signer.verifying_key());
    trust_store.register_recovery_principal("admin_user", recovery_admin.verifying_key());
    trust_store.set_checkpoint("obj_e2e_pipeline", 1);

    let out_file = sandbox.path().join("e2e_reconstructed.dat");
    let opts = SecureEmergencyReadOptions {
        object_id: "obj_e2e_pipeline".into(),
        namespace: "default_ns".into(),
        token,
        trust_store,
        storage_roots: roots,
        output_path: out_file.clone(),
        current_time_unix_secs: 1700000000,
    };

    let read_report = execute_secure_emergency_read(&opts).unwrap();
    assert_eq!(read_report.object_id, "obj_e2e_pipeline");
    assert_eq!(read_report.object_size, 16384);
    assert_eq!(
        read_report.freshness_status,
        RecoveryFreshnessStatus::AuthenticLatestCommitted
    );

    let reconstructed_bytes = fs::read(&out_file).unwrap();
    assert_eq!(reconstructed_bytes, payload);
}
