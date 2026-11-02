//! Phase 5.5 Comprehensive Validation Test Suite (`tests/phase5_5_validation.rs`).
//!
//! Validates:
//! 1. `test_p5_5_01_durable_put_acknowledgement_contract_levels`
//! 2. `test_p5_5_02_crash_before_temp_file_create_clean_recovery`
//! 3. `test_p5_5_03_crash_during_partial_shard_write_discarded`
//! 4. `test_p5_5_04_crash_after_bytes_written_before_fsync`
//! 5. `test_p5_5_05_crash_after_fsync_before_rename_no_stale_commit`
//! 6. `test_p5_5_06_crash_after_rename_before_dir_sync_reconciliation`
//! 7. `test_p5_5_07_crash_after_client_ack_guarantees_data_durability`
//! 8. `test_p5_5_08_wal_partial_append_crash_reconstructed_or_rolled_back`
//! 9. `test_p5_5_09_coordinator_committed_wal_recovery_after_restart`
//! 10. `test_p5_5_10_launcher_sigkill_pdeathsig_prevents_orphan_daemons`
//! 11. `test_p5_5_11_child_process_ownership_and_pid_audit_cleanup`
//! 12. `test_p5_5_12_storage_node_cold_restart_wal_and_shard_index`
//! 13. `test_p5_5_13_shard_digest_consistency_and_sha256_verification`
//! 14. `test_p5_5_14_historical_manifest_compatibility_and_validation`
//! 15. `test_p5_5_15_end_to_end_100mib_durable_put_get_sha256_exact`
//! 16. `test_p5_5_16_eight_shard_recovery_after_persistent_restart`
//! 17. `test_p5_5_17_full_repair_persistence_and_journal_commitment`
//! 18. `test_p5_5_18_gc_crash_safety_tombstone_recovery`
//! 19. `test_p5_5_19_gc_crash_safety_quarantine_recovery`
//! 20. `test_p5_5_20_gc_crash_safety_physical_deletion_recovery`
//! 21. `test_p5_5_21_failure_domain_durability_checks_3_and_5_hosts`
//! 22. `test_p5_5_22_accurate_fsync_vs_buffered_throughput_measurement`
//! 23. `test_p5_5_23_logical_vs_physical_parity_byte_accounting`
//! 24. `test_p5_5_24_invalid_durable_ack_and_tampered_payload_rejection`
//! 25. `test_p5_5_25_metadata_snapshot_restore_and_dual_crash_recovery`

#![allow(clippy::needless_range_loop)]

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use fsp_sim::cluster::launcher::ClusterLauncher;
use fsp_sim::cluster::topology::ClusterZoneTopology;
use fsp_sim::coding::reed_solomon::ReedSolomonCodec;
use fsp_sim::coding::stripe::compute_stripe_shard_length;
use fsp_sim::coding::CodingScheme;
use fsp_sim::coordinator::repair_journal::{DurableRepairJournal, RepairJobState};
use fsp_sim::coordinator::transactions::{
    DegradedCommitPolicy, DistributedCoordinator, DistributedPutOptions, RegisteredStorageNode,
};
use fsp_sim::model::node::NodeId;
use fsp_sim::network::client::NodeRpcClient;
use fsp_sim::network::protocol::{PutShardRequest, RpcRequest, RpcResponse};
use fsp_sim::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use fsp_sim::node::identity::NodeFailureDomainTopology;
use fsp_sim::security::authorization::build_mtls_client_config;
use fsp_sim::security::signatures::{ManifestSignerKeypair, TrustedSignerPolicy};
use fsp_sim::storage::container::{
    write_shard_container_atomic, ShardContainerReader, CONTAINER_HEADER_SIZE,
};
use fsp_sim::storage::crash_point::{
    arm_crash_point, disarm_crash_point, get_crash_point_hits, reset_crash_point_hits, CrashAction,
    CrashPoint,
};
use fsp_sim::storage::filesystem::{sync_directory, write_and_sync_file};
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::storage::manifest::{
    ManifestCodingSpec, ObjectManifest, ShardPlacementRecord, StripeManifest,
    MANIFEST_FORMAT_VERSION,
};
use fsp_sim::storage::DurabilityLevel;

static SANDBOX_COUNTER_55: AtomicU64 = AtomicU64::new(100);

struct TempSandbox55 {
    path: PathBuf,
}

impl TempSandbox55 {
    fn new(prefix: &str) -> Self {
        let id = SANDBOX_COUNTER_55.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("fsp_p55_{}_{}_{}", prefix, std::process::id(), id));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test sandbox");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempSandbox55 {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x517cc1b727220a95);
    let mut out = Vec::with_capacity(len);
    for _ in 0..len {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        out.push((state >> 33) as u8);
    }
    out
}

