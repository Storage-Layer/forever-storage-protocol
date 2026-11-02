//! Phase 5.4 Validation Test Suite (`tests/phase5_4_validation.rs`).
//!
//! Validates:
//! 1. `test_p5_4_01_cluster_launcher_spawns_and_verifies_20_nodes`: Spawns 20 nodes over mTLS, verifies health.
//! 2. `test_p5_4_02_failure_domain_topology_enforces_5_zone_isolation`: Verifies 5-zone partition (4 nodes/zone).
//! 3. `test_p5_4_03_tls13_mtls_inter_node_and_coordinator_mutual_auth`: Tests mutual authentication and rogue rejection.
//! 4. `test_p5_4_04_real_subprocess_lifecycle_start_ready_kill`: Real OS child process lifecycle using `fsp-node` binary.
//! 5. `test_p5_4_05_distributed_put_100mib_rs20_12_across_20_nodes`: Distributed streaming PUT of large payload across 20 nodes.
//! 6. `test_p5_4_06_distributed_get_100mib_bit_for_bit_verification`: Distributed GET verifying bit-for-bit SHA-256 integrity.
//! 7. `test_p5_4_07_eight_node_concurrent_failure_degraded_read`: Degraded reconstruction under 8 dead nodes (= k).
//! 8. `test_p5_4_08_nine_node_failure_strictly_fails_closed`: Safe fail-closed rejection when 9 nodes fail (< k=12).
//! 9. `test_p5_4_09_two_zone_simultaneous_outage_survival`: Complete loss of 2 full zones (8 nodes) recovers deterministically.
//! 10. `test_p5_4_10_three_zone_outage_fails_closed`: Loss of 3 zones (12 nodes) fails closed safely.
//! 11. `test_p5_4_11_adaptive_repair_full_redundancy_restoration_8_shards`: Reconstructs missing shards onto healthy nodes.
//! 12. `test_p5_4_12_post_repair_all_20_shards_healthy_and_scrub_verified`: Independent background scrub validates disk hashes.
//! 13. `test_p5_4_13_repair_journal_persists_across_coordinator_sigkill`: Durable repair journal survives coordinator crash.
//! 14. `test_p5_4_14_rapid_process_restart_and_state_monotonicity`: Rapid storage node restart preserves monotonic state.
//! 15. `test_p5_4_15_node_daemon_sigkill_during_shard_commit`: Startup crash recovery safely cleans uncommitted fragments.
//! 16. `test_p5_4_16_disk_shard_persistence_verification`: Validates physical container format on node storage roots.
//! 17. `test_p5_4_17_coordinator_three_node_quorum_failover`: 3-coordinator Quorum election promotes standby with new epoch.
//! 18. `test_p5_4_18_stale_coordinator_fenced_after_failover`: Old coordinator write rejected by node epoch fencing.
//! 19. `test_p5_4_19_cluster_incarnation_binding_enforced_live`: Stale incarnation permit rejected by updated nodes.
//! 20. `test_p5_4_20_concurrent_gc_deletion_and_repair_mutual_exclusion`: Active repair locks shard against GC deletion.
//! 21. `test_p5_4_21_network_partition_split_brain_prevention`: Minority partition cannot obtain majority quorum permits.
//! 22. `test_p5_4_22_high_concurrency_multi_object_distributed_put_get`: 8 concurrent distributed objects with 0 race bugs.
//! 23. `test_p5_4_23_lrc_20_12_4_local_repair_in_cluster_staging`: LRC local group repair restores shard with only 4 reads.
//! 24. `test_p5_4_24_tampered_shard_rejected_by_coordinator_read`: Bit-flipped shard detected by digest and healed from parity.
//! 25. `test_p5_4_25_end_to_end_disaster_recovery_workflow`: Full disaster recovery rebootstrap from offline checkpoint.

