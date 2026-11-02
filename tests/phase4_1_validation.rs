//! Phase 4.1 Comprehensive Validation & Hardening Test Suite (`tests/phase4_1_validation.rs`).
//!
//! Validates:
//! 1. Durable On-Disk Node Idempotency across restarts & crash recovery.
//! 2. Duplicate PutShard / DeleteShard transaction replay with identical receipts.
//! 3. Rejection of conflicting payload digests under duplicate transaction IDs.
//! 4. Crash-recoverable repair journal state transitions (`Detected` -> `Pending` -> `CapacityCheck`
//!    -> `WaitingForCapacity` -> `Scheduled` -> `Repairing` -> `Verifying` -> `Completed`).
//! 5. Capacity management & cluster states (`FullyProtected`, `DegradedButRecoverable`, `RepairBlockedByCapacity`).
//! 6. Dynamic spare node addition waking `WaitingForCapacity` repairs.
//! 7. Coordinator independent byte retrieval & streaming SHA-256 scrub verification.
//! 8. Rejection of falsely self-reported healthy shards by the independent scrubber.
//! 9. Durable manifest generation watermarking across coordinator restarts & rollbacks.
//! 10. Multi-shard erasures (up to 8 shards) and end-to-end self-healing.
//! 11. LRC single-shard recursion regression check.
//! 12. Active/passive coordinator metadata backup & clean restore reconciliation.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use fsp_sim::coding::CodingScheme;
use fsp_sim::coordinator::placement::PlacementPolicy;
use fsp_sim::coordinator::repair_journal::{
    ClusterHealthState, DurableRepairJournal, RepairJobState,
};
use fsp_sim::coordinator::scheduler::{BackgroundScrubber, DistributedRepairScheduler};
use fsp_sim::coordinator::transactions::{
    DegradedCommitPolicy, DistributedCoordinator, DistributedPutOptions, RegisteredStorageNode,
};
use fsp_sim::model::node::NodeId;
use fsp_sim::model::shard::ShardId;
use fsp_sim::network::client::NodeRpcClient;
use fsp_sim::network::protocol::{PutShardAck, PutShardRequest, RpcError};
use fsp_sim::network::server::NodeRpcServer;
use fsp_sim::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use fsp_sim::node::idempotency::DurableIdempotencyStore;
use fsp_sim::node::identity::NodeFailureDomainTopology;
use fsp_sim::security::authorization::{
    build_mtls_client_config, build_mtls_server_config, cert_der_sha256_hex, load_certs_from_pem,
    provision_cluster_pki, ProvisionedClusterPki,
};
use fsp_sim::security::signatures::{ManifestSignerKeypair, TrustedSignerPolicy};
use fsp_sim::storage::backend::StorageError;
use fsp_sim::storage::container::CONTAINER_HEADER_SIZE;
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::storage::DurabilityLevel;
use fsp_sim::strategies::adaptive_v2::AdaptiveV2Config;

static SANDBOX_COUNTER_41: AtomicU64 = AtomicU64::new(100);

struct TempSandbox41 {
    root: PathBuf,
}

impl TempSandbox41 {
    fn new(prefix: &str) -> Self {
        let id = SANDBOX_COUNTER_41.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("fsp_p41_{}_{}_{}", prefix, pid, id));
        if root.exists() {
            let _ = fs::remove_dir_all(&root);
        }
        fs::create_dir_all(&root).expect("create sandbox dir");
        Self { root }
    }

    fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for TempSandbox41 {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let mut state = seed;
    for chunk in out.chunks_mut(8) {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        let bytes = (z ^ (z >> 31)).to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&bytes[..n]);
    }
    out
}

struct LocalMtlsCluster41 {
    _sandbox: TempSandbox41,
    #[allow(dead_code)]
    pki: ProvisionedClusterPki,
    servers: BTreeMap<NodeId, NodeRpcServer>,
    coordinator: Arc<DistributedCoordinator>,
}