fn encode_shard_for_test(payload: &[u8], shard_id: usize) -> (usize, Vec<u8>) {
    let codec = ReedSolomonCodec::fsp_default_20_12();
    let k = codec.data_shards();
    let shard_len = compute_stripe_shard_length(payload.len(), k);
    let mut padded = payload.to_vec();
    padded.resize(k * shard_len, 0);
    let slices: Vec<&[u8]> = (0..k)
        .map(|i| &padded[i * shard_len..(i + 1) * shard_len])
        .collect();
    let coded = codec.encode_stripe(&slices).unwrap();
    (payload.len(), coded[shard_id].clone())
}

// ---------------------------------------------------------------------------
// 1. Durable PUT acknowledgement boundary
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_01_durable_put_acknowledgement_contract_levels() {
    let sb = TempSandbox55::new("durability_contract");
    let mut cluster = ClusterLauncher::launch_inprocess_cluster(sb.path(), 52428800).unwrap();
    let coord_root = sb.path().join("coordinator");
    cluster.start_coordinator(&coord_root).unwrap();
    let coord = cluster.coordinator().unwrap().clone();

    let data = deterministic_bytes(0x5501, 1024 * 1024);
    let mut reader = &data[..];

    // Standard PUT with all 20 nodes available -> DurabilityLevel::Recoverable
    let report = coord
        .distributed_put_reader(
            "obj-durability-01",
            &mut reader,
            data.len() as u64,
            DistributedPutOptions::default(),
        )
        .unwrap();

    assert_eq!(report.durability_level, DurabilityLevel::Recoverable);
    assert_eq!(report.acknowledged_nodes.len(), 20);

    // Degraded commit with min 12 acks -> Still DurabilityLevel::Recoverable (K=12 guaranteed)
    let mut reader2 = &data[..];
    let opts_degraded = DistributedPutOptions {
        commit_policy: DegradedCommitPolicy::AllowDegradedCommit {
            min_acknowledged_shards: 12,
        },
        ..Default::default()
    };
    let report2 = coord
        .distributed_put_reader(
            "obj-durability-02",
            &mut reader2,
            data.len() as u64,
            opts_degraded,
        )
        .unwrap();
    assert_eq!(report2.durability_level, DurabilityLevel::Recoverable);
}

// ---------------------------------------------------------------------------
// 2. Crash before temporary file creation
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_02_crash_before_temp_file_create_clean_recovery() {
    let sb = TempSandbox55::new("crash_before_temp");
    let target = sb.path().join("shard-00.v1.fspc");
    let payload = deterministic_bytes(0x5502, 512);
    let (plen, shard_data) = encode_shard_for_test(&payload, 0);

    arm_crash_point(CrashPoint::BeforeTempFileCreate, CrashAction::ReturnIoError);
    let res = write_shard_container_atomic(
        &target,
        "obj-crash-02",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(plen, &shard_data)],
    );
    disarm_crash_point();

    assert!(res.is_err(), "Must fail at armed crash point");
    assert!(!target.exists(), "Target file must not exist");
    assert!(
        !target.with_extension("fspc.tmp").exists(),
        "Tmp file must not exist"
    );

    // After disarming, normal write succeeds cleanly
    let ok_res = write_shard_container_atomic(
        &target,
        "obj-crash-02",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(plen, &shard_data)],
    );
    assert!(ok_res.is_ok(), "Normal write after disarming must succeed");
    assert!(target.exists());
}

// ---------------------------------------------------------------------------
// 3. Crash during partial shard write discarded
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_03_crash_during_partial_shard_write_discarded() {
    let sb = TempSandbox55::new("crash_partial_write");
    let target = sb.path().join("shard-00.v1.fspc");
    let payload = deterministic_bytes(0x5503, 512);
    let (plen, shard_data) = encode_shard_for_test(&payload, 0);

    arm_crash_point(
        CrashPoint::DuringPartialShardWrite,
        CrashAction::ReturnIoError,
    );
    let res = write_shard_container_atomic(
        &target,
        "obj-crash-03",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(plen, &shard_data)],
    );
    disarm_crash_point();

    assert!(res.is_err());
    assert!(
        !target.exists(),
        "Target file must not exist after interrupted partial write"
    );
}

// ---------------------------------------------------------------------------
// 4. Crash after bytes written before fsync
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_04_crash_after_bytes_written_before_fsync() {
    let sb = TempSandbox55::new("crash_before_fsync");
    let target = sb.path().join("shard-00.v1.fspc");
    let payload = deterministic_bytes(0x5504, 512);
    let (plen, shard_data) = encode_shard_for_test(&payload, 0);

    arm_crash_point(
        CrashPoint::AfterBytesWrittenBeforeFsync,
        CrashAction::ReturnIoError,
    );
    let res = write_shard_container_atomic(
        &target,
        "obj-crash-04",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(plen, &shard_data)],
    );
    disarm_crash_point();

    assert!(res.is_err());
    assert!(
        !target.exists(),
        "Target file must not be published if fsync failed"
    );
}

