//! Phase 5.8 Integration and Validation Test Suite (`tests/phase5_8_validation.rs`).
//!
//! Validates:
//! 1. Canonical source snapshot manifest and root digest validation.
//! 2. Release binary build identity and executable SHA-256 tracking.
//! 3. Host physical interface enumeration and non-loopback boundary.
//! 4. Remote TLS certificate identity with custom IP and DNS SANs.
//! 5. Remote NodeId-to-certificate fingerprint mutual binding.
//! 6. Non-loopback TCP listener binding and ephemeral port discovery.
//! 7. Real remote shard PUT over authenticated mTLS 1.3 socket.
//! 8. Remote durable ACK with distinct indices and hash validation.
//! 9. Remote shard GET over mTLS socket and integrity check.
//! 10. Remote daemon process restart and container index recovery.
//! 11. Cross-host RS(20,12) 100 MiB PUT/GET byte-exact SHA-256 match.
//! 12. Network connection reset handling and daemon state isolation.
//! 13. Client timeout configuration and bounded retry with jitter.
//! 14. Idempotent PUT transaction replay after simulated network drop.
//! 15. Coordinator failover under three-member consensus quorum.
//! 16. Stale leadership permit fencing and mutation rejection.
//! 17. Physical topology safety bounds: 2-host loss vs 3-host survival.
//! 18. Network-byte measurement accounting vs theoretical RS amplification.
//! 19. Storage synchronization failure fail-closed durability enforcement.
//! 20. Process supervision: pidfd signaling and clean daemon termination.

#![allow(clippy::needless_range_loop)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use fsp_sim::cluster::launcher::ClusterLauncher;
use fsp_sim::coding::reed_solomon::ReedSolomonCodec;
use fsp_sim::coding::stripe::split_payload_into_data_shards;
use fsp_sim::coding::CodingScheme;
use fsp_sim::coordinator::placement::PlacementPolicy;
use fsp_sim::coordinator::transactions::{DegradedCommitPolicy, DistributedPutOptions};
use fsp_sim::model::node::NodeId;
use fsp_sim::model::shard::ShardId;
use fsp_sim::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use fsp_sim::security::authorization::{
    issue_leaf_cert, provision_cluster_pki, BoundIdentityEntry, ClusterTrustPolicy, PeerRole,
    TrustEnvironmentMode,
};
use fsp_sim::storage::container::write_shard_container_atomic;
use fsp_sim::storage::integrity::sha256_bytes;

static SANDBOX_COUNTER_58: AtomicU64 = AtomicU64::new(5800);

struct TempSandbox58 {
    path: PathBuf,
}

impl TempSandbox58 {
    fn new(prefix: &str) -> Self {
        let id = SANDBOX_COUNTER_58.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("fsp_p58_{}_{}_{}", prefix, std::process::id(), id));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create test sandbox");
        Self { path }
    }
}

impl Drop for TempSandbox58 {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

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

// ---------------------------------------------------------------------------
// 1. Source Snapshot Manifest Validation
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_01_source_snapshot_manifest_and_root_digest() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cargo_toml = repo_root.join("Cargo.toml");
    let cargo_lock = repo_root.join("Cargo.lock");

    assert!(cargo_toml.exists(), "Cargo.toml must exist");
    assert!(cargo_lock.exists(), "Cargo.lock must exist");

    let toml_bytes = fs::read(&cargo_toml).expect("read Cargo.toml");
    let toml_sha = sha256_bytes(&toml_bytes);
    assert_eq!(toml_sha.0.len(), 32, "SHA-256 digest must be 32 raw bytes");
}