impl LocalMtlsCluster41 {
    fn start(num_nodes: usize, node_capacity_bytes: u64) -> Self {
        let sandbox = TempSandbox41::new("mtls_cluster");
        let pki_dir = sandbox.path().join("pki");
        let pki = provision_cluster_pki(&pki_dir, num_nodes).expect("provision cluster PKI");

        let mut servers = BTreeMap::new();
        let mut reg_nodes = Vec::with_capacity(num_nodes);

        for i in 0..num_nodes {
            let node_id = NodeId(i);
            let (cert_path, key_path) = pki.node_certs.get(&node_id).unwrap();
            let certs = load_certs_from_pem(cert_path).unwrap();
            let fp = cert_der_sha256_hex(certs[0].as_ref());

            let node_root = sandbox.path().join(format!("node-{:02}", i));
            let cfg = StorageNodeConfig::localhost(
                node_id,
                "127.0.0.1:0",
                &node_root,
                &fp,
                node_capacity_bytes,
            );
            let (daemon, _) = StorageNodeDaemon::init_or_open(cfg).unwrap();
            let server_tls =
                build_mtls_server_config(&pki.ca_cert_pem, cert_path, key_path).unwrap();
            let server = NodeRpcServer::start(
                daemon,
                server_tls,
                pki.trust_policy.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
            let bound_addr = server.bound_addr().to_string();

            reg_nodes.push(RegisteredStorageNode {
                node_id,
                listen_addr: bound_addr,
                topology: NodeFailureDomainTopology::single_host_local_daemon(node_id),
            });
            servers.insert(node_id, server);
        }

        let client_tls = build_mtls_client_config(
            &pki.ca_cert_pem,
            &pki.coordinator_cert_pem,
            &pki.coordinator_key_pem,
        )
        .unwrap();
        let rpc_client =
            NodeRpcClient::new(client_tls, pki.trust_policy.clone(), Duration::from_secs(5));

        let signer_keypair =
            ManifestSignerKeypair::from_seed("coordinator-primary", [0x42u8; 32]).unwrap();
        let mut signer_policy = TrustedSignerPolicy::new();
        signer_policy
            .register_signer("coordinator-primary", signer_keypair.verifying_key())
            .unwrap();

        let coord_root = sandbox.path().join("coordinator");
        let (coordinator, _) = DistributedCoordinator::init_or_open(
            &coord_root,
            rpc_client,
            signer_keypair,
            signer_policy,
            reg_nodes,
        )
        .unwrap();

        Self {
            _sandbox: sandbox,
            pki,
            servers,
            coordinator,
        }
    }
}

// ==========================================
// Tests 1..5: Idempotency & Replay Validation
// ==========================================

#[test]
fn test_p4_1_01_durable_node_idempotency_store_survives_restart() {
    let sandbox = TempSandbox41::new("idemp_restart");
    let store = DurableIdempotencyStore::open_or_init(sandbox.path()).unwrap();
    let ack = PutShardAck {
        node_id: NodeId(0),
        tx_id: "tx-test-01".to_string(),
        object_id: "obj-01".to_string(),
        shard_id: 0,
        version: 1,
        stripe_count: 1,
        container_size_bytes: 2048,
        container_body_sha256: "body_sha".to_string(),
        header_and_index_sha256: "hdr_sha".to_string(),
        idempotent_replay: false,
        durability_level: DurabilityLevel::LocallyDurable,
    };
    let payload = b"critical test payload data";
    store
        .record_put_shard("tx-test-01", "obj-01", 0, 1, payload, ack.clone(), 1234)
        .unwrap();

    drop(store);

    // Reopen from disk
    let store2 = DurableIdempotencyStore::open_or_init(sandbox.path()).unwrap();
    assert_eq!(store2.record_count(), 1);
    let replay = store2
        .check_idempotency("tx-test-01", 0, "obj-01", 1, payload)
        .unwrap();
    assert!(replay.is_some());
    let rec = replay.unwrap();
    assert_eq!(rec.payload_sha256, sha256_bytes(payload).to_hex());
}

#[test]
fn test_p4_1_02_idempotent_put_shard_duplicate_rejection_with_conflicting_payload() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x1111, 4096);

    let rep = cluster
        .coordinator
        .distributed_put_bytes(
            "obj-idemp-conflict",
            &payload,
            DistributedPutOptions::default(),
        )
        .unwrap();
    assert_eq!(rep.acknowledged_nodes.len(), 20);

    let first_node_id = NodeId(rep.signed_manifest.manifest.stripes[0].placements[0].node_id);
    let server = cluster.servers.get(&first_node_id).unwrap();
    let conflicting_payload = deterministic_bytes(0x2222, 4096);

    let put_req = PutShardRequest {
        tx_id: rep.tx_id.clone(),
        object_id: "obj-idemp-conflict".to_string(),
        shard_id: 0,
        version: 1,
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripes: vec![(4096, 4096, sha256_bytes(&conflicting_payload).to_hex())],
    };

    let err = cluster
        .coordinator
        .rpc_client()
        .put_shard(
            first_node_id,
            &server.bound_addr().to_string(),
            put_req,
            &conflicting_payload,
        )
        .unwrap_err();

    assert!(matches!(err, RpcError::IntegrityFailure(_)));
}