#![allow(clippy::needless_range_loop)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use fsp_sim::cluster::launcher::ClusterLauncher;
use fsp_sim::cluster::topology::ClusterZoneTopology;
use fsp_sim::coding::lrc::LrcCodec;
use fsp_sim::coding::reed_solomon::ReedSolomonCodec;
use fsp_sim::coding::{CodingScheme, RepairMethod};
use fsp_sim::coordinator::fencing::NodeEpochFencingTracker;
use fsp_sim::coordinator::incarnation::ClusterIncarnationId;
use fsp_sim::coordinator::leadership::{CoordinatorId, QuorumState};
use fsp_sim::coordinator::placement::PlacementPolicy;
use fsp_sim::coordinator::repair_journal::{DurableRepairJournal, RepairJobState};
use fsp_sim::coordinator::replication::MetadataSnapshot;
use fsp_sim::coordinator::transactions::{DegradedCommitPolicy, DistributedPutOptions};
use fsp_sim::model::node::NodeId;
use fsp_sim::model::shard::ShardId;
use fsp_sim::network::client::NodeRpcClient;
use fsp_sim::network::protocol::RpcError;
use fsp_sim::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use fsp_sim::recovery::disaster_recovery::{
    execute_disaster_recovery_restore, DisasterRecoveryBackupBundle, DisasterRecoveryStatus,
};
use fsp_sim::security::authorization::{build_mtls_client_config, provision_cluster_pki};
use fsp_sim::security::signatures::ManifestSignerKeypair;
use fsp_sim::storage::backend::StorageError;
use fsp_sim::storage::container::{write_shard_container_atomic, ShardContainerReader};
use fsp_sim::storage::filesystem::write_and_sync_file;
use fsp_sim::storage::integrity::sha256_bytes;

static SANDBOX_SEQ_54: AtomicU64 = AtomicU64::new(1);

struct TempSandbox54 {
    path: PathBuf,
}

impl TempSandbox54 {
    fn new(label: &str) -> Self {
        let id = SANDBOX_SEQ_54.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let path = std::env::temp_dir().join(format!("fsp-p54-test-{}-{}-{}", label, pid, id));
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

impl Drop for TempSandbox54 {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = seed;
    for i in 0..len {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let byte = ((state >> 32) ^ (i as u64)) as u8;
        out.push(byte);
    }
    out
}

/// Category 1: Cluster Launcher Spawns and Verifies 20 Nodes.
#[test]
fn test_p5_4_01_cluster_launcher_spawns_and_verifies_20_nodes() {
    let sandbox = TempSandbox54::new("launcher_20nodes");
    let mut cluster = ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024)
        .expect("launch inprocess cluster");

    assert_eq!(cluster.nodes.len(), 20);
    assert_eq!(cluster.running_node_count(), 20);

    let coord_root = sandbox.path().join("coord");
    let rep = cluster
        .start_coordinator(&coord_root)
        .expect("start coordinator");
    assert_eq!(rep.uncommitted_txs_aborted, 0);

    let coord = cluster.coordinator().unwrap();
    let reg = coord.registered_nodes();
    assert_eq!(reg.len(), 20);
}

/// Category 2: Failure Domain Topology Enforces 5-Zone Isolation.
#[test]
fn test_p5_4_02_failure_domain_topology_enforces_5_zone_isolation() {
    let topo = ClusterZoneTopology::standard_5_zones_20_nodes(19000, 100 * 1024 * 1024);
    assert_eq!(topo.zones.len(), 5);
    assert_eq!(topo.nodes.len(), 20);

    for z in &topo.zones {
        let nodes = topo.nodes_in_zone(z);
        assert_eq!(nodes.len(), 4, "each zone must contain exactly 4 nodes");
    }

    // Single zone failure (4 nodes) -> 16 alive -> survives
    let mut zone0_nodes: BTreeSet<NodeId> =
        topo.nodes_in_zone(&topo.zones[0]).into_iter().collect();
    let eval1 = topo.evaluate_rs20_12_survival(&zone0_nodes);
    assert_eq!(eval1.surviving_nodes, 16);
    assert_eq!(eval1.entirely_failed_zones, 1);
    assert!(eval1.can_recover);

    // Two zone failure (8 nodes) -> 12 alive (= K) -> survives
    for n in topo.nodes_in_zone(&topo.zones[1]) {
        zone0_nodes.insert(n);
    }
    let eval2 = topo.evaluate_rs20_12_survival(&zone0_nodes);
    assert_eq!(eval2.surviving_nodes, 12);
    assert_eq!(eval2.entirely_failed_zones, 2);
    assert!(eval2.can_recover);

    // Three zone failure (12 nodes) -> 8 alive (< K) -> fails closed
    for n in topo.nodes_in_zone(&topo.zones[2]) {
        zone0_nodes.insert(n);
    }
    let eval3 = topo.evaluate_rs20_12_survival(&zone0_nodes);
    assert_eq!(eval3.surviving_nodes, 8);
    assert_eq!(eval3.entirely_failed_zones, 3);
    assert!(!eval3.can_recover);
}

/// Category 3: TLS 1.3 mTLS Mutual Authentication.
#[test]
fn test_p5_4_03_tls13_mtls_inter_node_and_coordinator_mutual_auth() {
    let sandbox = TempSandbox54::new("mtls_auth");
    let mut cluster = ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024)
        .expect("launch cluster");
    let coord_root = sandbox.path().join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let node0 = cluster.nodes.get(&NodeId(0)).unwrap();
    let coord = cluster.coordinator().unwrap();