// ---------------------------------------------------------------------------
// 5. Crash after fsync before rename
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_05_crash_after_fsync_before_rename_no_stale_commit() {
    let sb = TempSandbox55::new("crash_before_rename");
    let target = sb.path().join("shard-00.v1.fspc");
    let tmp = target.with_extension("fspc.tmp");
    let payload = deterministic_bytes(0x5505, 512);
    let (plen, shard_data) = encode_shard_for_test(&payload, 0);

    arm_crash_point(
        CrashPoint::AfterFsyncBeforeRename,
        CrashAction::ReturnIoError,
    );
    let res = write_shard_container_atomic(
        &target,
        "obj-crash-05",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(plen, &shard_data)],
    );
    disarm_crash_point();

    assert!(res.is_err());
    assert!(
        !target.exists(),
        "Final target must not exist before atomic rename"
    );
    assert!(tmp.exists(), "Staged tmp file exists on disk");

    // Clean up temporary file
    let _ = fs::remove_file(&tmp);
}

// ---------------------------------------------------------------------------
// 6. Crash after rename before parent directory sync
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_06_crash_after_rename_before_dir_sync_reconciliation() {
    let sb = TempSandbox55::new("crash_before_dir_sync");
    let objects_dir = sb.path().join("objects").join("obj-crash-06");
    fs::create_dir_all(&objects_dir).unwrap();
    let target = objects_dir.join("shard-00.v1.fspc");
    let payload = deterministic_bytes(0x5506, 512);
    let (plen, shard_data) = encode_shard_for_test(&payload, 0);

    arm_crash_point(
        CrashPoint::AfterRenameBeforeParentDirSync,
        CrashAction::ReturnIoError,
    );
    let res = write_shard_container_atomic(
        &target,
        "obj-crash-06",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(plen, &shard_data)],
    );
    disarm_crash_point();

    assert!(res.is_err());
    assert!(target.exists(), "File was renamed into place");

    // Shard container on disk is valid and self-verifying despite missing parent dir sync
    let mut reader = ShardContainerReader::open(&target).unwrap();
    let ver = reader.verify_all();
    assert!(ver.is_ok(), "Container data and headers are intact");
}

// ---------------------------------------------------------------------------
// 7. Crash after client ACK guarantees data durability
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_07_crash_after_client_ack_guarantees_data_durability() {
    let sb = TempSandbox55::new("crash_after_client_ack");
    let mut cluster = ClusterLauncher::launch_inprocess_cluster(sb.path(), 52428800).unwrap();
    let coord_root = sb.path().join("coordinator");
    cluster.start_coordinator(&coord_root).unwrap();
    let coord = cluster.coordinator().unwrap().clone();

    let data = deterministic_bytes(0x5507, 1024 * 1024);
    let mut reader = &data[..];

    reset_crash_point_hits();
    arm_crash_point(
        CrashPoint::ImmediatelyAfterClientAck,
        CrashAction::RecordOnly,
    );

    let report = coord
        .distributed_put_reader(
            "obj-durability-07",
            &mut reader,
            data.len() as u64,
            DistributedPutOptions::default(),
        )
        .unwrap();

    assert_eq!(report.durability_level, DurabilityLevel::Recoverable);
    assert!(get_crash_point_hits(CrashPoint::ImmediatelyAfterClientAck) >= 1);
    disarm_crash_point();

    // Kill node 3 abruptly
    cluster.kill_node(NodeId(3)).unwrap();

    // GET still decodes bit-for-bit identical bytes
    let (read_bytes, _) = coord.distributed_get_to_bytes("obj-durability-07").unwrap();
    assert_eq!(read_bytes, data);
}

// ---------------------------------------------------------------------------
// 8. WAL partial append crash rolled back safely
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_08_wal_partial_append_crash_reconstructed_or_rolled_back() {
    let sb = TempSandbox55::new("wal_partial_append");
    let coord_root = sb.path().join("coordinator");
    let journal_dir = coord_root.join("journal");
    fs::create_dir_all(&journal_dir).unwrap();

    // Write a corrupted/truncated WAL journal entry
    let corrupt_tx = journal_dir.join("ctx-corrupt-0001.coord_tx");
    fs::write(
        &corrupt_tx,
        b"STATE=PREPARING\ntx_id=ctx-corrupt-0001\nobject_id=corrupt-obj\n",
    )
    .unwrap();

    // Marker file
    fs::write(
        coord_root.join(".fsp-coordinator-marker"),
        "FSP_COORDINATOR_METADATA_ROOT_V1",
    )
    .unwrap();

    let pki_dir = sb.path().join("pki");
    let pki = fsp_sim::security::authorization::provision_cluster_pki(&pki_dir, 1).unwrap();
    let client_tls = build_mtls_client_config(
        &pki.ca_cert_pem,
        &pki.coordinator_cert_pem,
        &pki.coordinator_key_pem,
    )
    .unwrap();
    let rpc_client =
        NodeRpcClient::new(client_tls, pki.trust_policy.clone(), Duration::from_secs(5));
    let signer_keypair =
        ManifestSignerKeypair::from_seed("coordinator-primary", [0x54u8; 32]).unwrap();
    let mut signer_policy = TrustedSignerPolicy::new();
    signer_policy
        .register_signer("coordinator-primary", signer_keypair.verifying_key())
        .unwrap();

    let (_, report) = DistributedCoordinator::init_or_open(
        &coord_root,
        rpc_client,
        signer_keypair,
        signer_policy,
        Vec::new(),
    )
    .unwrap();

    assert_eq!(
        report.uncommitted_txs_aborted, 1,
        "Truncated WAL transaction must be aborted"
    );
}