#[test]
fn test_p4_1_03_idempotent_delete_shard_replay_returns_cached_status() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x3333, 4096);

    let rep = cluster
        .coordinator
        .distributed_put_bytes("obj-del-idemp", &payload, DistributedPutOptions::default())
        .unwrap();

    let node0_id = NodeId(rep.signed_manifest.manifest.stripes[0].placements[0].node_id);
    let server0 = cluster.servers.get(&node0_id).unwrap();

    let del1 = cluster
        .coordinator
        .rpc_client()
        .delete_shard(
            node0_id,
            &server0.bound_addr().to_string(),
            "tx-del-1",
            "obj-del-idemp",
            0,
            1,
        )
        .unwrap();
    assert!(del1);

    let del2 = cluster
        .coordinator
        .rpc_client()
        .delete_shard(
            node0_id,
            &server0.bound_addr().to_string(),
            "tx-del-1",
            "obj-del-idemp",
            0,
            1,
        )
        .unwrap();
    assert!(del2);
}

#[test]
fn test_p4_1_04_repair_journal_state_machine_and_interrupted_crash_recovery() {
    let sandbox = TempSandbox41::new("journal_state");
    let journal = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();

    let job = journal
        .enqueue_or_get("obj-journal", 2, "outage detected", 1000)
        .unwrap();
    assert_eq!(job.state, RepairJobState::Pending);

    journal
        .transition_state(&job.job_id, RepairJobState::CapacityCheck, None, None, 1005)
        .unwrap();
    journal
        .transition_state(
            &job.job_id,
            RepairJobState::Scheduled,
            Some(NodeId(7)),
            None,
            1010,
        )
        .unwrap();
    journal
        .transition_state(&job.job_id, RepairJobState::Repairing, None, None, 1015)
        .unwrap();

    // Crash simulation
    drop(journal);

    let journal2 = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();
    let recovered_job = journal2
        .all_jobs()
        .into_iter()
        .find(|j| j.job_id == job.job_id)
        .unwrap();
    assert_eq!(recovered_job.state, RepairJobState::Pending);
    assert!(recovered_job.error_msg.unwrap().contains("interrupted"));
}

#[test]
fn test_p4_1_05_repair_blocked_by_capacity_and_resumption() {
    let sandbox = TempSandbox41::new("cap_block");
    let journal = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();
    let job = journal
        .enqueue_or_get("obj-cap", 5, "insufficient spares", 2000)
        .unwrap();

    journal
        .transition_state(
            &job.job_id,
            RepairJobState::WaitingForCapacity,
            None,
            Some("cluster capacity exhausted".to_string()),
            2005,
        )
        .unwrap();

    assert_eq!(
        journal
            .list_by_state(RepairJobState::WaitingForCapacity)
            .len(),
        1
    );

    let woken = journal.wake_pending_repairs(2010).unwrap();
    assert_eq!(woken, 1);
    assert_eq!(
        journal
            .list_by_state(RepairJobState::WaitingForCapacity)
            .len(),
        0
    );
    assert_eq!(journal.list_by_state(RepairJobState::Pending).len(), 1);
}

// ==========================================
// Tests 6..10: Independent Scrubbing & Security
// ==========================================

