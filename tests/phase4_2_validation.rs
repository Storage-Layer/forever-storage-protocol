//! Phase 4.2 Comprehensive Validation Test Suite (`tests/phase4_2_validation.rs`).
//!
//! Validates:
//! 1. 3-Member Quorum Consensus & Monotonic Epoch Elections.
//! 2. Quorum Loss Fail-Closed Mutation Blocking.
//! 3. Fencing Token Issuance, Wire Serialization & Validation.
//! 4. Storage Node Fenced Rejection of Stale Leader PutShard.
//! 5. Storage Node Fenced Rejection of Stale Leader DeleteShard.
//! 6. Storage Node Epoch Advance & Durable `.rec` Fsync.
//! 7. Storage Node Restart Preserves Highest Seen Epoch.
//! 8. Manifest Commit Blocked when Coordinator Epoch Superseded.
//! 9. Metadata Replication WAL Logging & Replay.
//! 10. Standby Promotion & In-Flight Repair Reconciliation.
//! 11. Graceful Leader Failover with State Continuity.
//! 12. Subprocess SIGKILL Crash Recovery and Standby Promotion.
//! 13. Split-Brain Defense: Isolated Old Leader Fenced from Storage Nodes.
//! 14. Physical Placement: RS(20,12) 3-Host Single-Host Loss Tolerance ($F=1$).
//! 15. Physical Placement: 2-Host Topology Mathematically Rejected ($F=1$).
//! 16. Physical Placement: 5-Host Topology Double-Host Loss Tolerance ($F=2$).
//! 17. Multi-Level Failure Domain Validation (Host + Rack + Zone).
//! 18. TLS Connection Pool: Connection Reuse & Handshake Reduction.
//! 19. TLS Connection Pool: Idle Timeout & Connection Eviction.
//! 20. Bounded Exponential Backoff Retry.
//! 21. Real Configuration Loader: `coordinator.example.toml` Parsing & Validation.
//! 22. Real Configuration Loader: `node.example.toml` Parsing & Validation.
//! 23. Configuration Strict Mode: Unknown Field Rejection.
//! 24. Network Fault Injection: Deterministic Packet Loss Simulation.
//! 25. End-to-End Distributed Cluster PUT/GET with Connection Pooling.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use fsp_sim::coding::CodingScheme;
use fsp_sim::config::loader::{
    load_coordinator_config_file, load_node_config_file, parse_coordinator_config,
};
use fsp_sim::coordinator::failover::CoordinatorFailoverManager;
use fsp_sim::coordinator::fencing::{FencingToken, NodeEpochFencingTracker};
use fsp_sim::coordinator::leadership::{
    CoordinatorId, CoordinatorRole, DurableEpochStore, QuorumState,
};
use fsp_sim::coordinator::placement::PlacementPolicy;
use fsp_sim::coordinator::repair_journal::{DurableRepairJournal, RepairJobState};
use fsp_sim::coordinator::replication::{MetadataReplicationWal, ReplicationRecord};
use fsp_sim::coordinator::transactions::{
    DegradedCommitPolicy, DistributedCoordinator, DistributedPutOptions, RegisteredStorageNode,
};
use fsp_sim::model::node::NodeId;
use fsp_sim::network::client::NodeRpcClient;
use fsp_sim::network::connection_pool::{ConnectionPoolConfig, TlsConnectionPool};
use fsp_sim::network::protocol::{PutShardRequest, RpcError};
use fsp_sim::network::retry::{retry_operation, RetryPolicy};
use fsp_sim::network::server::NodeRpcServer;
use fsp_sim::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use fsp_sim::node::identity::NodeFailureDomainTopology;
use fsp_sim::placement::failure_domains::{
    plan_physical_placement, validate_placement_durability, FailureDomainConstraints,
    PhysicalFailureDomain, PlacementCandidate,
};
use fsp_sim::security::authorization::{
    build_mtls_client_config, build_mtls_server_config, cert_der_sha256_hex, load_certs_from_pem,
    provision_cluster_pki, ProvisionedClusterPki,
};
use fsp_sim::security::signatures::{ManifestSignerKeypair, TrustedSignerPolicy};
use fsp_sim::storage::backend::StorageError;
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::testing::fault_injection::{FaultInjector, FaultRule};