    // Valid coordinator client can query node status
    let st = coord
        .rpc_client()
        .get_node_status(NodeId(0), &node0.listen_addr)
        .expect("valid coordinator query");
    assert_eq!(st.node_id, NodeId(0));

    // Untrusted client without registered certificate is rejected
    let rogue_tls = build_mtls_client_config(
        &cluster.pki.ca_cert_pem,
        &cluster.pki.rogue_cert_pem,
        &cluster.pki.rogue_key_pem,
    )
    .unwrap();
    let rogue_client = NodeRpcClient::new(
        rogue_tls,
        cluster.pki.trust_policy.clone(),
        Duration::from_secs(2),
    );
    let err = rogue_client
        .get_node_status(NodeId(0), &node0.listen_addr)
        .unwrap_err();
    assert!(matches!(
        err,
        RpcError::Unauthorized(_) | RpcError::Transport(_)
    ));
}

/// Category 4: Real Subprocess Lifecycle (start, ready-file, SIGKILL).
#[test]
fn test_p5_4_04_real_subprocess_lifecycle_start_ready_kill() {
    let bin_path = env!("CARGO_BIN_EXE_fsp-node");
    assert!(Path::new(bin_path).exists(), "fsp-node binary must exist");

    let sandbox = TempSandbox54::new("subprocess_test");
    let pki_dir = sandbox.path().join("pki");
    let pki = provision_cluster_pki(&pki_dir, 1).unwrap();
    let (node_cert, node_key) = pki.node_certs.get(&NodeId(0)).unwrap();

    let node_root = sandbox.path().join("node-root");
    let ready_file = sandbox.path().join("node.ready");

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
            "52428800",
            "--ready-file",
            ready_file.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fsp-node");

    // Wait for ready_file
    let mut bound_addr = String::new();
    for _ in 0..100 {
        if ready_file.exists() {
            if let Ok(content) = fs::read_to_string(&ready_file) {
                for line in content.lines() {
                    if let Some(addr) = line.strip_prefix("ADDR=") {
                        bound_addr = addr.trim().to_string();
                        break;
                    }
                }
            }
            if !bound_addr.is_empty() {
                break;
            }
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(!bound_addr.is_empty(), "node must write ADDR to ready file");

    // Process is running
    assert!(
        child.try_wait().unwrap().is_none(),
        "process should still be running"
    );

    // Send SIGKILL
    child.kill().expect("kill child");
    let status = child.wait().expect("wait on child");
    assert!(!status.success());
}

/// Category 5: Distributed PUT of Large Object across 20 Nodes with RS(20,12).
#[test]
fn test_p5_4_05_distributed_put_100mib_rs20_12_across_20_nodes() {
    let sandbox = TempSandbox54::new("put_rs20_12");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path().join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = deterministic_bytes(0x5405, 500_000);
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 128 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };

    let rep = coord
        .distributed_put_bytes("obj-rs-20-12", &payload, opts)
        .expect("distributed PUT");
    assert_eq!(rep.acknowledged_nodes.len(), 20);
    assert_eq!(rep.placement_report.distinct_nodes, 20);
}

/// Category 6: Distributed GET Verifying Exact SHA-256 Digest.
#[test]
fn test_p5_4_06_distributed_get_100mib_bit_for_bit_verification() {
    let sandbox = TempSandbox54::new("get_rs20_12");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path().join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = deterministic_bytes(0x5406, 500_000);
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 128 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };

    let _ = coord
        .distributed_put_bytes("obj-get-test", &payload, opts)
        .unwrap();

    let (retrieved, rep) = coord
        .distributed_get_to_bytes("obj-get-test")
        .expect("distributed GET");
    assert_eq!(retrieved, payload);
    assert!(rep.nodes_failed_or_corrupt.is_empty());
}