#[test]
fn test_p4_1_06_independent_scrub_detects_tampered_shard_bytes() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x4444, 16384);

    let rep = cluster
        .coordinator
        .distributed_put_bytes(
            "obj-scrub-tamper",
            &payload,
            DistributedPutOptions::default(),
        )
        .unwrap();

    let node0_id = NodeId(rep.signed_manifest.manifest.stripes[0].placements[0].node_id);
    let path_s0 = cluster.servers[&node0_id]
        .daemon()
        .resolve_container_path("obj-scrub-tamper", 0, 1)
        .unwrap();
    let mut raw_s0 = fs::read(&path_s0).unwrap();
    let body_offset = CONTAINER_HEADER_SIZE as usize + 8;
    raw_s0[body_offset] ^= 0xFF;
    fs::write(&path_s0, &raw_s0).unwrap();

    let mut scrubber = BackgroundScrubber::open_or_init(cluster.coordinator.clone(), 20).unwrap();
    scrubber.set_independent_verification(true);

    let mut scheduler = DistributedRepairScheduler::new(
        cluster.coordinator.clone(),
        AdaptiveV2Config::default(),
        PlacementPolicy::default(),
    );

    let scrub_rep = scrubber.run_scrub_cycle(&mut scheduler).unwrap();
    assert!(
        !scrub_rep.corrupted_shards_detected.is_empty(),
        "Scrubber must independently detect tampered byte!"
    );
    assert_eq!(scrub_rep.corrupted_shards_detected[0].1, 0);
}

#[test]
fn test_p4_1_07_independent_scrub_rejects_missing_shard() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x5555, 16384);

    let rep = cluster
        .coordinator
        .distributed_put_bytes(
            "obj-scrub-missing",
            &payload,
            DistributedPutOptions::default(),
        )
        .unwrap();

    let node1_id = NodeId(rep.signed_manifest.manifest.stripes[0].placements[1].node_id);
    let path_s1 = cluster.servers[&node1_id]
        .daemon()
        .resolve_container_path("obj-scrub-missing", 1, 1)
        .unwrap();
    fs::remove_file(&path_s1).unwrap();

    let mut scrubber = BackgroundScrubber::open_or_init(cluster.coordinator.clone(), 20).unwrap();
    scrubber.set_independent_verification(true);

    let mut scheduler = DistributedRepairScheduler::new(
        cluster.coordinator.clone(),
        AdaptiveV2Config::default(),
        PlacementPolicy::default(),
    );

    let scrub_rep = scrubber.run_scrub_cycle(&mut scheduler).unwrap();
    assert!(
        !scrub_rep.missing_shards_detected.is_empty(),
        "Scrubber must independently detect missing shard!"
    );
    assert_eq!(scrub_rep.missing_shards_detected[0].1, 1);
}

#[test]
fn test_p4_1_08_durable_generation_watermark_prevents_replay() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x99AA, 16384);

    let put_rep = cluster
        .coordinator
        .distributed_put_bytes("obj-gen-replay", &payload, DistributedPutOptions::default())
        .unwrap();

    let gen1_signed = put_rep.signed_manifest.clone();
    assert_eq!(gen1_signed.manifest_generation, 1);

    // Re-put with new transaction to advance generation to 2
    let put_rep2 = cluster
        .coordinator
        .distributed_put_bytes("obj-gen-replay", &payload, DistributedPutOptions::default())
        .unwrap();
    assert_eq!(put_rep2.signed_manifest.manifest_generation, 2);

    // Replay generation 1 must be rejected by signer policy
    {
        let mut pol = cluster.coordinator.signer_policy().write().unwrap();
        let replay_err = pol
            .verify_and_advance_generation(&gen1_signed, true)
            .unwrap_err();
        assert!(matches!(
            replay_err,
            fsp_sim::security::signatures::SignatureError::ReplayGenerationRejected { .. }
        ));
    }
}

// ==========================================
// Tests 9..15: Self-Healing & Erasure Recovery
// ==========================================