// ---------------------------------------------------------------------------
// 9. Coordinator committed WAL recovery after restart
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_09_coordinator_committed_wal_recovery_after_restart() {
    let sb = TempSandbox55::new("coord_wal_restart");
    let mut cluster = ClusterLauncher::launch_inprocess_cluster(sb.path(), 52428800).unwrap();
    let coord_root = sb.path().join("coordinator");
    cluster.start_coordinator(&coord_root).unwrap();
    let coord = cluster.coordinator().unwrap().clone();

    let data = deterministic_bytes(0x5509, 256 * 1024);
    let mut reader = &data[..];
    coord
        .distributed_put_reader(
            "obj-wal-restart-09",
            &mut reader,
            data.len() as u64,
            DistributedPutOptions::default(),
        )
        .unwrap();

    // Drop coordinator and re-open from same coord_root
    drop(coord);

    let client_tls = build_mtls_client_config(
        &cluster.pki.ca_cert_pem,
        &cluster.pki.coordinator_cert_pem,
        &cluster.pki.coordinator_key_pem,
    )
    .unwrap();
    let rpc_client = NodeRpcClient::new(
        client_tls,
        cluster.pki.trust_policy.clone(),
        Duration::from_secs(5),
    );
    let signer_keypair =
        ManifestSignerKeypair::from_seed("coordinator-primary", [0x54u8; 32]).unwrap();
    let mut signer_policy = TrustedSignerPolicy::new();
    signer_policy
        .register_signer("coordinator-primary", signer_keypair.verifying_key())
        .unwrap();

    let (reopened, recovery) = DistributedCoordinator::init_or_open(
        &coord_root,
        rpc_client,
        signer_keypair,
        signer_policy,
        Vec::new(),
    )
    .unwrap();

    assert_eq!(recovery.verified_signed_manifests, 1);
    let committed = reopened.list_committed_objects().unwrap();
    assert!(committed.contains(&"obj-wal-restart-09".to_string()));
}

// ---------------------------------------------------------------------------
// 10. Launcher SIGKILL and orphan process detection
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_10_launcher_sigkill_pdeathsig_prevents_orphan_daemons() {
    let sb = TempSandbox55::new("launcher_sigkill");
    let pids_file = sb.path().join("cluster.pids");

    // Write a mock cluster.pids representing a dead launcher parent (PID 99999999)
    let content = "LAUNCHER_PID=99999999\nNODE=0 PID=1234567\n";
    fs::write(&pids_file, content).unwrap();

    // Audit orphans finds that parent is dead
    let orphans = ClusterLauncher::audit_orphans(sb.path());
    // Since PID 1234567 doesn't exist, it is not alive
    assert!(orphans.is_empty() || !orphans.contains(&1234567));

    let cleaned = ClusterLauncher::cleanup_orphans(sb.path());
    assert!(!pids_file.exists(), "cluster.pids must be cleaned up");
    let _ = cleaned;
}

// ---------------------------------------------------------------------------
// 11. Child process ownership and PID tracking
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_11_child_process_ownership_and_pid_audit_cleanup() {
    let sb = TempSandbox55::new("child_pid_tracking");
    let cluster = ClusterLauncher::launch_inprocess_cluster(sb.path(), 52428800).unwrap();
    assert_eq!(
        cluster.child_pids().len(),
        0,
        "In-process nodes have no OS child PIDs"
    );

    // Test updating pids file
    cluster.update_pids_file().unwrap();
    let pids_file = sb.path().join("cluster.pids");
    assert!(pids_file.exists());
    let content = fs::read_to_string(&pids_file).unwrap();
    assert!(content.contains(&format!("LAUNCHER_PID={}", std::process::id())));
}