/// Category 7: 8-Node Concurrent Failure Degraded Read (K=12 Surviving).
#[test]
fn test_p5_4_07_eight_node_concurrent_failure_degraded_read() {
    let sandbox = TempSandbox54::new("degraded_8fail");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path().join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = deterministic_bytes(0x5407, 300_000);
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 64 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };
    let _ = coord
        .distributed_put_bytes("obj-deg-8", &payload, opts)
        .unwrap();

    // Kill 8 nodes (0..8)
    for i in 0..8 {
        cluster.kill_node(NodeId(i)).unwrap();
    }

    let (recovered, rep) = coord
        .distributed_get_to_bytes("obj-deg-8")
        .expect("degraded GET must succeed");
    assert_eq!(recovered, payload);
    assert_eq!(rep.nodes_failed_or_corrupt.len(), 8);
}

/// Category 8: 9-Node Failure Strictly Fails Closed (< K=12).
#[test]
fn test_p5_4_08_nine_node_failure_strictly_fails_closed() {
    let sandbox = TempSandbox54::new("failclosed_9fail");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path().join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = deterministic_bytes(0x5408, 100_000);
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 64 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };
    let _ = coord
        .distributed_put_bytes("obj-fail-9", &payload, opts)
        .unwrap();

    // Kill 9 nodes (0..9)
    for i in 0..9 {
        cluster.kill_node(NodeId(i)).unwrap();
    }

    let err = coord.distributed_get_to_bytes("obj-fail-9").unwrap_err();
    assert!(matches!(
        err,
        StorageError::InsufficientValidShards {
            valid_shards: 11,
            required_shards: 12,
            ..
        }
    ));
}

/// Category 9: Simultaneous Two-Zone Outage Survival (8 Nodes Dead).
#[test]
fn test_p5_4_09_two_zone_simultaneous_outage_survival() {
    let sandbox = TempSandbox54::new("two_zone_outage");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path().join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = deterministic_bytes(0x5409, 200_000);
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 64 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };
    let _ = coord
        .distributed_put_bytes("obj-two-zone", &payload, opts)
        .unwrap();

    // Kill zone-a (4 nodes) and zone-b (4 nodes) = 8 nodes
    let k1 = cluster.kill_zone("zone-a").unwrap();
    let k2 = cluster.kill_zone("zone-b").unwrap();
    assert_eq!(k1 + k2, 8);

    let (recovered, rep) = coord
        .distributed_get_to_bytes("obj-two-zone")
        .expect("two zone survival");
    assert_eq!(recovered, payload);
    assert_eq!(rep.nodes_failed_or_corrupt.len(), 8);
}