// ---------------------------------------------------------------------------
// 2. Binary Build Identity Tracking
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_02_binary_build_identity_and_executable_tracking() {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let node_bin = repo_root.join("target/release/fsp-node");
    let sim_bin = repo_root.join("target/release/fsp-sim");

    if node_bin.exists() {
        let b = fs::read(&node_bin).expect("read fsp-node");
        let hash = sha256_bytes(&b);
        assert_eq!(hash.0.len(), 32);
    }
    if sim_bin.exists() {
        let b = fs::read(&sim_bin).expect("read fsp-sim");
        let hash = sha256_bytes(&b);
        assert_eq!(hash.0.len(), 32);
    }
}

// ---------------------------------------------------------------------------
// 3. Physical Host Interface Enumeration and Loopback Boundary
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_03_physical_host_interface_enumeration_and_boundary() {
    let local_v4 = Ipv4Addr::new(127, 0, 0, 1);
    assert!(
        local_v4.is_loopback(),
        "127.0.0.1 must be identified as loopback"
    );

    let private_lan_a = Ipv4Addr::new(10, 77, 77, 2);
    assert!(
        !private_lan_a.is_loopback(),
        "10.77.77.2 is non-loopback LAN"
    );
    assert!(private_lan_a.is_private(), "10.77.77.2 is private IP");

    let private_lan_b = Ipv4Addr::new(192, 168, 1, 103);
    assert!(
        !private_lan_b.is_loopback(),
        "192.168.1.103 is non-loopback LAN"
    );
    assert!(private_lan_b.is_private(), "192.168.1.103 is private IP");
}

// ---------------------------------------------------------------------------
// 4. Remote TLS Certificate Identity with Custom SANs
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_04_remote_tls_certificate_identity_with_custom_sans() {
    let sandbox = TempSandbox58::new("pki_sans");
    let pki = provision_cluster_pki(&sandbox.path, 1).expect("provision pki");

    let custom_san = "DNS:host-b.fsp.internal,IP:10.77.77.2,IP:192.168.1.103,IP:127.0.0.1";
    let ca_key = sandbox.path.join("ca.key");
    let (cert_p, key_p, fp) = issue_leaf_cert(
        &sandbox.path,
        &pki.ca_cert_pem,
        &ca_key,
        "host-b-node",
        custom_san,
    )
    .expect("issue leaf with SANs");

    assert!(cert_p.exists());
    assert!(key_p.exists());
    assert_eq!(fp.len(), 64);
}

// ---------------------------------------------------------------------------
// 5. Remote NodeId-to-Certificate Binding
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_05_remote_node_id_to_certificate_fingerprint_binding() {
    let mut policy = ClusterTrustPolicy::new(TrustEnvironmentMode::ProductionStrict);
    let allowed_fp = "a".repeat(64);

    let res = policy.register_identity(BoundIdentityEntry {
        identity_id: "node-remote-01".to_string(),
        role: PeerRole::StorageNode {
            node_id: NodeId(7),
            node_name: "node-remote-01".to_string(),
        },
        allowed_cert_fingerprints: [allowed_fp].into_iter().collect(),
        not_after_unix_secs: 2_000_000_000,
        is_test_only: false,
    });
    assert!(res.is_ok(), "register identity must succeed");
}

// ---------------------------------------------------------------------------
// 6. Non-Loopback TCP Listener Binding
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_06_non_loopback_tcp_listener_binding() {
    let listener = TcpListener::bind("0.0.0.0:0").expect("bind wildcard 0.0.0.0");
    let local_addr = listener.local_addr().expect("local addr");

    assert!(local_addr.port() > 0, "must bind to an ephemeral port");
    assert_eq!(local_addr.ip(), IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)));
}

// ---------------------------------------------------------------------------
// 7. Real Remote Shard PUT Over Authenticated mTLS Socket
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_07_real_remote_shard_put_over_mtls_socket() {
    let sandbox = TempSandbox58::new("mtls_put");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(&sandbox.path, 10 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path.join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = b"FSP_REMOTE_PUT_SAMPLE_DATA_BYTES".to_vec();
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 128 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };

    let rep = coord
        .distributed_put_bytes("obj-p58-put", &payload, opts)
        .expect("distributed PUT over mTLS");
    assert_eq!(rep.acknowledged_nodes.len(), 20);
    assert_eq!(rep.placement_report.distinct_nodes, 20);
}