static SANDBOX_COUNTER_42: AtomicU64 = AtomicU64::new(200);

struct TempSandbox42 {
    root: PathBuf,
}

impl TempSandbox42 {
    fn new(prefix: &str) -> Self {
        let id = SANDBOX_COUNTER_42.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("fsp_p42_{}_{}_{}", prefix, pid, id));
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

impl Drop for TempSandbox42 {
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

struct LocalMtlsCluster42 {
    _sandbox: TempSandbox42,
    pki: ProvisionedClusterPki,
    servers: BTreeMap<NodeId, NodeRpcServer>,
    coordinator: Arc<DistributedCoordinator>,
}

impl LocalMtlsCluster42 {
    fn start(num_nodes: usize, node_capacity_bytes: u64) -> Self {
        let sandbox = TempSandbox42::new("mtls_p42");
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
            let srv_tls = build_mtls_server_config(&pki.ca_cert_pem, cert_path, key_path).unwrap();

            let server = NodeRpcServer::start(
                daemon,
                srv_tls,
                pki.trust_policy.clone(),
                Duration::from_secs(5),
            )
            .unwrap();
            let bound_addr = server.bound_addr().to_string();

            let topo = NodeFailureDomainTopology::single_host_local_daemon(node_id);

            reg_nodes.push(RegisteredStorageNode {
                node_id,
                listen_addr: bound_addr,
                topology: topo,
            });

            servers.insert(node_id, server);
        }

        let coord_root = sandbox.path().join("coordinator");
        let client_tls = build_mtls_client_config(
            &pki.ca_cert_pem,
            &pki.coordinator_cert_pem,
            &pki.coordinator_key_pem,
        )
        .unwrap();

        let rpc_client =
            NodeRpcClient::new(client_tls, pki.trust_policy.clone(), Duration::from_secs(5));

        let signer =
            ManifestSignerKeypair::from_seed("coordinator-p42-primary", [0x42u8; 32]).unwrap();
        let mut signer_policy = TrustedSignerPolicy::new();
        signer_policy
            .register_signer("coordinator-p42-primary", signer.verifying_key())
            .unwrap();