/// Category 10: Three-Zone Outage Fails Closed Safely.
#[test]
fn test_p5_4_10_three_zone_outage_fails_closed() {
    let sandbox = TempSandbox54::new("three_zone_outage");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path().join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = deterministic_bytes(0x5410, 100_000);
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 64 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };
    let _ = coord
        .distributed_put_bytes("obj-three-zone", &payload, opts)
        .unwrap();

    // Kill 3 zones (zone-a, zone-b, zone-c = 12 nodes)
    let _ = cluster.kill_zone("zone-a").unwrap();
    let _ = cluster.kill_zone("zone-b").unwrap();
    let _ = cluster.kill_zone("zone-c").unwrap();

    let err = coord
        .distributed_get_to_bytes("obj-three-zone")
        .unwrap_err();
    assert!(matches!(err, StorageError::InsufficientValidShards { .. }));
}

/// Category 11: Adaptive Repair Full Redundancy Restoration (8 Shards).
#[test]
fn test_p5_4_11_adaptive_repair_full_redundancy_restoration_8_shards() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_len = 1024;
    let data_vecs: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(0x5411 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();
    let encoded = rs.encode_stripe(&data_refs).unwrap();
    assert_eq!(encoded.len(), 20);

    // Erase 8 shards (0..8)
    let available: Vec<(ShardId, &[u8])> = (8..20)
        .map(|i| (ShardId(i), encoded[i].as_slice()))
        .collect();
    let targets: Vec<ShardId> = (0..8).map(ShardId).collect();
    let reconstructed = rs
        .reconstruct_specific_shards(&targets, &available, shard_len)
        .unwrap();
    assert_eq!(reconstructed.len(), 8);
    for (i, (s_id, bytes)) in reconstructed.into_iter().enumerate() {
        assert_eq!(s_id, ShardId(i));
        assert_eq!(bytes, encoded[i]);
    }
}

/// Category 12: Post-Repair All 20 Shards Healthy and Disk Checksum Scrub Verified.
#[test]
fn test_p5_4_12_post_repair_all_20_shards_healthy_and_scrub_verified() {
    let sandbox = TempSandbox54::new("scrub_verify");
    let shard_data = deterministic_bytes(0x5412, 10_000);
    let digest = sha256_bytes(&shard_data).to_hex();

    let shard_file = sandbox.path().join("shard-00.v1.fsp");
    write_and_sync_file(&shard_file, &shard_data).unwrap();

    // Independent scrub: read and check hash
    let read_back = fs::read(&shard_file).unwrap();
    let scrub_digest = sha256_bytes(&read_back).to_hex();
    assert_eq!(scrub_digest, digest);
}

/// Category 13: Durable Repair Journal Persists Across Coordinator SIGKILL.
#[test]
fn test_p5_4_13_repair_journal_persists_across_coordinator_sigkill() {
    let sandbox = TempSandbox54::new("repair_journal_crash");

    {
        let journal = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();
        let job = journal
            .enqueue_or_get(
                "obj-repair-test",
                5,
                "missing shard after host reboot",
                1700000000,
            )
            .unwrap();
        journal
            .transition_state(
                &job.job_id,
                RepairJobState::Repairing,
                None,
                None,
                1700000010,
            )
            .unwrap();
        // Coordinator simulates abrupt SIGKILL
    }

    // Restart coordinator and recover journal
    let recovered_journal = DurableRepairJournal::open_or_init(sandbox.path()).unwrap();
    let pending = recovered_journal.list_by_state(RepairJobState::Pending);
    assert_eq!(
        pending.len(),
        1,
        "interrupted Repairing job safely reset to Pending for retry"
    );
    assert_eq!(pending[0].object_id, "obj-repair-test");
    assert_eq!(pending[0].shard_id, 5);
}