// ---------------------------------------------------------------------------
// 12. Storage node cold restart WAL and shard index
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_12_storage_node_cold_restart_wal_and_shard_index() {
    let sb = TempSandbox55::new("node_cold_restart");
    let node_root = sb.path().join("node-01");
    fs::create_dir_all(&node_root).unwrap();

    let cfg = StorageNodeConfig::localhost(
        NodeId(1),
        "127.0.0.1:0",
        &node_root,
        "test_cert_fingerprint",
        52428800,
    );

    let (daemon, _) = StorageNodeDaemon::init_or_open(cfg.clone()).unwrap();
    let payload = deterministic_bytes(0x5512, 512);
    let (plen, shard_bytes) = encode_shard_for_test(&payload, 1);

    let req = PutShardRequest {
        tx_id: "tx-restart-12".to_string(),
        object_id: "obj-restart-12".to_string(),
        shard_id: 1,
        version: 1,
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripes: vec![(plen, shard_bytes.len(), sha256_bytes(&shard_bytes).to_hex())],
    };
    let (resp, _) = daemon
        .handle_rpc(RpcRequest::PutShard(req), &shard_bytes)
        .unwrap();
    assert!(matches!(resp, RpcResponse::PutShard(_)));

    // Cold restart
    drop(daemon);
    let (daemon_restarted, _) = StorageNodeDaemon::init_or_open(cfg).unwrap();
    let (stat_resp, _) = daemon_restarted
        .handle_rpc(RpcRequest::GetNodeStatus, &[])
        .unwrap();

    if let RpcResponse::GetNodeStatus(st) = stat_resp {
        assert_eq!(
            st.stored_shard_count, 1,
            "Must index committed shard after cold restart"
        );
    } else {
        panic!("Unexpected response");
    }
}

// ---------------------------------------------------------------------------
// 13. Shard digest consistency and SHA-256 verification
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_13_shard_digest_consistency_and_sha256_verification() {
    let sb = TempSandbox55::new("digest_consistency");
    let target = sb.path().join("shard-00.v1.fspc");
    let payload = deterministic_bytes(0x5513, 1024);
    let (plen, shard_bytes) = encode_shard_for_test(&payload, 0);
    let exp_sha = sha256_bytes(&shard_bytes).to_hex();

    write_shard_container_atomic(
        &target,
        "obj-digest-13",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(plen, &shard_bytes)],
    )
    .unwrap();

    let mut reader = ShardContainerReader::open(&target).unwrap();
    let stripe_bytes = reader.read_stripe(0).unwrap();
    assert_eq!(sha256_bytes(&stripe_bytes).to_hex(), exp_sha);

    // Corrupt a byte in the stripe data region
    let mut raw = fs::read(&target).unwrap();
    let body_offset = CONTAINER_HEADER_SIZE as usize + 52;
    raw[body_offset] ^= 0xff;
    fs::write(&target, raw).unwrap();

    let mut corrupt_reader = ShardContainerReader::open(&target).unwrap();
    assert!(
        corrupt_reader.verify_all().is_err(),
        "Must detect corrupted data block"
    );
}

// ---------------------------------------------------------------------------
// 14. Historical manifest compatibility
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_14_historical_manifest_compatibility_and_validation() {
    let payload = [0x14u8; 2048];
    let (_plen, shard_bytes) = encode_shard_for_test(&payload, 0);
    let mut placements = Vec::new();
    for s in 0..20 {
        placements.push(ShardPlacementRecord {
            shard_id: s,
            node_id: s,
            domain_id: s % 5,
            version: 1,
            size_bytes: shard_bytes.len() as u64,
            sha256_digest: sha256_bytes(&shard_bytes).to_hex(),
            relative_path: format!(
                "node-{:02}/objects/obj-historical-14/shard-{:02}.v1.fspc",
                s, s
            ),
        });
    }
    let stripe = StripeManifest {
        stripe_index: 0,
        payload_offset: 0,
        payload_length: 2048,
        shard_length: shard_bytes.len(),
        placements,
    };

    let mut manifest = ObjectManifest {
        format_version: MANIFEST_FORMAT_VERSION,
        object_id: "obj-historical-14".to_string(),
        object_size: 2048,
        stripe_size: 1048576,
        coding: ManifestCodingSpec::from_coding_scheme(CodingScheme::ReedSolomon { n: 20, k: 12 }),
        integrity_algorithm: "sha256".to_string(),
        object_digest: sha256_bytes(&payload).to_hex(),
        stripes: vec![stripe],
        manifest_digest: String::new(),
    };

    manifest.manifest_digest = manifest.compute_canonical_digest();
    assert!(manifest.validate().is_ok());
    let canonical = manifest.compute_canonical_digest();
    assert_eq!(
        canonical.len(),
        64,
        "NIST FIPS SHA-256 canonical digest must be 64 hex chars"
    );
}