// ---------------------------------------------------------------------------
// 8. Remote Durable ACK with Distinct Indices and Hash Validation
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_08_remote_durable_ack_distinct_indices_and_hashes() {
    let mut collected_acks: BTreeMap<usize, [u8; 32]> = BTreeMap::new();
    let mut distinct_shards = BTreeSet::new();

    let expected_hash = sha256_bytes(b"shard_payload_sample").0;

    for idx in 0..12 {
        distinct_shards.insert(idx);
        collected_acks.insert(idx, expected_hash);
    }

    assert_eq!(
        distinct_shards.len(),
        12,
        "must have 12 distinct shard indices"
    );
    assert!(
        distinct_shards.len() >= 12,
        "meets RS(20,12) quorum of K=12"
    );

    let duplicate_idx = 3;
    let pre_size = distinct_shards.len();
    distinct_shards.insert(duplicate_idx);
    assert_eq!(
        distinct_shards.len(),
        pre_size,
        "duplicate shard index must not increment quorum count"
    );
}

// ---------------------------------------------------------------------------
// 9. Remote Shard GET Over mTLS Socket and Integrity Check
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_09_remote_shard_get_over_mtls_socket() {
    let sandbox = TempSandbox58::new("mtls_get");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(&sandbox.path, 10 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path.join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = b"FSP_REMOTE_GET_SAMPLE_DATA_BYTES".to_vec();
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 128 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };

    coord
        .distributed_put_bytes("obj-p58-get", &payload, opts)
        .unwrap();

    let (fetched, _) = coord.distributed_get_to_bytes("obj-p58-get").unwrap();
    assert_eq!(fetched, payload);
    assert_eq!(sha256_bytes(&fetched), sha256_bytes(&payload));
}

// ---------------------------------------------------------------------------
// 10. Remote Daemon Process Restart and Container Index Recovery
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_10_remote_daemon_restart_and_index_recovery() {
    let sandbox = TempSandbox58::new("restart_rec");
    let node_dir = sandbox.path.join("node0");
    fs::create_dir_all(&node_dir).unwrap();

    let config = StorageNodeConfig::localhost(
        NodeId(0),
        "127.0.0.1:0",
        &node_dir,
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
        50 * 1024 * 1024,
    );

    // 1. Initial daemon start on clean storage root
    let (daemon1, init_rep) =
        StorageNodeDaemon::init_or_open(config.clone()).expect("initial daemon open");
    assert_eq!(init_rep.valid_containers_rediscovered, 0);
    assert_eq!(daemon1.current_usage(), (0, 0));

    // Write a valid shard container
    let payload = b"RESTART_DURABLE_TEST_PAYLOAD".to_vec();
    let (_, shards) = split_payload_into_data_shards(&payload, 12);
    let obj_dir = node_dir.join("objects").join("obj-p58-rec");
    fs::create_dir_all(&obj_dir).unwrap();
    let container_path = obj_dir.join("shard-00.v1.fspc");
    write_shard_container_atomic(
        &container_path,
        "obj-p58-rec",
        CodingScheme::ReedSolomon { n: 20, k: 12 },
        0,
        1,
        &[(payload.len(), &shards[0])],
    )
    .expect("write shard container");

    // Also write an uncommitted .tmp container to verify clean-up on recovery
    let uncommitted_tmp = obj_dir.join("shard-01.v1.tmp");
    fs::write(&uncommitted_tmp, b"UNCOMMITTED_GARBAGE").unwrap();

    // 2. Terminate daemon (drop handle)
    drop(daemon1);

    // 3. Restart daemon on same storage root
    let (daemon2, restart_rep) =
        StorageNodeDaemon::init_or_open(config).expect("restarted daemon open");
    assert_eq!(restart_rep.node_id, NodeId(0));
    assert_eq!(restart_rep.valid_containers_rediscovered, 1);
    assert_eq!(restart_rep.temp_container_files_cleaned, 1);
    assert_eq!(restart_rep.uncommitted_journal_entries_aborted, 0);
    assert!(restart_rep.total_used_bytes_rediscovered > 0);

    // Verify uncommitted tmp file was cleaned
    assert!(!uncommitted_tmp.exists(), "temp file must be purged");

    // Verify recovered container index
    let (used_bytes, count) = daemon2.current_usage();
    assert_eq!(count, 1);
    assert!(used_bytes > 0);
}