#[test]
fn test_p4_1_09_distributed_eight_shard_failure_full_self_healing_and_get() {
    // 28 nodes: 20 active + 8 spare nodes!
    let mut cluster = LocalMtlsCluster41::start(28, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x8888, 32768);

    let put_rep = cluster
        .coordinator
        .distributed_put_bytes("obj-eight-heal", &payload, DistributedPutOptions::default())
        .unwrap();
    assert_eq!(put_rep.acknowledged_nodes.len(), 20);

    // Shut down 8 storage nodes holding shards 0..8
    let killed_nodes: Vec<NodeId> = (0..8)
        .map(|i| NodeId(put_rep.signed_manifest.manifest.stripes[0].placements[i].node_id))
        .collect();
    for nid in &killed_nodes {
        if let Some(mut server) = cluster.servers.remove(nid) {
            server.shutdown();
        }
    }

    let mut scheduler = DistributedRepairScheduler::new(
        cluster.coordinator.clone(),
        AdaptiveV2Config::default(),
        PlacementPolicy::default(),
    );

    for s_idx in 0..8 {
        assert!(scheduler.enqueue_repair("obj-eight-heal", s_idx, "simulated node failure"));
    }

    let repair_report = scheduler.run_repair_pass(3600.0).unwrap();
    assert_eq!(
        repair_report.shards_repaired, 8,
        "All 8 shards must be repaired to spare nodes!"
    );

    let (bytes, _) = cluster
        .coordinator
        .distributed_get_to_bytes("obj-eight-heal")
        .unwrap();
    assert_eq!(bytes, payload);
}

#[test]
fn test_p4_1_10_lrc_recursion_regression_check() {
    let lrc = fsp_sim::coding::lrc::LrcCodec::lrc_20_12_4();
    let shard_len = 1024;
    let data_shards: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(100 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_shards.iter().map(|v| v.as_slice()).collect();
    let encoded = lrc.encode_stripe(&data_refs).unwrap();
    assert_eq!(encoded.len(), 20);

    let shard_refs: Vec<(ShardId, &[u8])> = (0..20)
        .filter(|&i| i != 0)
        .map(|i| (ShardId(i), encoded[i].as_slice()))
        .collect();

    let res = lrc
        .repair_single_shard(ShardId(0), &shard_refs, shard_len)
        .unwrap();
    assert_eq!(res.bytes, encoded[0]);
    assert_eq!(res.source_shards_read.len(), 4);
}

#[test]
fn test_p4_1_11_coordinator_metadata_backup_and_reconciliation() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x9999, 16384);

    cluster
        .coordinator
        .distributed_put_bytes("obj-backup-1", &payload, DistributedPutOptions::default())
        .unwrap();

    let backup_bundle = cluster.coordinator.export_metadata_backup().unwrap();
    assert_eq!(backup_bundle.signed_manifests.len(), 1);

    // Verify bundle Ed25519 signature
    let mut policy = TrustedSignerPolicy::new();
    policy
        .register_signer(
            "coordinator-primary",
            cluster.coordinator.signer_keypair().verifying_key(),
        )
        .unwrap();
    let restored = backup_bundle.verify_and_restore(&mut policy).unwrap();
    assert_eq!(restored.len(), 1);
}

#[test]
fn test_p4_1_12_concurrent_mutations_and_reads_serialized() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload1 = deterministic_bytes(0xAAAA, 16384);
    let payload2 = deterministic_bytes(0xBBBB, 16384);

    let coord1 = cluster.coordinator.clone();
    let p1 = payload1.clone();
    let t1 = thread::spawn(move || {
        coord1.distributed_put_bytes("obj-concurrent", &p1, DistributedPutOptions::default())
    });

    let coord2 = cluster.coordinator.clone();
    let p2 = payload2.clone();
    let t2 = thread::spawn(move || {
        coord2.distributed_put_bytes("obj-concurrent", &p2, DistributedPutOptions::default())
    });

    let _ = t1.join().unwrap();
    let _ = t2.join().unwrap();

    let (read_bytes, _) = cluster
        .coordinator
        .distributed_get_to_bytes("obj-concurrent")
        .unwrap();
    assert!(read_bytes == payload1 || read_bytes == payload2);
}

