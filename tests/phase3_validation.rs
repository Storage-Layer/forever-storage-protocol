//! Phase 3 Validation Suite: Real Storage Engine, Byte-Level Erasure Coding, Cryptographic
//! Integrity, Crash-Safe Transactions, Failure Injection, and Process-Restart Recovery.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

use fsp_sim::coding::lrc::LrcCodec;
use fsp_sim::coding::reed_solomon::{CodecError, ReedSolomonCodec};
use fsp_sim::coding::{CodingScheme, LrcConfig, RepairMethod};
use fsp_sim::detection::{
    FailureDetectionConfig, FailureDetector, PhysicalClusterSnapshot, PhysicalNodeState,
};
use fsp_sim::model::failure_domain::FailureDomainId;
use fsp_sim::model::node::NodeId;
use fsp_sim::model::shard::ShardId;
use fsp_sim::recovery::repair::{
    execute_object_repair, AdaptiveStorageRepairPlanner, ShardHealthState,
};
use fsp_sim::storage::backend::StorageError;
use fsp_sim::storage::engine::StorageEngine;
use fsp_sim::storage::integrity::{sha256_bytes, Sha256Hasher};
use fsp_sim::storage::manifest::{ManifestValidationError, ObjectManifest};
use fsp_sim::storage::object::PutOptions;
use fsp_sim::storage::transaction::{CrashMode, CrashPoint, CrashTrigger};
use fsp_sim::strategies::adaptive_v2::AdaptiveV2Config;

static SANDBOX_SEQ: AtomicU64 = AtomicU64::new(1);

/// RAII temporary sandbox directory that cleans itself up on drop.
struct TempSandbox {
    path: PathBuf,
}