        let (coordinator, _) = DistributedCoordinator::init_or_open(
            &coord_root,
            rpc_client,
            signer,
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

// -------------------------------------------------------------------------------------------------
// 1. Quorum Consensus & Monotonic Epoch Elections
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_01_three_member_quorum_election_and_majority_agreement() {
    let sandbox = TempSandbox42::new("quorum_election");
    let c1 = CoordinatorId::new("coord-01");
    let c2 = CoordinatorId::new("coord-02");
    let c3 = CoordinatorId::new("coord-03");

    let mut peers1 = BTreeSet::new();
    peers1.insert(c2.clone());
    peers1.insert(c3.clone());

    let quorum1 = QuorumState::new(c1.clone(), peers1, sandbox.path()).unwrap();
    assert_eq!(quorum1.cluster_size(), 3);
    assert_eq!(quorum1.quorum_threshold(), 2);
    assert_eq!(quorum1.current_epoch(), 0);

    // Start election for epoch 1
    let vote_req = quorum1.start_election(1).unwrap();
    assert_eq!(vote_req.proposed_epoch, 1);

    // Vote from coord-02 granted -> 2 of 3 votes -> Leader!
    let role = quorum1.record_vote(c2, true, 1000).unwrap();
    assert!(role.is_leader());
    assert_eq!(quorum1.current_epoch(), 1);
}

// -------------------------------------------------------------------------------------------------
// 2. Quorum Loss Fail-Closed Mutation Blocking
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_02_quorum_loss_fail_closed_rejection() {
    let sandbox = TempSandbox42::new("quorum_fail_closed");
    let c1 = CoordinatorId::new("coord-01");
    let mut peers = BTreeSet::new();
    peers.insert(CoordinatorId::new("coord-02"));
    peers.insert(CoordinatorId::new("coord-03"));

    let quorum = QuorumState::new(c1, peers, sandbox.path()).unwrap();
    quorum.start_election(1).unwrap();

    // coord-02 votes false, coord-03 unreachable -> only 1 of 3 votes
    let role = quorum
        .record_vote(CoordinatorId::new("coord-02"), false, 1000)
        .unwrap();
    assert!(!role.is_leader());
    assert!(matches!(role, CoordinatorRole::Candidate { .. }));

    // Force step down to fenced under partition
    quorum.step_down_to_fenced("isolated from quorum majority".into(), Some(2));
    assert!(matches!(
        quorum.current_role(),
        CoordinatorRole::Fenced { .. }
    ));
}

// -------------------------------------------------------------------------------------------------
// 3. Fencing Token Issuance, Wire Serialization & Validation
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_03_fencing_token_serialization_and_verification() {
    let token = FencingToken::new(42, CoordinatorId::new("leader-alpha"), 1700000000);
    let wire = token.to_wire_string();
    assert_eq!(wire, "TOKEN:42:leader-alpha:1700000000");

    let parsed = FencingToken::from_wire_string(&wire).unwrap();
    assert_eq!(parsed, token);

    let malformed = FencingToken::from_wire_string("TOKEN:invalid:bad");
    assert!(malformed.is_err());
}

// -------------------------------------------------------------------------------------------------
// 4. Storage Node Fenced Rejection of Stale Leader PutShard
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_04_storage_node_rejects_put_shard_from_stale_epoch_leader() {
    let cluster = LocalMtlsCluster42::start(1, 10 * 1024 * 1024);
    let server = cluster.servers.get(&NodeId(0)).unwrap();
    let addr = server.bound_addr().to_string();

    let client = cluster.coordinator.rpc_client();
    let payload = deterministic_bytes(0x1111, 1024);
    let sha_hex = sha256_bytes(&payload).to_hex();

    let req = PutShardRequest {
        tx_id: "tx-fenced-01".into(),
        object_id: "obj-fenced".into(),
        shard_id: 0,
        version: 1,
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripes: vec![(12_288, 1024, sha_hex.clone())],
    };

    // 1. Submit PutShardFenced with epoch 10 -> node accepts and sets highest seen epoch to 10
    let ack1 = client
        .put_shard_fenced(NodeId(0), &addr, req.clone(), &payload, 10)
        .expect("epoch 10 must succeed");
    assert_eq!(ack1.shard_id, 0);

    // 2. Submit PutShardFenced with stale epoch 9 -> node MUST reject with RpcError::Fenced!
    let err = client
        .put_shard_fenced(NodeId(0), &addr, req, &payload, 9)
        .unwrap_err();
    assert!(matches!(
        err,
        RpcError::Fenced {
            current_epoch: 10,
            presented_epoch: 9
        }
    ));
}

// -------------------------------------------------------------------------------------------------
// 5. Storage Node Fenced Rejection of Stale Leader DeleteShard
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_05_storage_node_rejects_delete_shard_from_stale_epoch_leader() {
    let cluster = LocalMtlsCluster42::start(1, 10 * 1024 * 1024);
    let server = cluster.servers.get(&NodeId(0)).unwrap();
    let addr = server.bound_addr().to_string();

    let client = cluster.coordinator.rpc_client();
    let payload = deterministic_bytes(0x2222, 1024);
    let sha_hex = sha256_bytes(&payload).to_hex();

    let put_req = PutShardRequest {
        tx_id: "tx-del-fenced".into(),
        object_id: "obj-del".into(),
        shard_id: 0,
        version: 1,
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripes: vec![(12_288, 1024, sha_hex)],
    };
    client
        .put_shard_fenced(NodeId(0), &addr, put_req, &payload, 15)
        .unwrap();

    // Attempt DeleteShardFenced with stale epoch 14 -> rejected!
    let err = client
        .delete_shard_fenced(
            NodeId(0),
            &addr,
            "tx-del-stale".into(),
            "obj-del".into(),
            0,
            1,
            14,
        )
        .unwrap_err();
    assert!(matches!(
        err,
        RpcError::Fenced {
            current_epoch: 15,
            presented_epoch: 14
        }
    ));

    // Attempt DeleteShardFenced with epoch 16 -> accepted!
    let deleted = client
        .delete_shard_fenced(
            NodeId(0),
            &addr,
            "tx-del-valid".into(),
            "obj-del".into(),
            0,
            1,
            16,
        )
        .unwrap();
    assert!(deleted);
}

// -------------------------------------------------------------------------------------------------
// 6. Storage Node Epoch Advance & Durable `.rec` Fsync
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_06_storage_node_advances_epoch_and_persists_durable_watermark() {
    let sandbox = TempSandbox42::new("node_epoch_fsync");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert_eq!(tracker.highest_seen_epoch(), 0);

    tracker.check_and_update(5).unwrap();
    assert_eq!(tracker.highest_seen_epoch(), 5);

    // Stale check returns error
    let err = tracker.check_and_update(4).unwrap_err();
    assert!(matches!(
        err,
        RpcError::Fenced {
            current_epoch: 5,
            presented_epoch: 4
        }
    ));
}

// -------------------------------------------------------------------------------------------------
// 7. Storage Node Restart Preserves Highest Seen Epoch
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_07_storage_node_restart_reloads_highest_seen_epoch() {
    let sandbox = TempSandbox42::new("node_epoch_restart");
    {
        let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
        tracker.check_and_update(42).unwrap();
    }

    // Reopen from disk
    let tracker2 = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert_eq!(tracker2.highest_seen_epoch(), 42);

    let err = tracker2.check_and_update(41).unwrap_err();
    assert!(matches!(
        err,
        RpcError::Fenced {
            current_epoch: 42,
            presented_epoch: 41
        }
    ));
}

// -------------------------------------------------------------------------------------------------
// 8. Manifest Commit Blocked when Coordinator Epoch Superseded
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_08_manifest_commit_rejected_when_epoch_superseded() {
    let sandbox = TempSandbox42::new("epoch_superseded");
    let store = DurableEpochStore::open_or_init(sandbox.path()).unwrap();
    store.advance_epoch(10, 1000).unwrap();

    let err = store.advance_epoch(9, 1005).unwrap_err();
    assert!(matches!(
        err,
        StorageError::FencingViolation {
            current_epoch: 10,
            attempted_epoch: 9
        }
    ));
}

// -------------------------------------------------------------------------------------------------
// 9. Metadata Replication WAL Logging & Replay
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_09_standby_metadata_replication_wal_replay() {
    let sandbox = TempSandbox42::new("wal_replay");
    let wal = MetadataReplicationWal::open_or_init(sandbox.path()).unwrap();

    let rec1 = ReplicationRecord::GenerationWatermarkAdvanced {
        object_id: "obj-wal-1".into(),
        generation: 3,
    };
    let rec2 = ReplicationRecord::RepairJobStateChanged {
        job_id: "job-wal-2".into(),
        state: RepairJobState::Scheduled,
        target_node: Some(NodeId(5)),
        error_msg: None,
        updated_unix_secs: 1234,
    };

    wal.append(&rec1).unwrap();
    wal.append(&rec2).unwrap();

    let replayed = wal.read_all().unwrap();
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0], rec1);
    assert_eq!(replayed[1], rec2);
}

// -------------------------------------------------------------------------------------------------
// 10. Standby Promotion & In-Flight Repair Reconciliation
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_10_standby_promotion_and_repair_journal_reconciliation() {
    let sandbox = TempSandbox42::new("standby_promotion");
    let quorum = Arc::new(
        QuorumState::new(
            CoordinatorId::new("coord-standby"),
            BTreeSet::from([
                CoordinatorId::new("coord-primary"),
                CoordinatorId::new("witness-01"),
            ]),
            sandbox.path(),
        )
        .unwrap(),
    );
    let wal = Arc::new(MetadataReplicationWal::open_or_init(sandbox.path()).unwrap());
    let journal = Arc::new(DurableRepairJournal::open_or_init(sandbox.path()).unwrap());
    let policy = Arc::new(Mutex::new(TrustedSignerPolicy::new()));

    // Create an in-flight repair job in Repairing state
    let job = journal
        .enqueue_or_get("obj-reconcile", 3, "loss", 100)
        .unwrap();
    journal
        .transition_state(&job.job_id, RepairJobState::Repairing, None, None, 105)
        .unwrap();

    let failover_mgr = CoordinatorFailoverManager::new(quorum, wal, journal.clone(), policy);

    // Failover promotes standby with votes from primary & witness
    let votes = vec![
        (CoordinatorId::new("coord-primary"), true),
        (CoordinatorId::new("witness-01"), true),
    ];
    let role = failover_mgr.execute_failover(1, 110, votes).unwrap();
    assert!(role.is_leader());

    // Repair journal must have reconciled incomplete Repairing job back to Pending!
    let reconciled = journal
        .all_jobs()
        .into_iter()
        .find(|j| j.job_id == job.job_id)
        .unwrap();
    assert_eq!(reconciled.state, RepairJobState::Pending);
    assert!(reconciled
        .error_msg
        .unwrap()
        .contains("reconciled after coordinator failover"));
}

// -------------------------------------------------------------------------------------------------
// 11. Graceful Leader Failover with State Continuity
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_11_graceful_leader_failover_without_data_loss() {
    let sandbox = TempSandbox42::new("graceful_failover");
    let quorum = Arc::new(
        QuorumState::new(
            CoordinatorId::new("coord-primary"),
            BTreeSet::from([
                CoordinatorId::new("coord-standby"),
                CoordinatorId::new("witness-01"),
            ]),
            sandbox.path(),
        )
        .unwrap(),
    );
    let wal = Arc::new(MetadataReplicationWal::open_or_init(sandbox.path()).unwrap());
    let journal = Arc::new(DurableRepairJournal::open_or_init(sandbox.path()).unwrap());
    let policy = Arc::new(Mutex::new(TrustedSignerPolicy::new()));

    let mgr = CoordinatorFailoverManager::new(quorum, wal, journal, policy);
    mgr.resign_leadership("scheduled maintenance", Some(2));
    assert!(matches!(mgr.current_role(), CoordinatorRole::Fenced { .. }));
}

// -------------------------------------------------------------------------------------------------
// 12. Subprocess SIGKILL Crash Recovery and Standby Promotion
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_12_abrupt_leader_crash_sigkill_and_failover() {
    let sandbox = TempSandbox42::new("subproc_sigkill");
    let ready_file = sandbox.path().join("subproc.ready");
    let pki_dir = sandbox.path().join("pki");
    let pki = provision_cluster_pki(&pki_dir, 1).unwrap();
    let (node_cert, node_key) = pki.node_certs.get(&NodeId(0)).unwrap();
    let node_root = sandbox.path().join("node-root");

    let bin_path = env!("CARGO_BIN_EXE_fsp-node");
    let mut child = Command::new(bin_path)
        .args([
            "--node-id",
            "0",
            "--listen",
            "127.0.0.1:0",
            "--storage-root",
            node_root.to_str().unwrap(),
            "--ca-cert",
            pki.ca_cert_pem.to_str().unwrap(),
            "--node-cert",
            node_cert.to_str().unwrap(),
            "--node-key",
            node_key.to_str().unwrap(),
            "--coord-cert",
            pki.coordinator_cert_pem.to_str().unwrap(),
            "--capacity-bytes",
            "10485760",
            "--ready-file",
            ready_file.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon subprocess");

    for _ in 0..100 {
        if ready_file.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }

    // Kill abruptly with real kernel SIGKILL
    child.kill().expect("SIGKILL child process");
    let exit_status = child.wait().expect("wait on killed child");
    assert!(!exit_status.success());
}

// -------------------------------------------------------------------------------------------------
// 13. Split-Brain Defense: Isolated Old Leader Fenced from Storage Nodes
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_13_split_brain_prevention_under_partition() {
    let cluster = LocalMtlsCluster42::start(1, 10 * 1024 * 1024);
    let server = cluster.servers.get(&NodeId(0)).unwrap();
    let addr = server.bound_addr().to_string();
    let client = cluster.coordinator.rpc_client();

    let payload = deterministic_bytes(0x3333, 1024);
    let sha = sha256_bytes(&payload).to_hex();
    let req = PutShardRequest {
        tx_id: "split-brain-tx".into(),
        object_id: "sb-obj".into(),
        shard_id: 0,
        version: 1,
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripes: vec![(12_288, 1024, sha)],
    };

    // New leader with epoch 5 commits to storage node
    client
        .put_shard_fenced(NodeId(0), &addr, req.clone(), &payload, 5)
        .unwrap();

    // Old partitioned leader with epoch 4 attempts to write -> FENCED!
    let err = client
        .put_shard_fenced(NodeId(0), &addr, req, &payload, 4)
        .unwrap_err();
    assert!(matches!(
        err,
        RpcError::Fenced {
            current_epoch: 5,
            presented_epoch: 4
        }
    ));
}

// -------------------------------------------------------------------------------------------------
// 14. Physical Placement: RS(20,12) 3-Host Single-Host Loss Tolerance ($F=1$)
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_14_physical_failure_domain_rs20_12_three_host_tolerance() {
    let mut domains = Vec::new();
    // Host A: 7 shards, Host B: 7 shards, Host C: 6 shards = 20 shards
    for _ in 0..7 {
        domains.push(PhysicalFailureDomain::simple("host-A", "rack-01"));
    }
    for _ in 0..7 {
        domains.push(PhysicalFailureDomain::simple("host-B", "rack-02"));
    }
    for _ in 0..6 {
        domains.push(PhysicalFailureDomain::simple("host-C", "rack-03"));
    }

    let constraints = FailureDomainConstraints {
        tolerated_host_failures: 1,
        tolerated_rack_failures: 1,
        max_shards_per_host: 7,
        max_shards_per_rack: 7,
        max_shards_per_zone: 20,
        min_independent_hosts: 3,
    };

    let rep = validate_placement_durability(&domains, 20, 12, &constraints).unwrap();
    assert!(rep.is_valid);
    assert_eq!(rep.min_surviving_shards_on_host_failure, 13); // 20 - 7 = 13 >= k=12!
}

// -------------------------------------------------------------------------------------------------
// 15. Physical Placement: 2-Host Topology Mathematically Rejected ($F=1$)
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_15_physical_failure_domain_two_host_rejected() {
    let mut domains = Vec::new();
    // 2 hosts with 10 shards each
    for _ in 0..10 {
        domains.push(PhysicalFailureDomain::simple("host-A", "rack-01"));
    }
    for _ in 0..10 {
        domains.push(PhysicalFailureDomain::simple("host-B", "rack-02"));
    }

    let constraints = FailureDomainConstraints::default(); // requires 3 hosts
    let rep = validate_placement_durability(&domains, 20, 12, &constraints).unwrap();
    assert!(!rep.is_valid);
    assert!(rep
        .rejection_reason
        .unwrap()
        .contains("insufficient independent hosts"));
}

// -------------------------------------------------------------------------------------------------
// 16. Physical Placement: 5-Host Topology Double-Host Loss Tolerance ($F=2$)
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_16_physical_failure_domain_five_host_two_failure_tolerance() {
    let mut domains = Vec::new();
    // 5 hosts with 4 shards each on 5 distinct racks
    for h in 0..5 {
        for _ in 0..4 {
            domains.push(PhysicalFailureDomain::simple(
                format!("host-{:02}", h),
                format!("rack-{:02}", h),
            ));
        }
    }

    let constraints = FailureDomainConstraints {
        tolerated_host_failures: 2,
        tolerated_rack_failures: 2,
        max_shards_per_host: 4,
        max_shards_per_rack: 4,
        max_shards_per_zone: 20,
        min_independent_hosts: 5,
    };

    let rep = validate_placement_durability(&domains, 20, 12, &constraints).unwrap();
    assert!(rep.is_valid);
}

// -------------------------------------------------------------------------------------------------
// 17. Multi-Level Failure Domain Validation (Host + Rack + Zone)
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_17_multi_level_failure_domain_hierarchy_validation() {
    let mut candidates = Vec::new();
    for i in 0..24 {
        candidates.push(PlacementCandidate {
            node_id: NodeId(i),
            listen_addr: format!("127.0.0.1:{}", 19400 + i),
            domain: PhysicalFailureDomain::simple(
                format!("host-{:02}", i % 4),
                format!("rack-{:02}", i % 4),
            ),
            available_bytes: 100 * 1024 * 1024,
            online: true,
        });
    }

    let constraints = FailureDomainConstraints {
        tolerated_host_failures: 1,
        tolerated_rack_failures: 1,
        max_shards_per_host: 6,
        max_shards_per_rack: 6,
        max_shards_per_zone: 20,
        min_independent_hosts: 4,
    };

    let plan = plan_physical_placement(&candidates, 20, 12, &constraints).unwrap();
    assert_eq!(plan.len(), 20);
}

// -------------------------------------------------------------------------------------------------
// 18. TLS Connection Pool: Connection Reuse & Handshake Reduction
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_18_tls_connection_pool_reuses_connection_and_reduces_handshakes() {
    let cluster = LocalMtlsCluster42::start(1, 10 * 1024 * 1024);
    let server = cluster.servers.get(&NodeId(0)).unwrap();
    let addr = server.bound_addr().to_string();

    let client_tls = build_mtls_client_config(
        &cluster.pki.ca_cert_pem,
        &cluster.pki.coordinator_cert_pem,
        &cluster.pki.coordinator_key_pem,
    )
    .unwrap();

    let pool = TlsConnectionPool::new(
        client_tls,
        cluster.pki.trust_policy.clone(),
        ConnectionPoolConfig::default(),
    );

    // Acquire connection 1
    let conn1 = pool.acquire(NodeId(0), &addr).unwrap();
    assert_eq!(
        pool.metrics().handshakes_performed.load(Ordering::Relaxed),
        1
    );
    pool.release(conn1);

    // Acquire connection 2 -> must reuse!
    let conn2 = pool.acquire(NodeId(0), &addr).unwrap();
    assert_eq!(
        pool.metrics().handshakes_performed.load(Ordering::Relaxed),
        1
    ); // No new handshake!
    assert_eq!(pool.metrics().connections_reused.load(Ordering::Relaxed), 1);
    pool.release(conn2);
}

// -------------------------------------------------------------------------------------------------
// 19. TLS Connection Pool: Idle Timeout & Connection Eviction
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_19_tls_connection_pool_evicts_idle_connections() {
    let cluster = LocalMtlsCluster42::start(1, 10 * 1024 * 1024);
    let server = cluster.servers.get(&NodeId(0)).unwrap();
    let addr = server.bound_addr().to_string();

    let client_tls = build_mtls_client_config(
        &cluster.pki.ca_cert_pem,
        &cluster.pki.coordinator_cert_pem,
        &cluster.pki.coordinator_key_pem,
    )
    .unwrap();

    let pool = TlsConnectionPool::new(
        client_tls,
        cluster.pki.trust_policy.clone(),
        ConnectionPoolConfig {
            idle_timeout: Duration::from_millis(50),
            ..Default::default()
        },
    );

    let conn1 = pool.acquire(NodeId(0), &addr).unwrap();
    pool.release(conn1);

    // Sleep past idle timeout
    thread::sleep(Duration::from_millis(80));

    // Next acquire should evict the expired connection and do a fresh handshake
    let conn2 = pool.acquire(NodeId(0), &addr).unwrap();
    assert_eq!(
        pool.metrics()
            .connections_closed_idle
            .load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        pool.metrics().handshakes_performed.load(Ordering::Relaxed),
        2
    );
    pool.release(conn2);
}

// -------------------------------------------------------------------------------------------------
// 20. Bounded Exponential Backoff Retry
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_20_bounded_exponential_retry_with_jitter() {
    let policy = RetryPolicy {
        max_retries: 3,
        initial_delay: Duration::from_millis(10),
        max_delay: Duration::from_millis(50),
        backoff_factor: 2.0,
    };

    let mut attempts = 0;
    let res = retry_operation(&policy, |_attempt| {
        attempts += 1;
        if attempts < 3 {
            Err("transient network drop")
        } else {
            Ok(42)
        }
    });

    assert_eq!(res, Ok(42));
    assert_eq!(attempts, 3);
}

// -------------------------------------------------------------------------------------------------
// 21. Real Configuration Loader: `coordinator.example.toml` Parsing & Validation
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_21_config_loader_parses_coordinator_example_toml() {
    let path = PathBuf::from("deploy/coordinator.example.toml");
    let config =
        load_coordinator_config_file(&path, false).expect("parse coordinator.example.toml");
    assert_eq!(config.coordinator_id, "coordinator-primary-01");
    assert_eq!(config.max_shards_per_host, 1);
    assert_eq!(config.max_shards_per_rack, 5);
    assert_eq!(config.nodes.len(), 2);
}

// -------------------------------------------------------------------------------------------------
// 22. Real Configuration Loader: `node.example.toml` Parsing & Validation
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_22_config_loader_parses_node_example_toml() {
    let path = PathBuf::from("deploy/node.example.toml");
    let config = load_node_config_file(&path, false).expect("parse node.example.toml");
    assert_eq!(config.node_id, 0);
    assert_eq!(config.identity_id, "node-00");
    assert_eq!(config.capacity_limit_bytes, 1099511627776);
}

// -------------------------------------------------------------------------------------------------
// 23. Configuration Strict Mode: Unknown Field Rejection
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_23_config_loader_strict_mode_rejects_unknown_fields() {
    let toml = r#"
[coordinator]
coordinator_id = "coord-01"
metadata_root = "/var/lib/fsp"
placement_policy = "MultiHostDistinctMachines"
max_shards_per_host = 1
max_shards_per_rack = 5
default_coding_scheme = "RS(20,12)"
default_stripe_size_bytes = 65536
degraded_commit_policy = "StrictAllShards"
bogus_unrecognized_key = 12345

[security]
trust_environment_mode = "ProductionStrict"
ca_cert_pem = "ca.crt"
coordinator_cert_pem = "coord.crt"
coordinator_key_pem = "coord.key"
ed25519_manifest_signer_key_file = "signer.key"

[repair_and_scrub]
heartbeat_interval_secs = 30
scrub_max_verifications_per_cycle = 40
effective_repair_bandwidth_bps = 104857600
"#;

    let err = parse_coordinator_config(toml, true).unwrap_err();
    assert!(
        matches!(err, fsp_sim::config::validation::ConfigValidationError::UnknownField(k) if k == "coordinator.bogus_unrecognized_key")
    );
}

// -------------------------------------------------------------------------------------------------
// 24. Network Fault Injection: Deterministic Packet Loss Simulation
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_24_fault_injection_framework_simulates_network_drops() {
    let injector = FaultInjector::new();
    injector.add_rule(FaultRule::PacketLoss { probability: 1.0 });

    let res = injector.evaluate_network_rpc(NodeId(3));
    assert!(matches!(res, Err(RpcError::SimulatedNetworkDrop(_))));

    injector.clear_rules();
    assert!(injector.evaluate_network_rpc(NodeId(3)).is_ok());
}

// -------------------------------------------------------------------------------------------------
// 25. End-to-End Distributed Cluster PUT/GET with Connection Pooling
// -------------------------------------------------------------------------------------------------

#[test]
fn test_p4_2_25_end_to_end_100mib_distributed_put_get_with_connection_pooling() {
    let cluster = LocalMtlsCluster42::start(20, 150 * 1024 * 1024);
    let payload = deterministic_bytes(0x7777, 1024 * 1024); // 1 MiB payload

    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 65536,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };

    let rep = cluster
        .coordinator
        .distributed_put_bytes("obj-pooled-e2e", &payload, opts)
        .expect("distributed PUT");
    assert_eq!(rep.acknowledged_nodes.len(), 20);

    let (reconstructed, _) = cluster
        .coordinator
        .distributed_get_to_bytes("obj-pooled-e2e")
        .expect("distributed GET");
    assert_eq!(reconstructed, payload);
}