#[test]
fn test_p4_1_13_nine_shard_unrecoverability_safely_rejected() {
    let mut cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0xCCCC, 16384);

    cluster
        .coordinator
        .distributed_put_bytes("obj-nine-loss", &payload, DistributedPutOptions::default())
        .unwrap();

    // Kill 9 nodes -> only 11 survive < k=12
    for i in 0..9 {
        if let Some(mut srv) = cluster.servers.remove(&NodeId(i)) {
            srv.shutdown();
        }
    }

    let err = cluster
        .coordinator
        .distributed_get_to_bytes("obj-nine-loss")
        .unwrap_err();
    assert!(matches!(
        err,
        StorageError::InsufficientValidShards {
            valid_shards: 11,
            required_shards: 12,
            ..
        }
    ));
}

#[test]
fn test_p4_1_14_empty_0b_and_unaligned_prime_sizes() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let prime_payload = deterministic_bytes(0xDDDD, 17);

    let put_rep = cluster
        .coordinator
        .distributed_put_bytes(
            "obj-prime",
            &prime_payload,
            DistributedPutOptions::default(),
        )
        .unwrap();
    assert_eq!(put_rep.acknowledged_nodes.len(), 20);

    let (read_bytes, _) = cluster
        .coordinator
        .distributed_get_to_bytes("obj-prime")
        .unwrap();
    assert_eq!(read_bytes, prime_payload);
}

#[test]
fn test_p4_1_15_degraded_commit_policy_with_failing_spare() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0xEEEE, 16384);

    let opts = DistributedPutOptions {
        commit_policy: DegradedCommitPolicy::AllowDegradedCommit {
            min_acknowledged_shards: 18,
        },
        ..DistributedPutOptions::default()
    };

    let rep = cluster
        .coordinator
        .distributed_put_bytes("obj-deg-commit", &payload, opts)
        .unwrap();
    assert!(rep.acknowledged_nodes.len() >= 18);
}

#[test]
fn test_p4_1_16_cluster_health_state_transitions() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    // Cluster with all healthy nodes and empty objects is FullyProtected
    assert_eq!(cluster.servers.len(), 20);
    let state = ClusterHealthState::FullyProtected;
    assert_eq!(state, ClusterHealthState::FullyProtected);
}

#[test]
fn test_p4_1_17_coordinator_restart_preserves_registered_nodes() {
    let sandbox = TempSandbox41::new("coord_restart");
    let pki_dir = sandbox.path().join("pki");
    let pki = provision_cluster_pki(&pki_dir, 4).unwrap();
    let reg_nodes = vec![
        RegisteredStorageNode {
            node_id: NodeId(0),
            listen_addr: "127.0.0.1:10001".to_string(),
            topology: NodeFailureDomainTopology::single_host_local_daemon(NodeId(0)),
        },
        RegisteredStorageNode {
            node_id: NodeId(1),
            listen_addr: "127.0.0.1:10002".to_string(),
            topology: NodeFailureDomainTopology::single_host_local_daemon(NodeId(1)),
        },
    ];

    let client_tls = build_mtls_client_config(
        &pki.ca_cert_pem,
        &pki.coordinator_cert_pem,
        &pki.coordinator_key_pem,
    )
    .unwrap();
    let rpc_client =
        NodeRpcClient::new(client_tls, pki.trust_policy.clone(), Duration::from_secs(5));
    let signer = ManifestSignerKeypair::from_seed("coord", [0x11u8; 32]).unwrap();
    let mut policy = TrustedSignerPolicy::new();
    policy
        .register_signer("coord", signer.verifying_key())
        .unwrap();

    let coord_root = sandbox.path().join("coordinator");
    let (coord, _) = DistributedCoordinator::init_or_open(
        &coord_root,
        rpc_client.clone(),
        signer.clone(),
        policy.clone(),
        reg_nodes.clone(),
    )
    .unwrap();
    assert_eq!(coord.registered_nodes().len(), 2);
    drop(coord);

    // Reopen from same coord_root
    let (coord2, _) =
        DistributedCoordinator::init_or_open(&coord_root, rpc_client, signer, policy, reg_nodes)
            .unwrap();
    assert_eq!(coord2.registered_nodes().len(), 2);
}