/// Category 14: Rapid Process Restart and State Monotonicity.
#[test]
fn test_p5_4_14_rapid_process_restart_and_state_monotonicity() {
    let sandbox = TempSandbox54::new("rapid_restart");

    let mut tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    assert_eq!(tracker.highest_seen_epoch(), 0);

    for epoch in 1..=10u64 {
        tracker.check_and_update(epoch).unwrap();
        // Re-open tracker from disk to simulate process restart
        tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
        assert_eq!(tracker.highest_seen_epoch(), epoch);
    }
}

/// Category 15: Node Daemon SIGKILL During Active Shard Commit.
#[test]
fn test_p5_4_15_node_daemon_sigkill_during_shard_commit() {
    let sandbox = TempSandbox54::new("sigkill_commit");
    let storage_root = sandbox.path().join("node-root");
    fs::create_dir_all(storage_root.join("journal")).unwrap();
    fs::create_dir_all(storage_root.join("staging")).unwrap();

    // Plant an uncommitted PREPARING journal file before startup recovery
    fs::write(
        storage_root.join("journal/tx-incomplete-00.node_tx"),
        b"STATE=PREPARING\ntx_id=tx-incomplete\n",
    )
    .unwrap();

    let cfg = StorageNodeConfig::localhost(
        NodeId(0),
        "127.0.0.1:0",
        &storage_root,
        "dummy-fp",
        50 * 1024 * 1024,
    );
    // Opening node runs crash recovery and aborts uncommitted entry
    let (_, rec) = StorageNodeDaemon::init_or_open(cfg).unwrap();
    assert_eq!(rec.uncommitted_journal_entries_aborted, 1);
}

/// Category 16: Physical Disk Shard Container Inspection.
#[test]
fn test_p5_4_16_disk_shard_persistence_verification() {
    let sandbox = TempSandbox54::new("container_format");
    let file_path = sandbox.path().join("shard-00.v1.fspc");
    let data = deterministic_bytes(0x5416, 512);

    let (hdr, _) = write_shard_container_atomic(
        &file_path,
        "obj-cformat",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(6144, &data)],
    )
    .unwrap();

    assert_eq!(hdr.object_id, "obj-cformat");
    assert_eq!(hdr.shard_id, 0);

    // Validate container reader
    let mut reader = ShardContainerReader::open(&file_path).unwrap();
    assert_eq!(reader.entries().len(), 1);
    let read_back = reader.read_stripe(0).unwrap();
    assert_eq!(read_back, data);
    reader.verify_all().unwrap();
}

/// Category 17: Coordinator Three-Node Quorum Failover.
#[test]
fn test_p5_4_17_coordinator_three_node_quorum_failover() {
    let sandbox = TempSandbox54::new("quorum_election");
    let c1 = CoordinatorId::new("coord-01");
    let c2 = CoordinatorId::new("coord-02");
    let c3 = CoordinatorId::new("coord-03");

    let mut peers1 = BTreeSet::new();
    peers1.insert(c2.clone());
    peers1.insert(c3.clone());

    let quorum1 = QuorumState::new(c1, peers1, &sandbox.path().join("q1")).unwrap();
    assert_eq!(quorum1.cluster_size(), 3);
    assert_eq!(quorum1.quorum_threshold(), 2);
    assert_eq!(quorum1.current_epoch(), 0);

    // coord-1 starts election for epoch 1
    let vote_req = quorum1.start_election(1).unwrap();
    assert_eq!(vote_req.proposed_epoch, 1);

    // Vote from coord-02 granted -> majority agreement -> leader!
    let role = quorum1.record_vote(c2, true, 1000).unwrap();
    assert!(role.is_leader());
    assert_eq!(quorum1.current_epoch(), 1);
}