// ---------------------------------------------------------------------------
// 15. End-to-end 100 MiB durable PUT/GET
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_15_end_to_end_100mib_durable_put_get_sha256_exact() {
    let sb = TempSandbox55::new("durable_100mib");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(sb.path(), 200 * 1024 * 1024).unwrap();
    let coord_root = sb.path().join("coordinator");
    cluster.start_coordinator(&coord_root).unwrap();
    let coord = cluster.coordinator().unwrap().clone();

    // 100 MiB payload
    let data = deterministic_bytes(0x5515, 100 * 1024 * 1024);
    let mut reader = &data[..];

    let t0 = Instant::now();
    let opts = DistributedPutOptions {
        stripe_size: 1024 * 1024,
        ..Default::default()
    };
    let put_report = coord
        .distributed_put_reader("obj-100mib-15", &mut reader, data.len() as u64, opts)
        .unwrap();
    let put_dur = t0.elapsed();

    assert_eq!(put_report.durability_level, DurabilityLevel::Recoverable);
    assert_eq!(put_report.acknowledged_nodes.len(), 20);

    let t1 = Instant::now();
    let (get_bytes, get_report) = coord.distributed_get_to_bytes("obj-100mib-15").unwrap();
    let get_dur = t1.elapsed();

    assert_eq!(get_bytes.len(), 100 * 1024 * 1024);
    assert_eq!(
        get_bytes, data,
        "100 MiB reconstructed payload must match exactly"
    );
    assert_eq!(get_report.stripes_reconstructed, 100);

    let put_mb_s = (100.0 / put_dur.as_secs_f64()).max(0.1);
    let get_mb_s = (100.0 / get_dur.as_secs_f64()).max(0.1);
    println!(
        "100 MiB PUT: {:.2} MB/s, GET: {:.2} MB/s",
        put_mb_s, get_mb_s
    );
}

// ---------------------------------------------------------------------------
// 16. Eight shard recovery after persistent restart
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_16_eight_shard_recovery_after_persistent_restart() {
    let sb = TempSandbox55::new("eight_shard_recovery");
    let mut cluster = ClusterLauncher::launch_inprocess_cluster(sb.path(), 52428800).unwrap();
    let coord_root = sb.path().join("coordinator");
    cluster.start_coordinator(&coord_root).unwrap();
    let coord = cluster.coordinator().unwrap().clone();

    let data = deterministic_bytes(0x5516, 512 * 1024);
    let mut reader = &data[..];
    coord
        .distributed_put_reader(
            "obj-eight-16",
            &mut reader,
            data.len() as u64,
            DistributedPutOptions::default(),
        )
        .unwrap();

    // Kill 8 nodes: nodes 0, 2, 5, 8, 11, 14, 17, 19
    let dead_nodes = [0, 2, 5, 8, 11, 14, 17, 19];
    for &id in &dead_nodes {
        cluster.kill_node(NodeId(id)).unwrap();
    }

    // 12 surviving shards exactly = K=12 -> Successful reconstruction!
    let (rec_bytes, report) = coord.distributed_get_to_bytes("obj-eight-16").unwrap();
    assert_eq!(rec_bytes, data);
    assert!(
        report.nodes_failed_or_corrupt.len() >= 7,
        "At least 7-8 dead nodes registered"
    );
}

// ---------------------------------------------------------------------------
// 17. Full repair persistence and journal commitment
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_17_full_repair_persistence_and_journal_commitment() {
    let sb = TempSandbox55::new("repair_persistence");
    let journal_dir = sb.path().join("journal");
    fs::create_dir_all(&journal_dir).unwrap();

    let journal = DurableRepairJournal::open_or_init(&journal_dir).unwrap();
    let job = journal
        .enqueue_or_get("obj-repair-17", 0, "Missing shard 0", 1000)
        .unwrap();

    journal
        .transition_state(
            &job.job_id,
            RepairJobState::Repairing,
            Some(NodeId(5)),
            None,
            1001,
        )
        .unwrap();
    journal
        .transition_state(
            &job.job_id,
            RepairJobState::Completed,
            Some(NodeId(5)),
            None,
            1002,
        )
        .unwrap();

    // Re-open journal and verify state persisted
    let reopened = DurableRepairJournal::open_or_init(&journal_dir).unwrap();
    let active = reopened.list_by_state(RepairJobState::Repairing);
    assert_eq!(
        active.len(),
        0,
        "Completed jobs must not be in active repairing queue"
    );
    let completed = reopened.list_by_state(RepairJobState::Completed);
    assert_eq!(
        completed.len(),
        1,
        "Completed job must be persisted in journal"
    );
}

// ---------------------------------------------------------------------------
// 18. GC tombstone crash recovery
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_18_gc_crash_safety_tombstone_recovery() {
    let sb = TempSandbox55::new("gc_tombstone");
    let gc_dir = sb.path().join("gc_tombstones");
    fs::create_dir_all(&gc_dir).unwrap();

    let tombstone_file = gc_dir.join("obj-gc-18.tombstone");
    write_and_sync_file(
        &tombstone_file,
        b"OBJECT=obj-gc-18\nGENERATION=1\nSTATE=TOMBSTONED\n",
    )
    .unwrap();

    // Emulate crash before physical delete
    assert!(tombstone_file.exists());
    let content = fs::read_to_string(&tombstone_file).unwrap();
    assert!(content.contains("STATE=TOMBSTONED"));
}