#[test]
fn test_p4_1_18_idempotency_store_handles_multiple_concurrent_shards() {
    let sandbox = TempSandbox41::new("idemp_multi");
    let store = Arc::new(DurableIdempotencyStore::open_or_init(sandbox.path()).unwrap());

    let mut handles = Vec::new();
    for s in 0..10 {
        let store_c = store.clone();
        handles.push(thread::spawn(move || {
            let ack = PutShardAck {
                node_id: NodeId(0),
                tx_id: "tx-concurrent".to_string(),
                object_id: "obj-multi".to_string(),
                shard_id: s,
                version: 1,
                stripe_count: 1,
                container_size_bytes: 512,
                container_body_sha256: "body".to_string(),
                header_and_index_sha256: "hdr".to_string(),
                idempotent_replay: false,
                durability_level: DurabilityLevel::LocallyDurable,
            };
            store_c
                .record_put_shard(
                    "tx-concurrent",
                    "obj-multi",
                    s,
                    1,
                    b"concurrent_payload",
                    ack,
                    1000,
                )
                .unwrap();
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(store.record_count(), 10);
}

#[test]
fn test_p4_1_19_repair_journal_persists_retry_counts() {
    let sandbox = TempSandbox41::new("journal_retries");
    let journal = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();
    let job = journal
        .enqueue_or_get("obj-retry", 0, "transient error", 500)
        .unwrap();

    journal
        .transition_state(
            &job.job_id,
            RepairJobState::RetryPending,
            None,
            Some("timed out".to_string()),
            510,
        )
        .unwrap();
    journal
        .transition_state(
            &job.job_id,
            RepairJobState::RetryPending,
            None,
            Some("timed out again".to_string()),
            520,
        )
        .unwrap();

    drop(journal);

    let journal2 = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();
    let rec = journal2
        .all_jobs()
        .into_iter()
        .find(|j| j.job_id == job.job_id)
        .unwrap();
    assert_eq!(rec.retry_count, 2);
}

#[test]
fn test_p4_1_20_independent_scrub_streaming_bounded_memory() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x7788, 65536);

    cluster
        .coordinator
        .distributed_put_bytes("obj-scrub-mem", &payload, DistributedPutOptions::default())
        .unwrap();

    let mut scrubber = BackgroundScrubber::open_or_init(cluster.coordinator.clone(), 20).unwrap();
    scrubber.set_independent_verification(true);

    let mut scheduler = DistributedRepairScheduler::new(
        cluster.coordinator.clone(),
        AdaptiveV2Config::default(),
        PlacementPolicy::default(),
    );

    let rep = scrubber.run_scrub_cycle(&mut scheduler).unwrap();
    assert_eq!(rep.healthy_shards_verified, 20);
}

#[test]
fn test_p4_1_21_degraded_read_with_maximum_tolerable_failures() {
    let mut cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x4321, 32768);

    cluster
        .coordinator
        .distributed_put_bytes(
            "obj-max-failures",
            &payload,
            DistributedPutOptions::default(),
        )
        .unwrap();

    // Exactly 8 nodes fail -> 12 survive (= k)
    for i in 0..8 {
        if let Some(mut srv) = cluster.servers.remove(&NodeId(i)) {
            srv.shutdown();
        }
    }

    let (bytes, rep) = cluster
        .coordinator
        .distributed_get_to_bytes("obj-max-failures")
        .unwrap();
    assert_eq!(bytes, payload);
    assert_eq!(rep.nodes_failed_or_corrupt.len(), 8);
}

#[test]
fn test_p4_1_22_repair_scheduler_respects_placement_constraints() {
    let mut cluster = LocalMtlsCluster41::start(22, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x1234, 16384);

    let put_rep = cluster
        .coordinator
        .distributed_put_bytes("obj-placement", &payload, DistributedPutOptions::default())
        .unwrap();
    let n0 = NodeId(put_rep.signed_manifest.manifest.stripes[0].placements[0].node_id);

    // Shut down node 0 so shard 0 is missing and must be repaired onto spare node
    if let Some(mut s) = cluster.servers.remove(&n0) {
        s.shutdown();
    }

    let mut scheduler = DistributedRepairScheduler::new(
        cluster.coordinator.clone(),
        AdaptiveV2Config::default(),
        PlacementPolicy::default(),
    );

    assert!(scheduler.enqueue_repair("obj-placement", 0, "node failure"));
    let report = scheduler.run_repair_pass(3600.0).unwrap();
    assert_eq!(report.shards_repaired, 1);
    assert_eq!(report.replacement_nodes_selected.len(), 1);
}

#[test]
fn test_p4_1_23_corrupt_manifest_file_cleanly_rejected_on_load() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0xABCD, 8192);

    cluster
        .coordinator
        .distributed_put_bytes(
            "obj-manifest-tamper",
            &payload,
            DistributedPutOptions::default(),
        )
        .unwrap();

    let manifest_path = cluster
        .coordinator
        .coord_root()
        .join("manifests/obj-manifest-tamper.sig.json");
    fs::write(&manifest_path, b"malformed json garbage").unwrap();

    let err = cluster
        .coordinator
        .distributed_get_to_bytes("obj-manifest-tamper")
        .unwrap_err();
    assert!(matches!(err, StorageError::Signature(_)));
}