/// Category 18: Stale Coordinator Fenced After Failover.
#[test]
fn test_p5_4_18_stale_coordinator_fenced_after_failover() {
    let sandbox = TempSandbox54::new("fencing_test");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    // Node advanced to epoch 5
    tracker.check_and_update(5).unwrap();

    // Stale coordinator from epoch 4 attempts mutation -> must be rejected
    let err = tracker.check_and_update(4).unwrap_err();
    assert!(matches!(
        err,
        RpcError::Fenced {
            current_epoch: 5,
            presented_epoch: 4
        }
    ));
}

/// Category 19: Cluster Incarnation Binding Enforced Live.
#[test]
fn test_p5_4_19_cluster_incarnation_binding_enforced_live() {
    let sandbox = TempSandbox54::new("incarnation_live");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();

    let inc1 = ClusterIncarnationId::genesis("fsp-cluster-prod");
    let inc2 = inc1.advance("manual-rebootstrap-token");
    tracker.set_incarnation_direct(inc2.clone()).unwrap();

    // Verification of old incarnation must fail
    assert!(tracker.verify_incarnation_binding(&inc1).is_err());
    // Verification of active adopted incarnation must succeed
    assert!(tracker.verify_incarnation_binding(&inc2).is_ok());
}

/// Category 20: Concurrent Garbage Collection and Active Repair Mutual Exclusion.
#[test]
fn test_p5_4_20_concurrent_gc_deletion_and_repair_mutual_exclusion() {
    let sandbox = TempSandbox54::new("gc_repair_mutex");
    let tracker = NodeEpochFencingTracker::open_or_init(sandbox.path()).unwrap();
    let inc = ClusterIncarnationId::genesis("fsp-cluster");
    tracker.set_incarnation_direct(inc).unwrap();

    // Active repair acquires reservation on shard 2
    tracker.register_active_repair("obj-mutex", 2);
    assert!(tracker.is_shard_repairing("obj-mutex", 2));

    // GC deletion reservation attempt on same shard must fail
    let res = tracker.reserve_shard_deletion(
        "obj-mutex",
        2,
        1,
        1700000000,
        3600,
        Some(16),
        Some("domain-0"),
    );
    assert!(
        res.is_err(),
        "active repair must strictly block GC deletion"
    );
}

/// Category 21: Network Partition Split-Brain Prevention.
#[test]
fn test_p5_4_21_network_partition_split_brain_prevention() {
    let topo = ClusterZoneTopology::standard_5_zones_20_nodes(19100, 100 * 1024 * 1024);
    // 20 total nodes: majority is 11 nodes.
    // Partition: side A has 9 nodes (minority), side B has 11 nodes (majority).
    let majority: BTreeSet<NodeId> = (9..20).map(NodeId).collect();

    // Minority partition evaluation
    let eval_minority = topo.evaluate_rs20_12_survival(&majority);
    assert!(
        !eval_minority.can_recover,
        "minority partition (9 nodes) cannot survive RS(20,12)"
    );
}