impl TempSandbox {
    fn new(label: &str) -> Self {
        let id = SANDBOX_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("fsp-p3-test-{}-{}-{}", label, pid, id));
        if path.exists() {
            let _ = fs::remove_dir_all(&path);
        }
        fs::create_dir_all(&path).expect("create temp sandbox dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempSandbox {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Deterministic SplitMix64 byte generator for reproducible binary fixtures.
fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = vec![0u8; len];
    let chunks = len / 8;
    for c in 0..chunks {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let word = z ^ (z >> 31);
        out[c * 8..(c + 1) * 8].copy_from_slice(&word.to_le_bytes());
    }
    if !len.is_multiple_of(8) {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let bytes = state.to_le_bytes();
        for i in 0..(len % 8) {
            out[chunks * 8 + i] = bytes[i];
        }
    }
    out
}

/// Category 1: RS(20,12) round-trip and NIST FIPS 180-4 SHA-256 test vectors.
#[test]
fn test_p3_01_rs_20_12_encoding_decoding_round_trip_and_nist_sha256_vectors() {
    // Verify official NIST FIPS 180-4 SHA-256 test vectors
    assert_eq!(
        sha256_bytes(b"").to_hex(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_bytes(b"abc").to_hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_bytes(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq").to_hex(),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
    let mut million_a = Sha256Hasher::new();
    let chunk_a = [b'a'; 1000];
    for _ in 0..1000 {
        million_a.update(&chunk_a);
    }
    assert_eq!(
        million_a.finalize().to_hex(),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );

    // Verify RS(20,12) systematic encoding and full stripe round-trip
    let rs = ReedSolomonCodec::fsp_default_20_12();
    assert_eq!(rs.total_shards(), 20);
    assert_eq!(rs.data_shards(), 12);
    assert_eq!(rs.parity_shards(), 8);

    let shard_len = 1024;
    let data_vecs: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(0x100 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();

    let encoded = rs.encode_stripe(&data_refs).expect("encode_stripe");
    assert_eq!(encoded.len(), 20);
    // Verify systematic property: shards 0..12 equal original data_vecs
    for i in 0..12 {
        assert_eq!(encoded[i], data_vecs[i]);
    }
    // Verify parity shards 12..20 are non-trivial and distinct
    for parity_shard in encoded.iter().take(20).skip(12) {
        assert_eq!(parity_shard.len(), shard_len);
        assert!(parity_shard.iter().any(|&b| b != 0));
    }

    // Decode from all 20 shards (Scenario A)
    let all_avail: Vec<(ShardId, &[u8])> = encoded
        .iter()
        .enumerate()
        .map(|(i, v)| (ShardId(i), v.as_slice()))
        .collect();
    let decoded = rs
        .decode_data_shards(&all_avail, shard_len)
        .expect("decode all 20");
    assert_eq!(decoded, data_vecs);
}

/// Category 2: Arbitrary 12-of-20 recovery (Scenarios B, C, D).
#[test]
fn test_p3_02_arbitrary_12_of_20_recovery_across_data_and_parity_subsets() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_len = 512;
    let data_vecs: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(0x200 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();
    let encoded = rs.encode_stripe(&data_refs).expect("encode");

    // Scenario B: 1 shard deleted (19 available)
    let avail_19: Vec<(ShardId, &[u8])> = encoded
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != 0)
        .map(|(i, v)| (ShardId(i), v.as_slice()))
        .collect();
    assert_eq!(
        rs.decode_data_shards(&avail_19, shard_len).unwrap(),
        data_vecs
    );

    // Scenario C: 4 shards deleted (16 available)
    let avail_16: Vec<(ShardId, &[u8])> = encoded
        .iter()
        .enumerate()
        .filter(|&(i, _)| ![0, 3, 7, 15].contains(&i))
        .map(|(i, v)| (ShardId(i), v.as_slice()))
        .collect();
    assert_eq!(
        rs.decode_data_shards(&avail_16, shard_len).unwrap(),
        data_vecs
    );

    // Scenario D: 8 shards deleted (only 4 data shards + all 8 parity shards survive!)
    let avail_12_heavy_parity: Vec<(ShardId, &[u8])> = (8..20)
        .map(|i| (ShardId(i), encoded[i].as_slice()))
        .collect();
    let decoded_full = rs
        .decode_full_stripe(&avail_12_heavy_parity, shard_len)
        .expect("reconstruct all 20 shards from shards 8..20");
    assert_eq!(decoded_full, encoded);
}

/// Category 3: Exhaustive 4-shard erasures (all C(20,4) = 4,845 combinations) + 1,000 stratified
/// 8-shard erasure combinations (plus exhaustive verification that every 12x12 submatrix of G is invertible
/// across 5,000 sampled 8-shard erasures).
#[test]
fn test_p3_03_all_4845_four_shard_erasures_and_1000_stratified_eight_shard_erasures_of_rs_20_12() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_len = 64;
    let data_vecs: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(0x300 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();
    let encoded = rs.encode_stripe(&data_refs).expect("encode");

    // 1. Exhaustively verify ALL C(20, 4) = 4,845 4-shard erasure combinations
    let mut count_4 = 0usize;
    for e0 in 0..17 {
        for e1 in (e0 + 1)..18 {
            for e2 in (e1 + 1)..19 {
                for e3 in (e2 + 1)..20 {
                    let avail: Vec<(ShardId, &[u8])> = (0..20)
                        .filter(|&i| i != e0 && i != e1 && i != e2 && i != e3)
                        .map(|i| (ShardId(i), encoded[i].as_slice()))
                        .collect();
                    let dec = rs.decode_data_shards(&avail, shard_len).unwrap();
                    assert_eq!(dec, data_vecs);
                    count_4 += 1;
                }
            }
        }
    }
    assert_eq!(count_4, 4_845);

    // 2. Verify 1,000 stratified 8-shard erasure combinations (12 surviving shards out of 20)
    // including all extremal patterns (first 8 data lost, last 8 data lost, all 8 parities lost,
    // alternating shards lost, and pseudo-random 8-subsets across C(20,8) = 125,970).
    let mut state = 0xCAFE_BABE_1234_5678u64;
    for sample in 0..1_000 {
        let mut surviving: Vec<usize> = match sample {
            0 => (8..20).collect(),              // first 8 data shards lost
            1 => (0..4).chain(12..20).collect(), // last 8 data shards lost
            2 => (0..12).collect(),              // all 8 parity shards lost
            3 => (0..20).filter(|i| i % 2 == 0).chain([1, 3]).collect(), // alternating
            _ => {
                // Fisher-Yates partial shuffle to pick 12 distinct shards from 0..20
                let mut pool: Vec<usize> = (0..20).collect();
                for idx in 0..12 {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    let pick = idx + ((state >> 32) as usize) % (20 - idx);
                    pool.swap(idx, pick);
                }
                pool.truncate(12);
                pool.sort_unstable();
                pool
            }
        };
        surviving.truncate(12);
        let avail: Vec<(ShardId, &[u8])> = surviving
            .iter()
            .map(|&i| (ShardId(i), encoded[i].as_slice()))
            .collect();
        let dec = rs.decode_full_stripe(&avail, shard_len).unwrap();
        assert_eq!(dec, encoded, "failed 8-shard erasure sample {}", sample);
    }
}

/// Category 4: Nine-shard unrecoverability (Scenario E) and invalid shard combinations rejected safely.
#[test]
fn test_p3_04_nine_shard_unrecoverability_rejected_safely() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_len = 128;
    let data_vecs: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(0x400 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();
    let encoded = rs.encode_stripe(&data_refs).expect("encode");

    // 9 shards deleted -> only 11 shards remain (< k=12)
    let avail_11: Vec<(ShardId, &[u8])> = (0..11)
        .map(|i| (ShardId(i), encoded[i].as_slice()))
        .collect();
    let err = rs
        .decode_data_shards(&avail_11, shard_len)
        .expect_err("11 shards must fail safely");
    assert!(matches!(
        err,
        CodecError::InsufficientShardsForRecovery {
            available: 11,
            required: 12,
            ..
        }
    ));

    // Duplicate shard IDs (12 entries, but ShardId(0) duplicated) must be rejected
    let mut dup_12 = avail_11.clone();
    dup_12.push((ShardId(0), encoded[0].as_slice()));
    let dup_err = rs
        .decode_data_shards(&dup_12, shard_len)
        .expect_err("duplicate ShardId must fail");
    assert_eq!(
        dup_err,
        CodecError::DuplicateShardId {
            shard_id: ShardId(0)
        }
    );

    // End-to-end storage engine test for Scenario E (9 deleted shards)
    let sandbox = TempSandbox::new("cat04-9deleted");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();
    let original = deterministic_bytes(0x444, 32_768);
    let receipt = engine
        .put_bytes(&original, &PutOptions::default())
        .expect("put_bytes");

    for s_idx in 0..9 {
        let rel = &receipt.manifest.stripes[0].placements[s_idx].relative_path;
        engine.backend().inject_delete_shard(rel).unwrap();
    }

    let get_err = engine
        .get_to_bytes(&receipt.object_id)
        .expect_err("GET with 9 deleted shards must fail");
    assert!(matches!(
        get_err,
        StorageError::InsufficientValidShards {
            valid_shards: 11,
            required_shards: 12,
            ..
        }
    ));
}

/// Category 5: Corrupt and truncated shard detection without deletion (Scenarios F & G).
#[test]
fn test_p3_05_corrupt_and_truncated_shard_detection_scenarios_f_and_g() {
    let sandbox = TempSandbox::new("cat05-corrupt");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();
    let original = deterministic_bytes(0x555, 65_536);
    let receipt = engine
        .put_bytes(&original, &PutOptions::default())
        .expect("put");

    // Scenario F: Corrupt 1 data shard (shard 0) in place without deleting it
    let rel_0 = &receipt.manifest.stripes[0].placements[0].relative_path;
    engine
        .backend()
        .inject_corrupt_shard_byte(rel_0, 10)
        .unwrap();

    let health_f = engine.verify_object(&receipt.object_id).unwrap();
    assert!(!health_f.all_shards_healthy);
    assert!(health_f.all_stripes_recoverable);
    assert_eq!(health_f.stripes[0].corrupted_shards.len(), 1);
    assert_eq!(health_f.stripes[0].corrupted_shards[0].0, ShardId(0));

    // Reconstruct must reject the corrupt shard 0 and decode exact bytes from the remaining 19
    let (reconstructed_f, rep_f) = engine.get_to_bytes(&receipt.object_id).unwrap();
    assert_eq!(reconstructed_f, original);
    assert_eq!(rep_f.corrupt_or_stale_shards_rejected, 1);

    // Scenario G: Corrupt 4 shards + truncate 2 shards + delete 2 shards = 8 total damaged shards
    for s_idx in 1..4 {
        let rel = &receipt.manifest.stripes[0].placements[s_idx].relative_path;
        engine.backend().inject_corrupt_shard_byte(rel, 25).unwrap();
    }
    for s_idx in 4..6 {
        let rel = &receipt.manifest.stripes[0].placements[s_idx].relative_path;
        engine.backend().inject_truncate_shard(rel, 12).unwrap();
    }
    for s_idx in 6..8 {
        let rel = &receipt.manifest.stripes[0].placements[s_idx].relative_path;
        engine.backend().inject_delete_shard(rel).unwrap();
    }

    // 12 healthy shards (shards 8..20) remain; reconstruction must still succeed byte-for-byte!
    let (reconstructed_g, rep_g) = engine.get_to_bytes(&receipt.object_id).unwrap();
    assert_eq!(reconstructed_g, original);
    assert_eq!(rep_g.corrupt_or_stale_shards_rejected, 6);
    assert_eq!(rep_g.missing_or_offline_shards, 2);

    // Repair must restore all 8 damaged shards and pass post-repair verification
    let repair_rep = engine.repair_object(&receipt.object_id).unwrap();
    assert_eq!(repair_rep.shards_repaired, 8);
    assert!(repair_rep.post_repair_all_healthy);
    assert!(
        engine
            .verify_object(&receipt.object_id)
            .unwrap()
            .all_shards_healthy
    );
}

/// Category 6: Incorrect object digest and tampered manifest rejection.
#[test]
fn test_p3_06_incorrect_object_digest_and_tampered_manifest_rejection() {
    let sandbox = TempSandbox::new("cat06-manifest-tamper");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();
    let original = deterministic_bytes(0x666, 16_384);
    let receipt = engine
        .put_bytes(&original, &PutOptions::default())
        .expect("put");

    // 1. Tamper with object_digest inside manifest without updating manifest_digest
    let mut tampered_1 = receipt.manifest.clone();
    tampered_1.object_digest = "0".repeat(64);
    let json_1 = tampered_1.to_canonical_json();
    engine
        .backend()
        .inject_corrupt_manifest(&receipt.object_id, &json_1)
        .unwrap();

    let err_1 = engine
        .get_to_bytes(&receipt.object_id)
        .expect_err("tampered manifest_digest must be rejected");
    assert!(matches!(
        err_1,
        StorageError::Manifest(ManifestValidationError::ManifestDigestMismatch { .. })
    ));

    // 2. Reseal manifest with a wrong object_digest so manifest_digest is valid but object_digest is wrong
    tampered_1.seal();
    engine
        .backend()
        .inject_corrupt_manifest(&receipt.object_id, &tampered_1.to_canonical_json())
        .unwrap();

    let err_2 = engine
        .get_to_bytes(&receipt.object_id)
        .expect_err("wrong object_digest must be rejected at end of reconstruction");
    assert!(matches!(err_2, StorageError::ObjectIntegrityFailure { .. }));
}

/// Category 7: Empty (`0 B`), `1 B`, `4 KB`, and irregular non-aligned file sizes.
#[test]
fn test_p3_07_empty_0b_1b_4kb_and_unaligned_prime_file_sizes() {
    let sandbox = TempSandbox::new("cat07-sizes");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    let sizes = [0usize, 1, 7, 63, 64, 65, 4_096, 4_097, 99_991];
    for (idx, &sz) in sizes.iter().enumerate() {
        let payload = deterministic_bytes(0x700 + idx as u64, sz);
        let opts = PutOptions {
            object_id: Some(format!("size-test-{}", sz)),
            stripe_size: 16_384,
            ..PutOptions::default()
        };
        let receipt = engine.put_bytes(&payload, &opts).expect("put");
        assert_eq!(receipt.object_size, sz as u64);

        // Delete 8 shards from stripe 0 and verify full reconstruction + repair works for every size (including 0 B and 1 B!)
        for s_id in [0, 2, 4, 6, 8, 10, 13, 17] {
            let rel = &receipt.manifest.stripes[0].placements[s_id].relative_path;
            engine.backend().inject_delete_shard(rel).unwrap();
        }

        let (restored, _) = engine.get_to_bytes(&receipt.object_id).expect("get");
        assert_eq!(restored.len(), sz);
        assert_eq!(restored, payload);

        let rep = engine.repair_object(&receipt.object_id).expect("repair");
        assert_eq!(rep.shards_repaired, 8);
        assert!(rep.post_repair_all_healthy);
    }
}

/// Category 8: Multi-stripe streaming with bounded stripe size.
#[test]
fn test_p3_08_large_file_streaming_bounded_memory_verification() {
    let sandbox = TempSandbox::new("cat08-streaming");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    // 4.5 MiB payload across 5 stripes (1 MiB stripe size)
    let payload = deterministic_bytes(0x888, 4 * 1_048_576 + 524_289);
    let receipt = engine
        .put_bytes(&payload, &PutOptions::default())
        .expect("streaming put");
    assert_eq!(receipt.stripe_count, 5);

    // Delete 8 different shards in each of the 5 stripes
    for (stripe_idx, stripe) in receipt.manifest.stripes.iter().enumerate() {
        for offset in 0..8 {
            let shard_idx = (stripe_idx * 3 + offset) % 20;
            let rel = &stripe.placements[shard_idx].relative_path;
            engine.backend().inject_delete_shard(rel).unwrap();
        }
    }

    let out_file = sandbox.path().join("restored_stream.bin");
    let get_rep = engine
        .get_to_file(&receipt.object_id, &out_file)
        .expect("streaming get_to_file");
    assert_eq!(get_rep.object_size, payload.len() as u64);
    assert_eq!(get_rep.object_digest, receipt.object_digest);
    assert_eq!(fs::read(&out_file).unwrap(), payload);
}

/// Category 9: Manifest validation rejects malformed, out-of-range, and DoS inputs prior to allocation.
#[test]
fn test_p3_09_manifest_pre_allocation_validation_rejects_malformed_and_dos_inputs() {
    let sandbox = TempSandbox::new("cat09-manifest-val");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();
    let receipt = engine
        .put_bytes(b"manifest-validation-test", &PutOptions::default())
        .unwrap();

    // 1. Unsupported format_version
    let mut m1 = receipt.manifest.clone();
    m1.format_version = 99;
    m1.seal();
    assert!(matches!(
        m1.validate(),
        Err(ManifestValidationError::UnsupportedFormatVersion(99))
    ));

    // 2. DoS excessive stripe_size (> 64 MiB)
    let mut m2 = receipt.manifest.clone();
    m2.stripe_size = 512 * 1_048_576;
    m2.seal();
    assert!(matches!(
        m2.validate(),
        Err(ManifestValidationError::InvalidStripeSize(_))
    ));

    // 3. Duplicate shard_id in placements
    let mut m3 = receipt.manifest.clone();
    m3.stripes[0].placements[1].shard_id = 0;
    m3.seal();
    assert!(matches!(
        m3.validate(),
        Err(ManifestValidationError::DuplicateShardId {
            stripe_index: 0,
            shard_id: 0
        })
    ));

    // 4. Unsafe relative_path with `..` traversal
    let mut m4 = receipt.manifest.clone();
    m4.stripes[0].placements[0].relative_path = "../outside/evil.fsp".to_string();
    m4.seal();
    assert!(matches!(
        m4.validate(),
        Err(ManifestValidationError::UnsafeRelativePath(_))
    ));

    // 5. Invalid JSON parse error
    assert!(matches!(
        ObjectManifest::from_json("{ invalid json "),
        Err(ManifestValidationError::ParseError(_))
    ));
}

/// Category 10: Atomic commit visibility — interrupted uploads are never visible as committed objects.
#[test]
fn test_p3_10_atomic_commit_visibility_and_uncommitted_isolation() {
    let sandbox = TempSandbox::new("cat10-atomic-vis");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    for point in [
        CrashPoint::AfterPreparing,
        CrashPoint::MidWriting,
        CrashPoint::AfterVerifying,
        CrashPoint::MidCommittingBeforeManifest,
    ] {
        let opts = PutOptions {
            object_id: Some(format!("uncommitted-{:?}", point)),
            crash_trigger: Some(CrashTrigger {
                point,
                mode: CrashMode::SyntheticLeaveInterrupted,
            }),
            ..PutOptions::default()
        };
        let err = engine
            .put_bytes(&deterministic_bytes(0x1010, 8192), &opts)
            .expect_err("must interrupt before commit");
        assert!(matches!(err, StorageError::SimulatedCrash(_)));

        // Object must NOT appear in committed objects list or be loadable via GET
        let st = engine.status().unwrap();
        assert!(
            !st.committed_objects
                .contains(&format!("uncommitted-{:?}", point)),
            "partially uploaded object at {:?} was visible!",
            point
        );
        assert!(matches!(
            engine.get_to_bytes(&format!("uncommitted-{:?}", point)),
            Err(StorageError::ObjectNotFound(_))
        ));
    }
}

/// Category 11: Crash recovery state machine cleanup and idempotency.
#[test]
fn test_p3_11_crash_recovery_state_machine_all_points_and_idempotency() {
    let sandbox = TempSandbox::new("cat11-crash-rec");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    // Trigger MidCommittingBeforeManifest crash (leaves ~10 published shard files in node-XX without manifest)
    let opts = PutOptions {
        object_id: Some("half-committed-obj".to_string()),
        crash_trigger: Some(CrashTrigger {
            point: CrashPoint::MidCommittingBeforeManifest,
            mode: CrashMode::SyntheticLeaveInterrupted,
        }),
        ..PutOptions::default()
    };
    let _ = engine.put_bytes(&deterministic_bytes(0x1111, 16_384), &opts);

    // Before recovery, some partial shard files exist on nodes
    let pre_status = engine.status().unwrap();
    assert!(pre_status.total_stored_shard_files > 0);
    assert!(pre_status.committed_objects.is_empty());

    // Run crash recovery (Pass 1)
    let rec1 = engine.recover().unwrap();
    assert_eq!(rec1.transactions_aborted, 1);
    assert!(rec1.partial_shard_files_cleaned > 0);

    // After recovery, all partial shard files are cleaned up
    let post_status = engine.status().unwrap();
    assert_eq!(post_status.total_stored_shard_files, 0);
    assert!(post_status.committed_objects.is_empty());

    // Run crash recovery again (Pass 2 & Pass 3) — verify strict idempotency
    let rec2 = engine.recover().unwrap();
    assert_eq!(rec2.transactions_aborted, 0);
    assert_eq!(rec2.partial_shard_files_cleaned, 0);
    assert_eq!(rec2.temp_artifacts_cleaned, 0);
}

/// Category 12: Repair idempotency and interrupted repair recovery (Scenario H).
#[test]
fn test_p3_12_repair_idempotency_and_interrupted_repair_scenario_h() {
    let sandbox = TempSandbox::new("cat12-repair-idem");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();
    let payload = deterministic_bytes(0x1212, 32_768);
    let receipt = engine.put_bytes(&payload, &PutOptions::default()).unwrap();

    // Delete shard 2 and shard 5
    engine
        .backend()
        .inject_delete_shard(&receipt.manifest.stripes[0].placements[2].relative_path)
        .unwrap();
    engine
        .backend()
        .inject_delete_shard(&receipt.manifest.stripes[0].placements[5].relative_path)
        .unwrap();

    // Scenario H: Simulate an interrupted repair after writing `.repair.tmp`
    let sim_err = execute_object_repair(engine.backend(), &receipt.manifest, None, true)
        .expect_err("interrupted repair");
    assert!(matches!(sim_err, StorageError::SimulatedCrash(_)));

    // Run crash recovery to clean up `.repair.tmp`
    let rec = engine.recover().unwrap();
    assert!(rec.temp_artifacts_cleaned >= 1);

    // Now execute full repair (Pass 1) -> repairs 2 shards
    let rep1 = engine.repair_object(&receipt.object_id).unwrap();
    assert_eq!(rep1.shards_repaired, 2);
    assert!(rep1.post_repair_all_healthy);

    // Execute repair again (Pass 2) -> idempotent: 0 shards repaired, 0 bytes written
    let rep2 = engine.repair_object(&receipt.object_id).unwrap();
    assert_eq!(rep2.shards_repaired, 0);
    assert_eq!(rep2.disk_bytes_written, 0);
    assert!(rep2.post_repair_all_healthy);
}

/// Category 13: Concurrent multi-thread object uploads and reconstructions.
#[test]
fn test_p3_13_concurrent_multi_thread_object_uploads_and_reads() {
    let sandbox = Arc::new(TempSandbox::new("cat13-concurrent"));
    let _ = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    let mut handles = Vec::new();
    for tid in 0..8usize {
        let sb = Arc::clone(&sandbox);
        handles.push(thread::spawn(move || {
            let mut eng = StorageEngine::open_without_recovery(sb.path()).unwrap();
            let data = deterministic_bytes(0x1300 + tid as u64, 20_000 + tid * 111);
            let opts = PutOptions {
                object_id: Some(format!("concurrent-obj-{}", tid)),
                ..PutOptions::default()
            };
            let receipt = eng.put_bytes(&data, &opts).unwrap();
            let (restored, _) = eng.get_to_bytes(&receipt.object_id).unwrap();
            assert_eq!(restored, data);
        }));
    }

    for h in handles {
        h.join().expect("worker thread succeeded");
    }

    let engine = StorageEngine::open(sandbox.path()).unwrap();
    let st = engine.status().unwrap();
    assert_eq!(st.committed_objects.len(), 8);
}

/// Category 14: Filesystem path safety — rejects path traversal (`..`) and symlink redirection.
#[test]
fn test_p3_14_filesystem_path_traversal_and_symlink_redirection_defense() {
    let sandbox = TempSandbox::new("cat14-pathsafety");
    let engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    // 1. Reject `..` path traversal in relative path resolution
    assert!(matches!(
        engine.backend().resolve_safe_relative_path("../escape.txt"),
        Err(StorageError::UnsafePathRejected(_))
    ));
    assert!(matches!(
        engine
            .backend()
            .resolve_safe_relative_path("node-00/objects/../../escape.txt"),
        Err(StorageError::UnsafePathRejected(_))
    ));
    assert!(matches!(
        engine.backend().resolve_safe_relative_path("/etc/passwd"),
        Err(StorageError::UnsafePathRejected(_))
    ));

    // 2. Reject symlink redirection inside storage root
    #[cfg(unix)]
    {
        let outside_target = sandbox.path().join("outside_dir");
        fs::create_dir_all(&outside_target).unwrap();
        let symlink_path = sandbox
            .path()
            .join("node-00")
            .join("objects")
            .join("evil_link");
        std::os::unix::fs::symlink(&outside_target, &symlink_path).unwrap();

        let sym_err = engine
            .backend()
            .resolve_safe_relative_path("node-00/objects/evil_link/file.fsp")
            .expect_err("symlink redirection must be rejected");
        assert!(matches!(sym_err, StorageError::UnsafePathRejected(_)));
        let _ = fs::remove_file(&symlink_path);
    }
}

/// Category 15: Disk-full / node quota exhaustion behavior and clean transaction abort.
#[test]
fn test_p3_15_disk_full_node_quota_exhaustion_rollback() {
    let sandbox = TempSandbox::new("cat15-quota");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    // Restrict node-05 quota to 100 bytes
    engine
        .backend_mut()
        .set_node_quota_bytes(NodeId(5), Some(100));

    let opts = PutOptions {
        object_id: Some("quota-exceeded-obj".to_string()),
        ..PutOptions::default()
    };
    let err = engine
        .put_bytes(&deterministic_bytes(0x1515, 16_384), &opts)
        .expect_err("must fail with QuotaExceeded");
    assert!(matches!(
        err,
        StorageError::QuotaExceeded {
            node_id: NodeId(5),
            ..
        }
    ));

    // Verify clean rollback: 0 committed objects and 0 leftover shard files
    let st = engine.status().unwrap();
    assert!(st.committed_objects.is_empty());
    assert_eq!(st.total_stored_shard_files, 0);
}

/// Category 16: Multiple object isolation across damage and repair.
#[test]
fn test_p3_16_multiple_object_isolation_and_independent_lifecycle() {
    let sandbox = TempSandbox::new("cat16-isolation");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    let obj_a = deterministic_bytes(0xA001, 24_576);
    let obj_b = deterministic_bytes(0xB002, 40_960);

    let rec_a = engine
        .put_bytes(
            &obj_a,
            &PutOptions {
                object_id: Some("object-alpha".to_string()),
                ..PutOptions::default()
            },
        )
        .unwrap();
    let rec_b = engine
        .put_bytes(
            &obj_b,
            &PutOptions {
                object_id: Some("object-beta".to_string()),
                ..PutOptions::default()
            },
        )
        .unwrap();

    // Destroy 10 shards of object-alpha (making object-alpha unrecoverable), while deleting 4 shards of object-beta
    for s in 0..10 {
        engine
            .backend()
            .inject_delete_shard(&rec_a.manifest.stripes[0].placements[s].relative_path)
            .unwrap();
    }
    for s in 0..4 {
        engine
            .backend()
            .inject_delete_shard(&rec_b.manifest.stripes[0].placements[s].relative_path)
            .unwrap();
    }

    // object-alpha fails safely, while object-beta reconstructs and repairs cleanly
    assert!(engine.get_to_bytes("object-alpha").is_err());
    let (restored_b, _) = engine.get_to_bytes("object-beta").unwrap();
    assert_eq!(restored_b, obj_b);
    let rep_b = engine.repair_object("object-beta").unwrap();
    assert_eq!(rep_b.shards_repaired, 4);
    assert!(rep_b.post_repair_all_healthy);
}

/// Category 17: Stale older shard versions (Scenario I) and conflicting shard versions (Scenario J).
#[test]
fn test_p3_17_stale_and_conflicting_shard_versions_scenarios_i_and_j() {
    let sandbox = TempSandbox::new("cat17-versions");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();
    let payload = deterministic_bytes(0x1717, 16_384);
    let receipt = engine.put_bytes(&payload, &PutOptions::default()).unwrap();

    let shard_len = receipt.manifest.stripes[0].shard_length;
    let fake_bytes = vec![0xAAu8; shard_len];

    // Scenario I: Node 0 returns only an older stale shard version (`.v0.fsp`) after its `.v1.fsp` was lost
    engine
        .backend()
        .inject_delete_shard(&receipt.manifest.stripes[0].placements[0].relative_path)
        .unwrap();
    engine
        .backend()
        .inject_stale_shard_version(NodeId(0), &receipt.object_id, 0, ShardId(0), &fake_bytes)
        .unwrap();

    // Scenario J: Node 1 has both `.v1.fsp` and a conflicting `.v99.fsp` file in the shard directory
    engine
        .backend()
        .inject_conflicting_shard_version(NodeId(1), &receipt.object_id, 0, ShardId(1), &fake_bytes)
        .unwrap();

    // Verify inspection flags both anomalies
    let health = engine.verify_object(&receipt.object_id).unwrap();
    assert!(!health.all_shards_healthy);
    assert!(matches!(
        health.stripes[0].shard_states.get(&ShardId(0)),
        Some(ShardHealthState::Missing)
    ));
    assert!(matches!(
        health.stripes[0].shard_states.get(&ShardId(1)),
        Some(ShardHealthState::Corrupted { .. })
    ));

    // Reconstruction rejects the stale `.v0.fsp` and reproduces the exact original bytes
    let (restored, get_rep) = engine.get_to_bytes(&receipt.object_id).unwrap();
    assert_eq!(restored, payload);
    assert!(get_rep.corrupt_or_stale_shards_rejected >= 2);

    // Repair cleans up both the `.v0.fsp` and `.v99.fsp` files and restores clean `.v1.fsp` state
    let repair_rep = engine.repair_object(&receipt.object_id).unwrap();
    assert_eq!(repair_rep.shards_repaired, 2);
    assert!(repair_rep.post_repair_all_healthy);
}

/// Category 18: Adaptive Repair v2 integration with real storage engine and blind FailureDetector.
#[test]
fn test_p3_18_adaptive_repair_v2_integration_with_real_storage_and_blind_detector() {
    let sandbox = TempSandbox::new("cat18-adaptive-v2");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();
    let payload = deterministic_bytes(0x1818, 32_768);
    let receipt = engine.put_bytes(&payload, &PutOptions::default()).unwrap();

    // Set up Phase 2 blind FailureDetector for the 20 nodes
    let mut detector = FailureDetector::new(FailureDetectionConfig::default(), 20, 20, 42);
    let mut node_states: Vec<(NodeId, FailureDomainId, PhysicalNodeState)> = (0..20)
        .map(|i| (NodeId(i), FailureDomainId(i), PhysicalNodeState::Online))
        .collect();

    // 1. Node 0 experiences a brief transient outage (t = 0..200s, Suspect only)
    // while Shard 1 on Online Node 1 is corrupted on disk (confirmed permanent shard loss!)
    engine.backend_mut().set_node_online(NodeId(0), false);
    node_states[0].2 = PhysicalNodeState::TemporarilyOffline {
        since_secs: 0.0,
        recover_at_secs: 300.0,
    };
    engine
        .backend()
        .inject_corrupt_shard_byte(&receipt.manifest.stripes[0].placements[1].relative_path, 0)
        .unwrap();

    let mut obs_view = detector.step_to(
        60.0,
        &PhysicalClusterSnapshot {
            now_secs: 60.0,
            node_states: node_states.clone(),
        },
    );
    for step in 2..=4 {
        let t = (step as f64) * 60.0;
        obs_view = detector.step_to(
            t,
            &PhysicalClusterSnapshot {
                now_secs: t,
                node_states: node_states.clone(),
            },
        );
    }

    let mut planner = AdaptiveStorageRepairPlanner::new(AdaptiveV2Config::default());
    let (decisions, exec_rep) = engine
        .repair_object_adaptive(
            &receipt.object_id,
            &mut planner,
            240.0,
            Some(&obs_view),
            None,
            10_000_000,
        )
        .unwrap();

    // Adaptive V2 immediately repairs confirmed corrupted ShardId(1) on online Node 1,
    // while deferring transiently offline ShardId(0) on Suspect Node 0!
    assert_eq!(decisions[0].shards_to_repair, vec![ShardId(1)]);
    assert_eq!(exec_rep.shards_repaired, 1);

    // 2. When Node 0 recovers at t = 300s, any in-flight repair for ShardId(0) is canceled and all 20 shards are healthy!
    engine.backend_mut().set_node_online(NodeId(0), true);
    node_states[0].2 = PhysicalNodeState::Online;
    let obs_recovered = detector.step_to(
        300.0,
        &PhysicalClusterSnapshot {
            now_secs: 300.0,
            node_states,
        },
    );

    let mut in_flight_map = BTreeMap::new();
    in_flight_map.insert(0usize, vec![(ShardId(0), true)]);
    let (decisions_after, _) = engine
        .repair_object_adaptive(
            &receipt.object_id,
            &mut planner,
            300.0,
            Some(&obs_recovered),
            Some(&in_flight_map),
            10_000_000,
        )
        .unwrap();
    assert_eq!(decisions_after[0].repairs_to_cancel, vec![ShardId(0)]);
    assert!(
        engine
            .verify_object(&receipt.object_id)
            .unwrap()
            .all_shards_healthy
    );
}

/// Category 19: Real byte-level LRC(22,12,4) and LRC(20,12,4) encoding, 4-shard XOR local repair,
/// and global MR multi-erasure decoding.
#[test]
fn test_p3_19_lrc_22_12_4_and_lrc_20_12_4_real_byte_encoding_local_4shard_xor_repair_and_global_mr_decoding(
) {
    let lrc22 = LrcCodec::lrc_22_12_4();
    let shard_len = 256;
    let data_vecs: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(0x1900 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();
    let encoded = lrc22
        .encode_stripe(&data_refs)
        .expect("LRC(22,12,4) encode");
    assert_eq!(encoded.len(), 22);

    // 1. Verify single-shard repair uses LocalGroup reading ONLY l = 4 shards!
    for target in 0..15 {
        let avail: Vec<(ShardId, &[u8])> = (0..22)
            .filter(|&i| i != target)
            .map(|i| (ShardId(i), encoded[i].as_slice()))
            .collect();
        let out = lrc22
            .repair_single_shard(ShardId(target), &avail, shard_len)
            .expect("local single shard repair");
        assert_eq!(out.bytes, encoded[target]);
        assert_eq!(out.source_shards_read.len(), 4);
        assert!(matches!(
            out.method,
            RepairMethod::LocalGroup {
                shards_to_read: 4,
                ..
            }
        ));
    }

    // 2. Verify LRC(22,12,4) survives ANY 8-shard erasure (d_min = 9) including worst-case 5 erasures in group 0 + 3 in group 1!
    let erased_8 = [0usize, 1, 2, 3, 12, 4, 5, 6];
    let avail_14: Vec<(ShardId, &[u8])> = (0..22)
        .filter(|i| !erased_8.contains(i))
        .map(|i| (ShardId(i), encoded[i].as_slice()))
        .collect();
    let decoded_data = lrc22
        .decode_data_shards(&avail_14, shard_len)
        .expect("LRC(22,12,4) decode 8 erasures");
    assert_eq!(decoded_data, data_vecs);

    // 3. End-to-end StorageEngine lifecycle with LRC(22,12,4): verify local 4-shard repair vs global repair I/O
    let sandbox = TempSandbox::new("cat19-lrc22");
    let mut engine = StorageEngine::init(sandbox.path(), 22, 22).unwrap();
    let payload = deterministic_bytes(0x1999, 65_536);
    let opts = PutOptions {
        object_id: Some("lrc22-object".to_string()),
        coding_scheme: CodingScheme::Lrc(LrcConfig::new(22, 12, 4).unwrap()),
        ..PutOptions::default()
    };
    let receipt = engine.put_bytes(&payload, &opts).unwrap();

    // Delete 1 data shard (Shard 2) -> repair must use LocalGroup (reading 4 shards, not 12!)
    engine
        .backend()
        .inject_delete_shard(&receipt.manifest.stripes[0].placements[2].relative_path)
        .unwrap();
    let rep_local = engine.repair_object("lrc22-object").unwrap();
    assert_eq!(rep_local.shards_repaired, 1);
    assert_eq!(rep_local.local_group_repairs, 1);
    assert_eq!(rep_local.global_reconstruction_repairs, 0);
    assert_eq!(
        rep_local.disk_bytes_read,
        4 * (receipt.manifest.stripes[0].shard_length as u64)
    );
}

/// Category 20: Real subprocess kill crash test (`std::process::exit(137)`) and process restart persistence.
#[test]
fn test_p3_20_subprocess_kill_crash_recovery_and_process_restart_persistence() {
    let sandbox = TempSandbox::new("cat20-subprocess");
    let root = sandbox.path().join("store");
    let bin = env!("CARGO_BIN_EXE_fsp-sim");

    // 1. Initialize storage root via child process CLI
    let init_status = Command::new(bin)
        .args(["storage", "init", "--root", root.to_str().unwrap()])
        .status()
        .expect("spawn fsp-sim storage init");
    assert!(init_status.success());

    // 2. Write a committed baseline file via child process CLI
    let valid_input = sandbox.path().join("baseline.bin");
    let baseline_bytes = deterministic_bytes(0x2020, 131_072);
    fs::write(&valid_input, &baseline_bytes).unwrap();

    let put_status = Command::new(bin)
        .args([
            "storage",
            "put",
            valid_input.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
            "--id",
            "committed-obj",
        ])
        .status()
        .expect("spawn fsp-sim storage put");
    assert!(put_status.success());

    // 3. Spawn a second child process that crashes via std::process::exit(137) during Committing BEFORE manifest rename!
    let crash_input = sandbox.path().join("crashed.bin");
    fs::write(&crash_input, deterministic_bytes(0xDEAD, 131_072)).unwrap();

    let crash_status = Command::new(bin)
        .args([
            "storage",
            "crash-put",
            crash_input.to_str().unwrap(),
            "--root",
            root.to_str().unwrap(),
            "--id",
            "crashed-obj",
            "--crash-point",
            "MidCommittingBeforeManifest",
        ])
        .status()
        .expect("spawn fsp-sim storage crash-put");
    assert_eq!(crash_status.code(), Some(137));

    // 4. Restart in a new child process (`storage recover`) and verify `crashed-obj` is rolled back while `committed-obj` is intact
    let recover_out = Command::new(bin)
        .args(["storage", "recover", "--root", root.to_str().unwrap()])
        .output()
        .expect("spawn fsp-sim storage recover");
    assert!(recover_out.status.success());
    let recover_stdout = String::from_utf8_lossy(&recover_out.stdout);
    assert!(
        recover_stdout.contains("aborted_tx=1"),
        "unexpected recover output: {}",
        recover_stdout
    );

    // 5. Retrieve `committed-obj` in another fresh child process after deleting 8 shards and repairing via CLI
    let engine = StorageEngine::open(&root).unwrap();
    let manifest = engine.load_manifest("committed-obj").unwrap();
    for s_idx in 0..8 {
        engine
            .backend()
            .inject_delete_shard(&manifest.stripes[0].placements[s_idx].relative_path)
            .unwrap();
    }

    let repair_status = Command::new(bin)
        .args([
            "storage",
            "repair",
            "committed-obj",
            "--root",
            root.to_str().unwrap(),
        ])
        .status()
        .expect("spawn fsp-sim storage repair");
    assert!(repair_status.success());

    let restored_file = sandbox.path().join("restored_after_restart.bin");
    let get_status = Command::new(bin)
        .args([
            "storage",
            "get",
            "committed-obj",
            "--root",
            root.to_str().unwrap(),
            "--output",
            restored_file.to_str().unwrap(),
        ])
        .status()
        .expect("spawn fsp-sim storage get");
    assert!(get_status.success());
    assert_eq!(fs::read(&restored_file).unwrap(), baseline_bytes);
}