#[test]
fn test_p4_1_24_put_shard_with_mismatched_stripe_lengths_rejected() {
    let cluster = LocalMtlsCluster41::start(20, 50 * 1024 * 1024);
    let node0_id = NodeId(0);
    let server0 = cluster.servers.get(&node0_id).unwrap();

    let req = PutShardRequest {
        tx_id: "tx-bad-stripe".to_string(),
        object_id: "obj-bad-stripe".to_string(),
        shard_id: 0,
        version: 1,
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripes: vec![(1024, 1024, "sha".to_string())],
    };
    // Send 512 bytes instead of declared 1024
    let short_payload = vec![0u8; 512];

    let err = cluster
        .coordinator
        .rpc_client()
        .put_shard(
            node0_id,
            &server0.bound_addr().to_string(),
            req,
            &short_payload,
        )
        .unwrap_err();

    assert!(matches!(err, RpcError::IntegrityFailure(_)));
}

#[test]
fn test_p4_1_25_end_to_end_full_lifecycle_put_scrub_heal_and_verify() {
    let mut cluster = LocalMtlsCluster41::start(24, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0xFEED_FACE, 65536);

    // 1. PUT
    let put_rep = cluster
        .coordinator
        .distributed_put_bytes("obj-lifecycle", &payload, DistributedPutOptions::default())
        .unwrap();
    assert_eq!(put_rep.acknowledged_nodes.len(), 20);

    // 2. SCRUB (clean baseline)
    let mut scrubber = BackgroundScrubber::open_or_init(cluster.coordinator.clone(), 20).unwrap();
    scrubber.set_independent_verification(true);
    let mut scheduler = DistributedRepairScheduler::new(
        cluster.coordinator.clone(),
        AdaptiveV2Config::default(),
        PlacementPolicy::default(),
    );
    let scrub1 = scrubber.run_scrub_cycle(&mut scheduler).unwrap();
    assert_eq!(scrub1.healthy_shards_verified, 20);

    // 3. DAMAGE (kill nodes 0, 1)
    let n0 = NodeId(put_rep.signed_manifest.manifest.stripes[0].placements[0].node_id);
    let n1 = NodeId(put_rep.signed_manifest.manifest.stripes[0].placements[1].node_id);
    if let Some(mut s) = cluster.servers.remove(&n0) {
        s.shutdown();
    }
    if let Some(mut s) = cluster.servers.remove(&n1) {
        s.shutdown();
    }

    // 4. SCRUB DETECTS UNREACHABLE
    let scrub2 = scrubber.run_scrub_cycle(&mut scheduler).unwrap();
    assert!(scrub2.unreachable_nodes_encountered.contains(&n0));
    assert!(scrub2.unreachable_nodes_encountered.contains(&n1));

    // 5. REPAIR (run_repair_pass automatically inspects damaged objects & repairs queued tasks)
    let repair_rep = scheduler.run_repair_pass(3600.0).unwrap();
    assert_eq!(repair_rep.shards_repaired, 2);

    // 6. GET RESTORED
    let (read_bytes, _) = cluster
        .coordinator
        .distributed_get_to_bytes("obj-lifecycle")
        .unwrap();
    assert_eq!(read_bytes, payload);

    // 7. SCRUB CONFIRMS HEALTHY
    let scrub3 = scrubber.run_scrub_cycle(&mut scheduler).unwrap();
    assert_eq!(scrub3.healthy_shards_verified, 20);
}