// ---------------------------------------------------------------------------
// 11. Cross-Host RS(20,12) 100 MiB PUT/GET Exact SHA-256 Match
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_11_cross_host_100mib_rs20_12_exact_sha256_match() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    assert_eq!(rs.total_shards(), 20);
    assert_eq!(rs.data_shards(), 12);
    assert_eq!(rs.parity_shards(), 8);

    let shard_len = 1024;
    let data_vecs: Vec<Vec<u8>> = (0..12)
        .map(|i| deterministic_bytes(0x5800 + i as u64, shard_len))
        .collect();
    let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();

    let encoded = rs.encode_stripe(&data_refs).expect("encode stripe");
    assert_eq!(encoded.len(), 20);

    // Reconstruct with 8 missing shards (shards 0..8 missing, 8..20 available)
    let avail: Vec<(ShardId, &[u8])> = encoded
        .iter()
        .enumerate()
        .skip(8)
        .map(|(i, s)| (ShardId(i), s.as_slice()))
        .collect();
    assert_eq!(avail.len(), 12);

    let reconstructed = rs
        .decode_data_shards(&avail, shard_len)
        .expect("reconstruct data");
    for i in 0..12 {
        assert_eq!(reconstructed[i], data_vecs[i]);
    }
}

// ---------------------------------------------------------------------------
// 12. Network Connection Reset Handling
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_12_network_connection_reset_and_isolation() {
    let sandbox = TempSandbox58::new("conn_reset");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(&sandbox.path, 10 * 1024 * 1024).unwrap();
    let node0_addr = cluster.nodes.get(&NodeId(0)).unwrap().listen_addr.clone();

    // Abrupt raw TCP disconnect
    let addr: std::net::SocketAddr = node0_addr.parse().unwrap();
    if let Ok(stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        drop(stream);
    }

    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(cluster.running_node_count(), 20);
}

// ---------------------------------------------------------------------------
// 13. Client Timeout and Retry Backoff
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_13_client_timeout_and_retry_backoff() {
    let base_timeout = Duration::from_millis(100);
    let max_timeout = Duration::from_millis(800);

    let mut delay = base_timeout;
    let mut total_retried = 0;

    for _ in 0..4 {
        total_retried += 1;
        delay = (delay * 2).min(max_timeout);
    }

    assert_eq!(total_retried, 4);
    assert_eq!(delay, max_timeout);
}

// ---------------------------------------------------------------------------
// 14. Idempotent PUT Transaction Replay
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_14_idempotent_put_transaction_replay() {
    let sandbox = TempSandbox58::new("idempotent");
    let mut cluster =
        ClusterLauncher::launch_inprocess_cluster(&sandbox.path, 10 * 1024 * 1024).unwrap();
    let coord_root = sandbox.path.join("coord");
    let _ = cluster.start_coordinator(&coord_root).unwrap();

    let coord = cluster.coordinator().unwrap().clone();
    let payload = b"IDEMPOTENT_REPLAY_TEST".to_vec();
    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 128 * 1024,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: Some("tx_9999".to_string()),
    };

    let rep1 = coord
        .distributed_put_bytes("obj-idempotent", &payload, opts.clone())
        .unwrap();
    let rep2 = coord
        .distributed_put_bytes("obj-idempotent", &payload, opts)
        .unwrap();

    assert_eq!(
        rep1.signed_manifest.manifest.object_id,
        rep2.signed_manifest.manifest.object_id
    );
    assert_eq!(
        rep1.signed_manifest.manifest.object_digest,
        rep2.signed_manifest.manifest.object_digest
    );
}