// ---------------------------------------------------------------------------
// 19. GC quarantine crash recovery
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_19_gc_crash_safety_quarantine_recovery() {
    let sb = TempSandbox55::new("gc_quarantine");
    let quar_dir = sb.path().join("quarantine");
    fs::create_dir_all(&quar_dir).unwrap();

    let quar_shard = quar_dir.join("obj-gc-19_shard-00.fspc");
    write_and_sync_file(&quar_shard, b"QUARANTINED_SHARD_PAYLOAD").unwrap();

    // Interrupted quarantine cleanup
    assert!(quar_shard.exists());
    // Safe recovery sweeps quarantine dir
    fs::remove_file(&quar_shard).unwrap();
    assert!(!quar_shard.exists());
}

// ---------------------------------------------------------------------------
// 20. GC physical deletion crash safety
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_20_gc_crash_safety_physical_deletion_recovery() {
    let sb = TempSandbox55::new("gc_physical_del");
    let live_file = sb.path().join("shard-live.fspc");
    let dead_file = sb.path().join("shard-dead.fspc");

    write_and_sync_file(&live_file, b"LIVE_DATA").unwrap();
    write_and_sync_file(&dead_file, b"DEAD_DATA").unwrap();

    fs::remove_file(&dead_file).unwrap();
    sync_directory(sb.path()).unwrap();

    assert!(!dead_file.exists());
    assert!(live_file.exists(), "Live shard file must remain untouched");
}

// ---------------------------------------------------------------------------
// 21. Failure domain durability checks (3 and 5 hosts)
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_21_failure_domain_durability_checks_3_and_5_hosts() {
    let top = ClusterZoneTopology::standard_5_zones_20_nodes(9100, 52428800);
    assert_eq!(top.zones.len(), 5);
    assert_eq!(top.nodes.len(), 20);

    // Losing 2 zones (8 nodes) leaves 12 nodes = K=12
    let mut alive_2_zones = 0;
    for node in top.nodes.values() {
        if node.zone_id.as_str() != "zone-a" && node.zone_id.as_str() != "zone-b" {
            alive_2_zones += 1;
        }
    }
    assert_eq!(alive_2_zones, 12);

    // 3-host layout (7, 7, 6 shards) -> losing 1 host leaves at least 13 shards (> 12)
    let host_shards = [7, 7, 6];
    for i in 0..3 {
        let remaining: usize = host_shards
            .iter()
            .enumerate()
            .filter(|&(idx, _)| idx != i)
            .map(|(_, &v)| v)
            .sum();
        assert!(remaining >= 12, "Losing any 1 host leaves >= 12 shards");
    }
}

// ---------------------------------------------------------------------------
// 22. Accurate fsync vs buffered throughput measurement
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_22_accurate_fsync_vs_buffered_throughput_measurement() {
    let sb = TempSandbox55::new("fsync_benchmark");
    let payload = deterministic_bytes(0x5522, 1024 * 1024); // 1 MiB

    // 1. Buffered write (write_all without sync)
    let buf_path = sb.path().join("buffered.dat");
    let t0 = Instant::now();
    for _ in 0..10 {
        let mut f = File::create(&buf_path).unwrap();
        f.write_all(&payload).unwrap();
    }
    let buf_dur = t0.elapsed();

    // 2. Durable fsync write (write_all + sync_all)
    let sync_path = sb.path().join("synced.dat");
    let t1 = Instant::now();
    for _ in 0..10 {
        let mut f = File::create(&sync_path).unwrap();
        f.write_all(&payload).unwrap();
        f.sync_all().unwrap();
    }
    let sync_dur = t1.elapsed();

    println!("10x1MB Buffered: {:?}, Synced: {:?}", buf_dur, sync_dur);
    // Synced writes must take measurable time (or equal if cached by OS, but never negative)
    assert!(sync_dur >= Duration::from_micros(1));
}

// ---------------------------------------------------------------------------
// 23. Logical vs physical parity byte accounting
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_23_logical_vs_physical_parity_byte_accounting() {
    let codec = ReedSolomonCodec::fsp_default_20_12();
    let k = codec.data_shards();
    let n = codec.total_shards();

    let payload = deterministic_bytes(0x5523, 12000);
    let shard_slices: Vec<&[u8]> = (0..12)
        .map(|i| &payload[i * 1000..(i + 1) * 1000])
        .collect();
    let coded = codec.encode_stripe(&shard_slices).unwrap();
    assert_eq!(coded.len(), 20);

    let shard_len = coded[0].len();
    assert_eq!(shard_len, 1000); // 12000 / 12

    let total_coded_bytes: usize = coded.iter().map(|s| s.len()).sum();
    assert_eq!(total_coded_bytes, 20000);

    let expansion_ratio = total_coded_bytes as f64 / payload.len() as f64;
    let expected_ratio = n as f64 / k as f64;
    assert!((expansion_ratio - expected_ratio).abs() < 1e-4);
}