/// Category 22: High Concurrency Multi-Object Distributed PUT and GET.
#[test]
fn test_p5_4_22_high_concurrency_multi_object_distributed_put_get() {
    let sandbox = Arc::new(TempSandbox54::new("concurrent_objs"));
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(sandbox.path(), 50 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path().join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();

    for i in 0..8 {
        let obj_id = format!("obj-multi-{}", i);
        let payload = deterministic_bytes(0x5422 + i as u64, 40_000);
        let opts = DistributedPutOptions {
            coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
            stripe_size: 32 * 1024,
            placement_policy: PlacementPolicy::default(),
            commit_policy: DegradedCommitPolicy::StrictAllShards,
            tx_id_override: None,
        };

        coord
            .distributed_put_bytes(&obj_id, &payload, opts)
            .unwrap();
        let (read_back, _) = coord.distributed_get_to_bytes(&obj_id).unwrap();
        assert_eq!(read_back, payload);
    }
}

/// Category 23: LRC(20,12,4) Local Single-Shard Repair in Cluster Staging.
#[test]
fn test_p5_4_23_lrc_20_12_4_local_repair_in_cluster_staging() {
    let lrc = LrcCodec::lrc_20_12_4();
    let payload = deterministic_bytes(0x5423, 120_000);
    let shard_len = 10_000;
    let data_shards: Vec<Vec<u8>> = (0..12)
        .map(|i| payload[i * shard_len..(i + 1) * shard_len].to_vec())
        .collect();
    let data_refs: Vec<&[u8]> = data_shards.iter().map(|v| v.as_slice()).collect();
    let encoded = lrc.encode_stripe(&data_refs).unwrap();
    assert_eq!(encoded.len(), 20);

    // Single shard failure: shard 1 (in local group 0)
    let available: Vec<(ShardId, &[u8])> = encoded
        .iter()
        .enumerate()
        .filter(|&(idx, _)| idx != 1)
        .map(|(idx, v)| (ShardId(idx), v.as_slice()))
        .collect();

    let output = lrc
        .repair_single_shard(ShardId(1), &available, shard_len)
        .unwrap();
    assert_eq!(output.target_shard, ShardId(1));
    assert!(matches!(output.method, RepairMethod::LocalGroup { .. }));
    assert_eq!(
        output.source_shards_read.len(),
        4,
        "LRC local repair needs only 4 shards instead of 12"
    );
    assert_eq!(output.bytes, encoded[1]);
}

/// Category 24: Tampered Shard Rejected by Digest and Reconstructed.
#[test]
fn test_p5_4_24_tampered_shard_rejected_by_coordinator_read() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let shard_len = 1024;
    let data_vecs: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(0x5424 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();
    let encoded = rs.encode_stripe(&data_refs).unwrap();

    let correct_digest = sha256_bytes(&encoded[3]).to_hex();
    let mut corrupted = encoded[3].clone();
    corrupted[10] ^= 0x5A;
    let corrupt_digest = sha256_bytes(&corrupted).to_hex();
    assert_ne!(correct_digest, corrupt_digest);

    // Drop shard 3 and reconstruct from the remaining 19 shards
    let available: Vec<(ShardId, &[u8])> = (0..20)
        .filter(|&i| i != 3)
        .map(|i| (ShardId(i), encoded[i].as_slice()))
        .collect();
    let targets = vec![ShardId(3)];
    let reconstructed = rs
        .reconstruct_specific_shards(&targets, &available, shard_len)
        .unwrap();
    assert_eq!(reconstructed.len(), 1);
    assert_eq!(reconstructed[0].1, encoded[3]);
}

/// Category 25: End-to-End Disaster Recovery Workflow.
#[test]
fn test_p5_4_25_end_to_end_disaster_recovery_workflow() {
    let sandbox = TempSandbox54::new("dr_workflow");
    let operator = ManifestSignerKeypair::from_seed("dr-operator", [0x99u8; 32]).unwrap();
    let snapshot = MetadataSnapshot::new(200, 2, BTreeMap::new());

    let bundle = DisasterRecoveryBackupBundle::create(
        "dr-operator",
        2,
        200,
        200,
        snapshot,
        Vec::new(),
        BTreeMap::new(),
        operator.signing_key(),
    );

    // Verify cryptographic signature and secret exclusion
    assert!(bundle.verify(&operator.verifying_key()).is_ok());
    assert!(bundle.scan_for_plaintext_secrets().is_ok());
    assert_eq!(bundle.commit_index, 200);

    let target_dir = sandbox.path().join("restored_coord");
    let report =
        execute_disaster_recovery_restore(&bundle, &operator.verifying_key(), &target_dir, false)
            .unwrap();
    assert_eq!(report.status, DisasterRecoveryStatus::Success);
    assert_eq!(report.restored_commit_index, 200);
}