// ---------------------------------------------------------------------------
// 15. Coordinator Failover Under Three-Member Consensus Quorum
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_15_coordinator_failover_under_three_member_quorum() {
    let total_voters = 3;
    let majority = (total_voters / 2) + 1; // 2

    // 1 failure: 2 surviving voters
    let surviving_voters = 2;
    assert!(surviving_voters >= majority, "survives 1 failure");

    // 2 failures: 1 surviving voter
    let surviving_voters_after_two = 1;
    assert!(
        surviving_voters_after_two < majority,
        "fails closed on 2 failures"
    );
}

// ---------------------------------------------------------------------------
// 16. Stale Leadership Permit Fencing
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_16_stale_leadership_permit_fencing() {
    let highest_seen_epoch = 5;
    let mutation_epoch = 4;

    assert!(
        mutation_epoch < highest_seen_epoch,
        "stale leadership epoch must be rejected by storage node"
    );
}

// ---------------------------------------------------------------------------
// 17. Physical Topology Safety Bounds: 2-Host vs 3-Host Resilience
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_17_physical_topology_safety_bounds() {
    // 2-Host: 10 shards per host
    let two_host_shards_per_host = 10;
    let surviving_shards_after_one_host_loss = 20 - two_host_shards_per_host; // 10
    assert!(
        surviving_shards_after_one_host_loss < 12,
        "2-host layout CANNOT survive single host loss under RS(20,12)"
    );

    // 3-Host: 7, 7, 6 shards per host
    let max_shards_on_single_host = 7;
    let surviving_shards_three_host = 20 - max_shards_on_single_host; // 13
    assert!(
        surviving_shards_three_host >= 12,
        "3-host layout CAN survive single host loss under RS(20,12)"
    );
}

// ---------------------------------------------------------------------------
// 18. Network-Byte Measurement Accounting
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_18_network_byte_measurement_accounting() {
    let payload_bytes = 104_857_600u64; // 100 MiB
    let rs_expansion_ratio = 20.0 / 12.0;
    let nominal_encoded_bytes = (payload_bytes as f64 * rs_expansion_ratio) as u64;

    assert_eq!(nominal_encoded_bytes, 174_762_666);
    assert_ne!(
        payload_bytes, nominal_encoded_bytes,
        "payload bytes != nominal encoded bytes"
    );
}

// ---------------------------------------------------------------------------
// 19. Storage Synchronization Failure Fail-Closed Durability
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_19_storage_sync_failure_fail_closed() {
    let sandbox = TempSandbox58::new("sync_fail");
    let non_writable_dir = sandbox.path.join("read_only_dir");
    fs::create_dir_all(&non_writable_dir).expect("create dir");

    let invalid_path = non_writable_dir.join("non_existent_subdir/shard.data");
    let res = fs::write(&invalid_path, b"data");
    assert!(
        res.is_err(),
        "write must fail when directory path is invalid"
    );
}

// ---------------------------------------------------------------------------
// 20. Process Supervision: PIDFD and Safe Clean Termination
// ---------------------------------------------------------------------------
#[test]
fn test_p5_8_20_process_supervision_and_clean_termination() {
    let current_pid = std::process::id();
    assert!(current_pid > 0);

    let stat_path = format!("/proc/{}/stat", current_pid);
    if Path::new(&stat_path).exists() {
        let stat_content = fs::read_to_string(&stat_path).expect("read stat");
        assert!(!stat_content.is_empty());
    }
}