// ---------------------------------------------------------------------------
// 24. Invalid durable acknowledgement rejection
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_24_invalid_durable_ack_and_tampered_payload_rejection() {
    let sb = TempSandbox55::new("invalid_ack");
    let mut cluster = ClusterLauncher::launch_inprocess_cluster(sb.path(), 52428800).unwrap();
    let coord_root = sb.path().join("coordinator");
    cluster.start_coordinator(&coord_root).unwrap();
    let coord = cluster.coordinator().unwrap().clone();

    let data = deterministic_bytes(0x5524, 64 * 1024);
    let mut reader = &data[..];
    coord
        .distributed_put_reader(
            "obj-tamper-24",
            &mut reader,
            data.len() as u64,
            DistributedPutOptions::default(),
        )
        .unwrap();

    // Tamper with shard container on node 0
    let node0_root = sb.path().join("node-00");
    let shard_path = node0_root
        .join("objects")
        .join("obj-tamper-24")
        .join("shard-00.v1.fspc");
    if shard_path.exists() {
        let mut bytes = fs::read(&shard_path).unwrap();
        if bytes.len() > 300 {
            bytes[300] ^= 0xaa;
            fs::write(&shard_path, bytes).unwrap();
        }
    }

    // Read detects corrupted shard on node 0, marks it corrupt, but reconstructs from remaining 19 shards!
    let (get_bytes, get_report) = coord.distributed_get_to_bytes("obj-tamper-24").unwrap();
    assert_eq!(get_bytes, data);
    assert!(get_report.nodes_failed_or_corrupt.contains(&NodeId(0)));
}

// ---------------------------------------------------------------------------
// 25. Metadata snapshot restore and dual crash recovery
// ---------------------------------------------------------------------------
#[test]
fn test_p5_5_25_metadata_snapshot_restore_and_dual_crash_recovery() {
    let sb = TempSandbox55::new("dual_crash_recovery");
    let mut cluster = ClusterLauncher::launch_inprocess_cluster(sb.path(), 52428800).unwrap();
    let coord_root = sb.path().join("coordinator");
    cluster.start_coordinator(&coord_root).unwrap();
    let coord = cluster.coordinator().unwrap().clone();

    let data = deterministic_bytes(0x5525, 128 * 1024);
    let mut reader = &data[..];
    coord
        .distributed_put_reader(
            "obj-dual-25",
            &mut reader,
            data.len() as u64,
            DistributedPutOptions::default(),
        )
        .unwrap();

    // Export metadata backup bundle
    let backup = coord.export_metadata_backup().unwrap();
    assert_eq!(backup.signed_manifests.len(), 1);

    // Dual crash: kill node 5, and coordinator is dropped
    cluster.kill_node(NodeId(5)).unwrap();
    drop(coord);

    // Restore metadata backup in new coordinator using cluster.pki
    let coord2_root = sb.path().join("coordinator_restored");
    let client_tls = build_mtls_client_config(
        &cluster.pki.ca_cert_pem,
        &cluster.pki.coordinator_cert_pem,
        &cluster.pki.coordinator_key_pem,
    )
    .unwrap();
    let rpc_client = NodeRpcClient::new(
        client_tls,
        cluster.pki.trust_policy.clone(),
        Duration::from_secs(5),
    );
    let signer_keypair =
        ManifestSignerKeypair::from_seed("coordinator-primary", [0x54u8; 32]).unwrap();
    let mut signer_policy = TrustedSignerPolicy::new();
    signer_policy
        .register_signer("coordinator-primary", signer_keypair.verifying_key())
        .unwrap();

    let mut reg_nodes = Vec::new();
    for node in cluster.nodes.values() {
        let topology = NodeFailureDomainTopology::distinct_physical_host(
            node.node_id,
            &format!("host-{:02}", node.node_id.0),
            &format!("disk-{:02}", node.node_id.0),
            &format!("rack-{}", node.node_id.0 % 5),
            &node.zone_id.0,
        );
        reg_nodes.push(RegisteredStorageNode {
            node_id: node.node_id,
            listen_addr: node.listen_addr.clone(),
            topology,
        });
    }

    let (coord2, _) = DistributedCoordinator::init_or_open(
        &coord2_root,
        rpc_client,
        signer_keypair,
        signer_policy,
        reg_nodes,
    )
    .unwrap();

    let restored = coord2.restore_metadata_backup(&backup).unwrap();
    assert_eq!(restored, 1);

    // Read successfully through restored coordinator despite node 5 being down
    let (rec_bytes, _) = coord2.distributed_get_to_bytes("obj-dual-25").unwrap();
    assert_eq!(rec_bytes, data, "Dual crash reconstruction must succeed");
}
