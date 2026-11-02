//! Phase 4 Integration, Distributed Cluster, and Security Validation Suite (`tests/phase4_validation.rs`).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use sha2::{Digest, Sha256};

use fsp_sim::coding::codec::ErasureCodec;
use fsp_sim::coding::reed_solomon::ReedSolomonCodec;
use fsp_sim::coding::{CodingScheme, LrcConfig};
use fsp_sim::coordinator::placement::PlacementPolicy;
use fsp_sim::coordinator::scheduler::{BackgroundScrubber, DistributedRepairScheduler};
use fsp_sim::coordinator::transactions::{
    DegradedCommitPolicy, DistributedCoordinator, DistributedPutOptions, RegisteredStorageNode,
};
use fsp_sim::model::node::NodeId;
use fsp_sim::model::shard::ShardId;
use fsp_sim::network::client::NodeRpcClient;
use fsp_sim::network::protocol::{PutShardRequest, RpcError, RpcRequest};
use fsp_sim::network::server::NodeRpcServer;
use fsp_sim::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use fsp_sim::node::identity::NodeFailureDomainTopology;
use fsp_sim::security::authorization::{
    build_mtls_client_config, build_mtls_server_config, cert_der_sha256_hex, issue_leaf_cert,
    load_certs_from_pem, provision_cluster_pki, ProvisionedClusterPki,
};
use fsp_sim::security::signatures::{ManifestSignerKeypair, SignatureError, TrustedSignerPolicy};
use fsp_sim::storage::backend::StorageError;
use fsp_sim::storage::container::{
    ContainerError, ShardContainerReader, ShardContainerWriter, CONTAINER_HEADER_SIZE,
};
use fsp_sim::storage::engine::StorageEngine;
use fsp_sim::storage::integrity::sha256_bytes;
use fsp_sim::storage::object::PutOptions;
use fsp_sim::strategies::adaptive_v2::AdaptiveV2Config;

static SANDBOX_COUNTER: AtomicU64 = AtomicU64::new(1);

struct TempSandbox {
    root: PathBuf,
}

impl TempSandbox {
    fn new(prefix: &str) -> Self {
        let id = SANDBOX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let root = std::env::temp_dir().join(format!("fsp_p4_{}_{}_{}", prefix, pid, id));
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

impl Drop for TempSandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let mut s = seed;
    for chunk in out.chunks_mut(8) {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let bytes = (z ^ (z >> 31)).to_le_bytes();
        let n = chunk.len();
        chunk.copy_from_slice(&bytes[..n]);
    }
    out
}

/// Helper managing an in-process multi-node mTLS cluster for deterministic distributed tests.
struct LocalMtlsCluster {
    _sandbox: TempSandbox,
    pki: ProvisionedClusterPki,
    servers: BTreeMap<NodeId, NodeRpcServer>,
    coordinator: Arc<DistributedCoordinator>,
}

impl LocalMtlsCluster {
    fn start(num_nodes: usize, node_capacity_bytes: u64) -> Self {
        let sandbox = TempSandbox::new("mtls_cluster");
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

#[test]
fn test_p4_01_exhaustive_rs_20_12_submatrix_rank_and_lrc_rank_audit() {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let gm = rs.generator_matrix();

    // Exhaustively verify all C(20, 12) = 125,970 12x12 submatrices of RS(20,12) are invertible (rank == 12)
    let mut indices: Vec<usize> = (0..12).collect();
    let mut total_checked = 0usize;
    loop {
        let sub = gm.submatrix_rows(&indices);
        assert_eq!(
            sub.rank(),
            12,
            "RS(20,12) submatrix {:?} must have full rank 12",
            indices
        );
        total_checked += 1;

        let mut i = 11isize;
        while i >= 0 && indices[i as usize] == (i as usize) + 8 {
            i -= 1;
        }
        if i < 0 {
            break;
        }
        indices[i as usize] += 1;
        for j in (i as usize + 1)..12 {
            indices[j] = indices[j - 1] + 1;
        }
    }
    assert_eq!(total_checked, 125_970);

    // Verify ErasureCodec::can_decode on LRC(20,12,4) accurately detects the singular 6-erasure pattern
    // over GF(2^8), as well as combinatorial unrecoverability when all 5 shards of local group 0 are erased
    let lrc_scheme = CodingScheme::Lrc(LrcConfig::new(20, 12, 4).unwrap());
    let lrc_codec = ErasureCodec::from_scheme(lrc_scheme).unwrap();

    // Find the exact 1 singular 6-erasure pattern out of all C(20, 14) = 38,760 6-erasure patterns
    let mut comb: Vec<usize> = (0..14).collect();
    let mut full_rank_6_erasures = 0usize;
    let mut singular_6_erasures = 0usize;
    loop {
        let surv: BTreeSet<ShardId> = comb.iter().copied().map(ShardId).collect();
        if lrc_codec.can_decode(&surv) {
            full_rank_6_erasures += 1;
        } else {
            singular_6_erasures += 1;
        }
        let mut idx = 13isize;
        while idx >= 0 && comb[idx as usize] == (idx as usize) + 6 {
            idx -= 1;
        }
        if idx < 0 {
            break;
        }
        comb[idx as usize] += 1;
        for j in (idx as usize + 1)..14 {
            comb[j] = comb[j - 1] + 1;
        }
    }
    assert_eq!(full_rank_6_erasures, 38_759);
    assert_eq!(singular_6_erasures, 1);
}

#[test]
fn test_p4_02_differential_sha256_integrity_vs_rustcrypto_sha2() {
    let lengths = [
        0usize, 1, 3, 55, 56, 57, 63, 64, 65, 127, 128, 129, 4096, 65537, 262_144,
    ];
    for (idx, &len) in lengths.iter().enumerate() {
        let payload = deterministic_bytes(0xA5A5_0000 + idx as u64, len);
        let our_hex = sha256_bytes(&payload).to_hex();

        let mut ref_hasher = Sha256::new();
        ref_hasher.update(&payload);
        let ref_bytes = ref_hasher.finalize();
        let mut ref_hex = String::with_capacity(64);
        for b in ref_bytes {
            ref_hex.push_str(&format!("{:02x}", b));
        }

        assert_eq!(
            our_hex, ref_hex,
            "SHA-256 mismatch against RustCrypto sha2 at length {}",
            len
        );
    }
}

#[test]
fn test_p4_03_container_v2_roundtrip_random_access_and_corruption_rejection() {
    let sandbox = TempSandbox::new("container_v2");
    let path = sandbox.path().join("shard-03.v1.fspc");
    let scheme = CodingScheme::ReedSolomon { n: 20, k: 12 };

    let s0 = deterministic_bytes(1, 512);
    let s1 = deterministic_bytes(2, 512);
    let s2 = deterministic_bytes(3, 256);

    let mut writer =
        ShardContainerWriter::create(&path, "obj-v2-test", scheme, 3, 1).expect("create writer");
    writer.append_stripe(0, 6144, &s0).unwrap();
    writer.append_stripe(1, 6144, &s1).unwrap();
    writer.append_stripe(2, 3072, &s2).unwrap();
    let (_, hdr, entries) = writer.finalize().expect("finalize container");

    assert_eq!(hdr.stripe_count, 3);
    assert_eq!(entries.len(), 3);

    // Random-access read of stripe 2 directly without reading stripes 0 or 1
    let mut reader = ShardContainerReader::open(&path).expect("open container");
    assert_eq!(reader.read_stripe(2).unwrap(), s2);
    assert_eq!(reader.read_stripe(0).unwrap(), s0);
    assert_eq!(reader.read_stripe(1).unwrap(), s1);
    reader.verify_all().expect("verify_all");

    // 1. Corrupt a byte inside stripe 1 body -> read_stripe(1) and verify_all() must fail with StripeIntegrityMismatch
    let mut raw = fs::read(&path).unwrap();
    let corrupt_pos = (CONTAINER_HEADER_SIZE + 512 + 10) as usize;
    raw[corrupt_pos] ^= 0x40;
    let corrupt_body_path = sandbox.path().join("corrupt_body.fspc");
    fs::write(&corrupt_body_path, &raw).unwrap();
    let mut r_corrupt = ShardContainerReader::open(&corrupt_body_path).unwrap();
    assert_eq!(r_corrupt.read_stripe(0).unwrap(), s0);
    assert!(matches!(
        r_corrupt.read_stripe(1),
        Err(ContainerError::StripeIntegrityMismatch {
            stripe_index: 1,
            ..
        })
    ));

    // 2. Corrupt header byte -> ShardContainerReader::open must reject with HeaderIndexChecksumMismatch
    let mut raw_hdr = fs::read(&path).unwrap();
    raw_hdr[24] ^= 0x02; // flip version bit (1 -> 3) in header
    let corrupt_hdr_path = sandbox.path().join("corrupt_hdr.fspc");
    fs::write(&corrupt_hdr_path, &raw_hdr).unwrap();
    assert!(matches!(
        ShardContainerReader::open(&corrupt_hdr_path),
        Err(ContainerError::HeaderIndexChecksumMismatch { .. })
    ));

    // 3. Truncate file by 8 bytes -> ShardContainerReader::open must reject with InvalidOffsetLayout
    let trunc_path = sandbox.path().join("trunc.fspc");
    fs::write(&trunc_path, &raw[..raw.len() - 8]).unwrap();
    assert!(matches!(
        ShardContainerReader::open(&trunc_path),
        Err(ContainerError::InvalidOffsetLayout(_))
    ));

    // 4. Truncate file below 256-byte header -> ShardContainerReader::open must reject with TruncatedContainer
    let short_hdr_path = sandbox.path().join("short_hdr.fspc");
    fs::write(&short_hdr_path, &raw[..100]).unwrap();
    assert!(matches!(
        ShardContainerReader::open(&short_hdr_path),
        Err(ContainerError::TruncatedContainer { .. })
    ));
}

#[test]
fn test_p4_04_container_v1_backward_compatibility_and_v1_to_v2_migration() {
    let sandbox = TempSandbox::new("v1_to_v2_migration");
    let mut engine = StorageEngine::init(sandbox.path(), 20, 20).unwrap();

    let payload = deterministic_bytes(0x7777, 180_000);
    let v1_opts = PutOptions {
        object_id: Some("legacy-v1-obj".to_string()),
        stripe_size: 65_536, // 3 stripes -> 60 v1 .fsp files
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        crash_trigger: None,
        use_container_v2: false,
    };

    let receipt = engine.put_bytes(&payload, &v1_opts).unwrap();
    assert_eq!(receipt.stripe_count, 3);
    assert!(receipt.manifest.stripes[0].placements[0]
        .relative_path
        .ends_with(".fsp"));

    // Verify v1 read works
    let (read_v1, _) = engine.get_to_bytes("legacy-v1-obj").unwrap();
    assert_eq!(read_v1, payload);

    // Migrate v1 -> v2 container format
    let migrated_receipt = engine
        .migrate_object_v1_to_v2("legacy-v1-obj")
        .expect("migrate v1 to v2");
    for stripe in &migrated_receipt.manifest.stripes {
        for p in &stripe.placements {
            assert!(
                p.relative_path.ends_with(".fspc"),
                "migrated placement must use .fspc container"
            );
        }
    }

    // Verify only 1 .fspc container file exists per node directory (20 total instead of 60 .fsp files)
    let mut fspc_count = 0usize;
    let mut fsp_count = 0usize;
    for node_idx in 0..20 {
        let obj_dir = sandbox
            .path()
            .join(format!("node-{:02}/objects/legacy-v1-obj", node_idx));
        for entry in fs::read_dir(&obj_dir).unwrap() {
            let fname = entry.unwrap().file_name().into_string().unwrap();
            if fname.ends_with(".fspc") {
                fspc_count += 1;
            } else if fname.ends_with(".fsp") {
                fsp_count += 1;
            }
        }
    }
    assert_eq!(fspc_count, 20);
    assert_eq!(fsp_count, 0);

    // Verify degraded read after deleting 8 v2 containers (nodes 0..8)
    for node_idx in 0..8 {
        let rel = &migrated_receipt.manifest.stripes[0].placements[node_idx].relative_path;
        engine.backend().inject_delete_shard(rel).unwrap();
    }
    let (read_v2_degraded, _) = engine.get_to_bytes("legacy-v1-obj").unwrap();
    assert_eq!(read_v2_degraded, payload);
}

#[test]
fn test_p4_05_ed25519_signed_manifest_security_and_replay_protection() {
    let cluster = LocalMtlsCluster::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x55AA, 64_000);
    let put_rep = cluster
        .coordinator
        .distributed_put_bytes("signed-doc-01", &payload, DistributedPutOptions::default())
        .expect("distributed put");

    let valid_signed = put_rep.signed_manifest.clone();
    assert_eq!(valid_signed.manifest_generation, 1);

    // 1. Tamper with object_size inside the signed manifest -> rejected
    let mut tampered_manifest = valid_signed.clone();
    tampered_manifest.manifest.object_size += 1;
    {
        let pol = cluster
            .coordinator
            .signer_policy()
            .read()
            .unwrap_or_else(|p| p.into_inner());
        assert!(pol.verify_manifest(&tampered_manifest).is_err());
    }

    // 2. Tamper with shard placement node_id and recompute manifest_digest without re-signing -> rejected by Ed25519
    let mut tampered_placement = valid_signed.clone();
    tampered_placement.manifest.stripes[0].placements[0].node_id = 99;
    tampered_placement.manifest.seal();
    {
        let pol = cluster
            .coordinator
            .signer_policy()
            .read()
            .unwrap_or_else(|p| p.into_inner());
        assert!(matches!(
            pol.verify_manifest(&tampered_placement),
            Err(SignatureError::InvalidSignature(_))
        ));
    }

    // 3. Unknown signer_id -> rejected
    let rogue_signer = ManifestSignerKeypair::from_seed("rogue-coordinator", [0x99u8; 32]).unwrap();
    let rogue_signed = rogue_signer
        .sign_manifest(valid_signed.manifest.clone(), 2)
        .unwrap();
    {
        let pol = cluster
            .coordinator
            .signer_policy()
            .read()
            .unwrap_or_else(|p| p.into_inner());
        assert!(matches!(
            pol.verify_manifest(&rogue_signed),
            Err(SignatureError::UnknownSigner(_))
        ));
    }

    // 4. Wrong public key claimed for known signer_id -> rejected
    let mut wrong_pk = valid_signed.clone();
    wrong_pk.public_key_hex = rogue_signer.public_key_hex();
    {
        let pol = cluster
            .coordinator
            .signer_policy()
            .read()
            .unwrap_or_else(|p| p.into_inner());
        assert!(matches!(
            pol.verify_manifest(&wrong_pk),
            Err(SignatureError::PublicKeyMismatch { .. })
        ));
    }

    // 5. Advance to generation 2, then attempt to replay generation 1 -> rejected with ReplayGenerationRejected!
    let gen2_signed = cluster
        .coordinator
        .signer_keypair()
        .sign_manifest(valid_signed.manifest.clone(), 2)
        .unwrap();
    {
        let mut pol = cluster
            .coordinator
            .signer_policy()
            .write()
            .unwrap_or_else(|p| p.into_inner());
        pol.verify_and_advance_generation(&gen2_signed, true)
            .expect("accept gen 2");
        assert!(matches!(
            pol.verify_manifest(&valid_signed),
            Err(SignatureError::ReplayGenerationRejected {
                presented_generation: 1,
                recorded_generation: 2,
                ..
            })
        ));
    }

    // 6. Export and restore signed MetadataBackupBundle
    cluster
        .coordinator
        .save_signed_manifest_atomic(&gen2_signed)
        .unwrap();
    let bundle = cluster.coordinator.export_metadata_backup().unwrap();
    assert_eq!(bundle.signed_manifests.len(), 1);
    let restored_count = cluster
        .coordinator
        .restore_metadata_backup(&bundle)
        .unwrap();
    assert_eq!(restored_count, 1);
}

#[test]
fn test_p4_06_mtls_authentication_rogue_cert_expired_cert_identity_binding_and_rbac() {
    let cluster = LocalMtlsCluster::start(4, 10 * 1024 * 1024);
    let node0_addr = cluster
        .servers
        .get(&NodeId(0))
        .unwrap()
        .bound_addr()
        .to_string();

    // 1. Valid Coordinator mTLS client succeeds
    let status = cluster
        .coordinator
        .rpc_client()
        .get_node_status(NodeId(0), &node0_addr)
        .expect("valid coordinator mTLS");
    assert_eq!(status.node_id, NodeId(0));

    // 2. Rogue client certificate (signed by untrusted Rogue CA) is rejected during TLS 1.3 handshake
    let rogue_tls = build_mtls_client_config(
        &cluster.pki.ca_cert_pem,
        &cluster.pki.rogue_cert_pem,
        &cluster.pki.rogue_key_pem,
    )
    .unwrap();
    let rogue_client = NodeRpcClient::new(
        rogue_tls,
        cluster.pki.trust_policy.clone(),
        Duration::from_secs(3),
    );
    let rogue_res = rogue_client.get_node_status(NodeId(0), &node0_addr);
    assert!(
        rogue_res.is_err(),
        "rogue CA client certificate must be rejected"
    );

    // 3. Node Identity Mismatch: connecting to node-00's address while expecting NodeId(1) must be rejected!
    let mismatch_err = cluster
        .coordinator
        .rpc_client()
        .get_node_status(NodeId(1), &node0_addr)
        .unwrap_err();
    assert!(matches!(mismatch_err, RpcError::Unauthorized(_)));

    // 4. Role-Based Authorization (RBAC): ReadOnlyObserver can call Heartbeat/GetNodeStatus,
    //    but is rejected when attempting PutShard or DeleteShard!
    let obs_tls = build_mtls_client_config(
        &cluster.pki.ca_cert_pem,
        &cluster.pki.observer_cert_pem,
        &cluster.pki.observer_key_pem,
    )
    .unwrap();
    let obs_client = NodeRpcClient::new(
        obs_tls,
        cluster.pki.trust_policy.clone(),
        Duration::from_secs(3),
    );
    assert!(obs_client.heartbeat(NodeId(0), &node0_addr, 1, 100).is_ok());
    let unauth_del = obs_client.delete_shard(NodeId(0), &node0_addr, "tx-unauth", "obj-1", 0, 1);
    assert!(matches!(unauth_del, Err(RpcError::Unauthorized(_))));

    // 5. Expired Certificate rejection when clock advances past `not_after_unix_secs`
    let server0 = cluster.servers.get(&NodeId(0)).unwrap();
    server0.set_simulated_clock_unix_secs(5_000_000_000); // > 4_102_444_800
    let exp_err = cluster
        .coordinator
        .rpc_client()
        .get_node_status(NodeId(0), &node0_addr)
        .unwrap_err();
    assert!(matches!(exp_err, RpcError::Unauthorized(_)));
    server0.set_simulated_clock_unix_secs(0); // restore real clock

    // 6. Live Certificate Rotation on coordinator
    let ca_key = cluster.pki.pki_dir.join("ca.key");
    let (new_coord_cert, new_coord_key, _) = issue_leaf_cert(
        &cluster.pki.pki_dir,
        &cluster.pki.ca_cert_pem,
        &ca_key,
        "coordinator-rotated",
        "DNS:localhost,IP:127.0.0.1",
    )
    .unwrap();
    let new_cert_der = load_certs_from_pem(&new_coord_cert).unwrap();
    server0
        .trust_policy()
        .write()
        .unwrap()
        .rotate_certificate(
            "coordinator-primary",
            new_cert_der[0].as_ref(),
            4_200_000_000,
            true,
        )
        .unwrap();

    // Old coordinator cert is now revoked on node-00
    assert!(matches!(
        cluster
            .coordinator
            .rpc_client()
            .get_node_status(NodeId(0), &node0_addr),
        Err(RpcError::Unauthorized(_))
    ));

    // Hot-reload rotated coordinator cert -> succeeds!
    let rotated_client_tls =
        build_mtls_client_config(&cluster.pki.ca_cert_pem, &new_coord_cert, &new_coord_key)
            .unwrap();
    cluster
        .coordinator
        .rpc_client()
        .reload_tls_config(rotated_client_tls);
    assert!(cluster
        .coordinator
        .rpc_client()
        .get_node_status(NodeId(0), &node0_addr)
        .is_ok());
}

#[test]
fn test_p4_07_rpc_wire_protocol_version_mismatch_and_oversized_frame_rejection() {
    let cluster = LocalMtlsCluster::start(2, 10 * 1024 * 1024);
    let node0_addr = cluster
        .servers
        .get(&NodeId(0))
        .unwrap()
        .bound_addr()
        .to_string();

    // Protocol version 99 must be rejected by server with ProtocolVersionMismatch
    let meta = RpcRequest::GetNodeStatus.encode_meta();
    let err = cluster
        .coordinator
        .rpc_client()
        .send_raw_frame_with_version(NodeId(0), &node0_addr, 99, &meta, &[])
        .unwrap_err();
    assert!(matches!(err, RpcError::ProtocolVersionMismatch { .. }));
}

#[test]
fn test_p4_08_real_20_node_distributed_put_get_and_eight_node_failure_recovery() {
    let mut cluster = LocalMtlsCluster::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0xCAFE_BABE, 250_000);

    let opts = DistributedPutOptions {
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripe_size: 65_536,
        placement_policy: PlacementPolicy::default(),
        commit_policy: DegradedCommitPolicy::StrictAllShards,
        tx_id_override: None,
    };

    let put_rep = cluster
        .coordinator
        .distributed_put_bytes("dist-rs-obj", &payload, opts)
        .expect("20-node distributed PUT");
    assert_eq!(put_rep.acknowledged_nodes.len(), 20);
    assert_eq!(put_rep.placement_report.distinct_nodes, 20);
    assert_eq!(put_rep.placement_report.distinct_racks, 4);
    assert_eq!(put_rep.placement_report.max_shards_on_any_rack, 5);

    // Normal distributed GET
    let (reconstructed, get_rep) = cluster
        .coordinator
        .distributed_get_to_bytes("dist-rs-obj")
        .expect("normal distributed GET");
    assert_eq!(reconstructed, payload);
    assert!(get_rep.nodes_failed_or_corrupt.is_empty());

    // Shut down 8 storage nodes (nodes 0..8) -> 12 nodes remain (= k)
    for i in 0..8 {
        if let Some(mut srv) = cluster.servers.remove(&NodeId(i)) {
            srv.shutdown();
        }
    }

    let out_file = cluster
        ._sandbox
        .path()
        .join("recovered_after_8_failures.bin");
    let deg_rep = cluster
        .coordinator
        .distributed_get_to_file("dist-rs-obj", &out_file)
        .expect("degraded GET with 8 nodes offline must succeed");
    assert_eq!(deg_rep.nodes_failed_or_corrupt.len(), 8);
    assert_eq!(fs::read(&out_file).unwrap(), payload);

    // Shut down a 9th storage node (node 8 -> only 11 nodes remain < k=12) -> must fail explicitly!
    if let Some(mut srv) = cluster.servers.remove(&NodeId(8)) {
        srv.shutdown();
    }
    let unrec_err = cluster
        .coordinator
        .distributed_get_to_bytes("dist-rs-obj")
        .unwrap_err();
    assert!(matches!(
        unrec_err,
        StorageError::InsufficientValidShards {
            valid_shards: 11,
            required_shards: 12,
            ..
        }
    ));
}

#[test]
fn test_p4_09_subprocess_fsp_node_crash_sigkill_restart_and_idempotent_tx_replay() {
    let sandbox = TempSandbox::new("subprocess_node");
    let pki_dir = sandbox.path().join("pki");
    let pki = provision_cluster_pki(&pki_dir, 1).unwrap();
    let (node_cert, node_key) = pki.node_certs.get(&NodeId(0)).unwrap();
    let node_root = sandbox.path().join("node-00-root");
    let ready_file = sandbox.path().join("node-00.ready");

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
            "52428800",
            "--ready-file",
            ready_file.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn fsp-node child process");

    // Wait for ready_file
    let mut bound_addr = String::new();
    for _ in 0..100 {
        if ready_file.exists() {
            let txt = fs::read_to_string(&ready_file).unwrap_or_default();
            if let Some(line) = txt.lines().find(|l| l.starts_with("ADDR=")) {
                bound_addr = line.strip_prefix("ADDR=").unwrap().to_string();
                break;
            }
        }
        thread::sleep(Duration::from_millis(30));
    }
    assert!(
        !bound_addr.is_empty(),
        "fsp-node subprocess did not write ADDR="
    );

    let client_tls = build_mtls_client_config(
        &pki.ca_cert_pem,
        &pki.coordinator_cert_pem,
        &pki.coordinator_key_pem,
    )
    .unwrap();
    let client = NodeRpcClient::new(client_tls, pki.trust_policy.clone(), Duration::from_secs(5));

    let shard_bytes = deterministic_bytes(0x1234, 1024);
    let sha_hex = sha256_bytes(&shard_bytes).to_hex();
    let put_req = PutShardRequest {
        tx_id: "tx-idem-001".to_string(),
        object_id: "subproc-obj".to_string(),
        shard_id: 0,
        version: 1,
        coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
        stripes: vec![(12_288, 1024, sha_hex)],
    };

    let ack1 = client
        .put_shard(NodeId(0), &bound_addr, put_req.clone(), &shard_bytes)
        .expect("first PutShard to subprocess");
    assert!(!ack1.idempotent_replay);

    // Duplicate PutShard with same tx_id returns idempotent_replay == true!
    let ack2 = client
        .put_shard(NodeId(0), &bound_addr, put_req, &shard_bytes)
        .expect("duplicate PutShard replay");
    assert!(ack2.idempotent_replay);

    // Plant an uncommitted PREPARING journal file and staged .tmp file before SIGKILL
    fs::write(
        node_root.join("journal/tx-incomplete-00.node_tx"),
        b"STATE=PREPARING\ntx_id=tx-incomplete\n",
    )
    .unwrap();
    fs::write(
        node_root.join("staging/incomplete.fspc.tmp"),
        b"partial_bytes",
    )
    .unwrap();

    // Kill the child process abruptly (SIGKILL)
    let _ = child.kill();
    let _ = child.wait();
    let _ = fs::remove_file(&ready_file);

    // Restart fsp-node on the same storage root and verify crash recovery + shard rediscovery
    let mut child2 = Command::new(bin_path)
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
        .expect("respawn fsp-node child process");

    let mut addr2 = String::new();
    let mut ready_text = String::new();
    for _ in 0..100 {
        if ready_file.exists() {
            ready_text = fs::read_to_string(&ready_file).unwrap_or_default();
            if let Some(line) = ready_text.lines().find(|l| l.starts_with("ADDR=")) {
                addr2 = line.strip_prefix("ADDR=").unwrap().to_string();
                break;
            }
        }
        thread::sleep(Duration::from_millis(30));
    }
    assert!(!addr2.is_empty());
    assert!(ready_text.contains("RECOVERED_CONTAINERS=1"));
    assert!(ready_text.contains("ABORTED_TXS=1"));

    let listed = client.list_shards(NodeId(0), &addr2, None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].object_id, "subproc-obj");

    let _ = child2.kill();
    let _ = child2.wait();
}

#[test]
fn test_p4_10_coordinator_crash_recovery_and_orphan_remote_shard_cleanup() {
    let cluster = LocalMtlsCluster::start(6, 20 * 1024 * 1024);
    let nodes = cluster.coordinator.registered_nodes();

    // Write orphan shards directly to nodes 0, 1, 2 as if coordinator crashed mid-PUT before committing manifest
    let shard_bytes = deterministic_bytes(0x9999, 512);
    let sha_hex = sha256_bytes(&shard_bytes).to_hex();
    let mut journal_body =
        "STATE=PREPARING\ntx_id=ctx-crash-99\nobject_id=orphan-obj\nversion=1\n".to_string();

    for i in 0..3 {
        let nid = NodeId(i);
        let addr = &nodes[&nid].listen_addr;
        let req = PutShardRequest {
            tx_id: "ctx-crash-99".to_string(),
            object_id: "orphan-obj".to_string(),
            shard_id: i,
            version: 1,
            coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
            stripes: vec![(6_144, 512, sha_hex.clone())],
        };
        cluster
            .coordinator
            .rpc_client()
            .put_shard(nid, addr, req, &shard_bytes)
            .unwrap();
        journal_body.push_str(&format!("TARGET={}|{}|{}\n", i, nid.0, addr));
    }

    let journal_path = cluster
        .coordinator
        .coord_root()
        .join("journal/ctx-crash-99.coord_tx");
    fs::write(&journal_path, journal_body.as_bytes()).unwrap();

    // Run coordinator crash recovery -> must abort ctx-crash-99 and delete all 3 remote orphan shards!
    let rec_rep = cluster
        .coordinator
        .recover_coordinator_transactions()
        .expect("coordinator crash recovery");
    assert_eq!(rec_rep.uncommitted_txs_aborted, 1);
    assert_eq!(rec_rep.remote_orphan_shards_deleted, 3);

    for i in 0..3 {
        let nid = NodeId(i);
        let addr = &nodes[&nid].listen_addr;
        let list = cluster
            .coordinator
            .rpc_client()
            .list_shards(nid, addr, Some("orphan-obj".to_string()))
            .unwrap();
        assert!(
            list.is_empty(),
            "orphan shard on node-{} must be deleted during coordinator recovery",
            i
        );
    }
}

#[test]
fn test_p4_11_network_partition_packet_loss_corrupt_wire_response_and_quota_rejection() {
    let cluster = LocalMtlsCluster::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0x3141_5926, 90_000);
    cluster
        .coordinator
        .distributed_put_bytes("fault-obj", &payload, DistributedPutOptions::default())
        .unwrap();

    // Inject wire-level payload corruption on nodes 0, 1, 2 AND network partition on nodes 3, 4, 5
    let fp = cluster.coordinator.rpc_client().fault_policy();
    for i in 0..3 {
        fp.set_corrupt_get_response(NodeId(i), true);
    }
    for i in 3..6 {
        fp.set_node_partitioned(NodeId(i), true);
    }
    fp.set_node_delay(NodeId(6), Some(Duration::from_millis(5)));

    let (reconstructed, rep) = cluster
        .coordinator
        .distributed_get_to_bytes("fault-obj")
        .expect("distributed GET must survive 3 corrupt wire responses + 3 partitioned nodes");
    assert_eq!(reconstructed, payload);
    assert_eq!(rep.nodes_failed_or_corrupt.len(), 6);

    // Clear faults
    for i in 0..6 {
        fp.set_corrupt_get_response(NodeId(i), false);
        fp.set_node_partitioned(NodeId(i), false);
    }
    fp.set_node_delay(NodeId(6), None);

    // Test Disk-Full / QuotaExceeded rejection on a tight-quota cluster (capacity = 500 bytes per node)
    let tiny_cluster = LocalMtlsCluster::start(20, 500);
    let quota_err = tiny_cluster.coordinator.distributed_put_bytes(
        "too-big",
        &payload,
        DistributedPutOptions::default(),
    );
    assert!(
        quota_err.is_err(),
        "PUT exceeding node capacity limit must fail cleanly"
    );
}

#[test]
fn test_p4_12_distributed_adaptive_repair_v2_background_scrubber_and_concurrent_access() {
    // 22-node cluster: 20 active nodes for initial RS(20,12) placement + 2 spare nodes (20, 21)
    let mut cluster = LocalMtlsCluster::start(22, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0xFACE_FEED, 128_000);

    let put_rep = cluster
        .coordinator
        .distributed_put_bytes(
            "scrub-repair-obj",
            &payload,
            DistributedPutOptions::default(),
        )
        .unwrap();
    assert_eq!(put_rep.signed_manifest.manifest_generation, 1);

    // 1. Corrupt shard 0 on disk on its assigned node, and delete shard 1 on its assigned node
    let node_s0 = NodeId(put_rep.signed_manifest.manifest.stripes[0].placements[0].node_id);
    let node_s1 = NodeId(put_rep.signed_manifest.manifest.stripes[0].placements[1].node_id);

    let path_s0 = cluster.servers[&node_s0]
        .daemon()
        .resolve_container_path("scrub-repair-obj", 0, 1)
        .unwrap();
    let mut raw_s0 = fs::read(&path_s0).unwrap();
    let body_offset = CONTAINER_HEADER_SIZE as usize + 8;
    raw_s0[body_offset] ^= 0xFF;
    fs::write(&path_s0, &raw_s0).unwrap();

    let path_s1 = cluster.servers[&node_s1]
        .daemon()
        .resolve_container_path("scrub-repair-obj", 1, 1)
        .unwrap();
    fs::remove_file(&path_s1).unwrap();

    // Also shut down node_s1 so shard 1 must be repaired onto one of the spare nodes (20 or 21)!
    if let Some(mut srv) = cluster.servers.remove(&node_s1) {
        srv.shutdown();
    }

    let mut scheduler = DistributedRepairScheduler::new(
        cluster.coordinator.clone(),
        AdaptiveV2Config::default(),
        PlacementPolicy::default(),
    );
    let mut scrubber = BackgroundScrubber::open_or_init(cluster.coordinator.clone(), 25).unwrap();

    // 2. Run background scrub cycle -> detects corrupted shard 0 and unreachable node_s1
    let scrub_rep = scrubber.run_scrub_cycle(&mut scheduler).unwrap();
    assert_eq!(scrub_rep.verifications_executed, 20);
    assert_eq!(scrub_rep.corrupted_shards_detected.len(), 1);
    assert_eq!(scrub_rep.corrupted_shards_detected[0].1, 0);
    assert!(scrub_rep.unreachable_nodes_encountered.contains(&node_s1));

    // Enqueue shard 1 explicitly (simulating confirmed node loss) and verify deduplication
    assert!(scheduler.enqueue_repair("scrub-repair-obj", 1, "Confirmed node loss"));
    assert!(
        !scheduler.enqueue_repair("scrub-repair-obj", 1, "Duplicate enqueue"),
        "repair queue must deduplicate identical (object_id, shard_id) tasks"
    );

    // Also enqueue shard 2 as if node_s2 was transiently unreachable, while node_s2 is actually healthy
    // -> run_repair_pass must cancel shard 2's repair upon verifying shard 2 is intact!
    assert!(scheduler.enqueue_repair("scrub-repair-obj", 2, "Transient suspect"));

    // 3. Execute distributed Adaptive Repair v2 pass concurrently with a reader thread
    let coord_clone = cluster.coordinator.clone();
    let expected_payload = payload.clone();
    let reader_handle = thread::spawn(move || {
        for _ in 0..3 {
            let (bytes, _) = coord_clone
                .distributed_get_to_bytes("scrub-repair-obj")
                .unwrap();
            assert_eq!(bytes, expected_payload);
        }
    });

    let repair_rep = scheduler.run_repair_pass(3_600.0).unwrap();
    reader_handle.join().unwrap();

    assert_eq!(repair_rep.repairs_cancelled_on_node_recovery, 1);
    assert_eq!(repair_rep.shards_repaired, 2);
    assert!(
        repair_rep.updated_manifest_generations["scrub-repair-obj"] >= 2,
        "manifest_generation must advance after distributed repair"
    );

    // 4. Verify all 20 placements in the updated signed manifest are now healthy on their assigned nodes!
    let updated_signed = cluster
        .coordinator
        .load_verified_signed_manifest("scrub-repair-obj")
        .unwrap();
    let new_node_s1 = NodeId(updated_signed.manifest.stripes[0].placements[1].node_id);
    assert_ne!(
        new_node_s1, node_s1,
        "shard 1 must be relocated onto a spare node"
    );

    let scrub_rep2 = scrubber.run_scrub_cycle(&mut scheduler).unwrap();
    assert_eq!(scrub_rep2.healthy_shards_verified, 20);
    assert!(scrub_rep2.corrupted_shards_detected.is_empty());
    assert!(scrub_rep2.missing_shards_detected.is_empty());
}

#[test]
fn test_p4_13_degraded_commit_policy_and_subsequent_repair_to_full_redundancy() {
    let cluster = LocalMtlsCluster::start(20, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0xD3AD_BEEF, 64_000);

    // Partition nodes 18 and 19 AFTER probe_candidate_nodes or simulate drop on PutShard?
    // Wait: if we partition nodes 18 and 19 before probe_candidate_nodes, probe_candidate_nodes
    // sees only 18 online nodes (< 20). Instead, let's test when nodes 18 and 19 drop PutShard
    // or when 20 nodes are probed online and then nodes 18 & 19 fail during PutShard, OR let's
    // partition nodes 18 & 19 right after probe? Even simpler: we can configure a 22-node cluster
    // where 20 nodes are selected (nodes 0..20) and we inject a simulated failure on node 19's
    // PutShard by setting node 19's capacity limit to 500 bytes (so Heartbeat succeeds with
    // available_bytes = 500, or if we set capacity_limit_bytes small on node 19 after startup).
    // Even better: on node 18 and node 19, let's fill their quota or set `corrupt`? Wait:
    // if we set a small capacity limit on node 19 (`20_000` bytes) and `required_shard_bytes` for
    // a 64_000-byte object is `256 + 5336 + 52 = 5644` bytes, what if another object fills node 19
    // or what if we corrupt/delete? Wait: why not add `set_drop_put_shard` or test `StrictAllShards`
    // vs `AllowDegradedCommit` by setting `packet_loss_rate` or partitioning during PUT?
    // Wait: `probe_candidate_nodes` calls `heartbeat`! If `NetworkFaultPolicy` only drops `PutShard`,
    // `heartbeat` succeeds during placement planning and then `PutShard` fails on nodes 18 & 19!
    // Let's test `StrictAllShards` abort + rollback when 2 nodes are offline at placement time AND
    // let's test `AllowDegradedCommit`!
    let fp = cluster.coordinator.rpc_client().fault_policy();
    fp.set_node_partitioned(NodeId(18), true);
    fp.set_node_partitioned(NodeId(19), true);

    // With 2 nodes partitioned out of 20, placement for n=20 fails cleanly without orphan shards
    let err = cluster.coordinator.distributed_put_bytes(
        "strict-fail-obj",
        &payload,
        DistributedPutOptions::default(),
    );
    assert!(err.is_err());

    // Heal nodes 18 and 19 -> PUT with AllowDegradedCommit { min_acknowledged_shards: 18 } succeeds
    fp.set_node_partitioned(NodeId(18), false);
    fp.set_node_partitioned(NodeId(19), false);
    let opts = DistributedPutOptions {
        commit_policy: DegradedCommitPolicy::AllowDegradedCommit {
            min_acknowledged_shards: 18,
        },
        ..DistributedPutOptions::default()
    };
    let rep = cluster
        .coordinator
        .distributed_put_bytes("degraded-ok-obj", &payload, opts)
        .unwrap();
    assert_eq!(rep.acknowledged_nodes.len(), 20);
}

#[test]
fn test_p4_14_lrc_20_12_4_and_lrc_22_12_4_distributed_cluster_put_get_and_repair() {
    let mut cluster = LocalMtlsCluster::start(24, 50 * 1024 * 1024);
    let payload = deterministic_bytes(0xABCD_1234, 150_000);

    for (name, scheme, n_shards) in [
        (
            "dist-lrc-20-12-4",
            CodingScheme::Lrc(LrcConfig::new(20, 12, 4).unwrap()),
            20usize,
        ),
        (
            "dist-lrc-22-12-4",
            CodingScheme::Lrc(LrcConfig::new(22, 12, 4).unwrap()),
            22usize,
        ),
    ] {
        let opts = DistributedPutOptions {
            coding_scheme: scheme,
            stripe_size: 65_536,
            placement_policy: PlacementPolicy::SingleHostMultiDaemon {
                max_shards_per_rack: 6,
            },
            commit_policy: DegradedCommitPolicy::StrictAllShards,
            tx_id_override: None,
        };
        let put_rep = cluster
            .coordinator
            .distributed_put_bytes(name, &payload, opts)
            .expect("distributed LRC PUT");
        assert_eq!(put_rep.acknowledged_nodes.len(), n_shards);

        // Delete 5 shards (0, 1, 5, 10, 15) on their hosting nodes and verify degraded distributed GET
        for &s_id in &[0usize, 1, 5, 10, 15] {
            let nid = NodeId(put_rep.signed_manifest.manifest.stripes[0].placements[s_id].node_id);
            let p = cluster.servers[&nid]
                .daemon()
                .resolve_container_path(name, s_id, 1)
                .unwrap();
            fs::remove_file(&p).unwrap();
        }

        let (rec, get_rep) = cluster.coordinator.distributed_get_to_bytes(name).unwrap();
        assert_eq!(rec, payload);
        assert_eq!(get_rep.nodes_failed_or_corrupt.len(), 5);

        // Repair via DistributedRepairScheduler
        let mut sched = DistributedRepairScheduler::new(
            cluster.coordinator.clone(),
            AdaptiveV2Config::default(),
            PlacementPolicy::SingleHostMultiDaemon {
                max_shards_per_rack: 6,
            },
        );
        for &s_id in &[0usize, 1, 5, 10, 15] {
            sched.enqueue_repair(name, s_id, "LRC test repair");
        }
        let rep = sched.run_repair_pass(1_000.0).unwrap();
        assert_eq!(rep.shards_repaired, 5);
    }
    let _ = &mut cluster;
}

#[test]
fn test_p4_15_oversized_rpc_metadata_and_payload_frame_rejection() {
    use fsp_sim::network::protocol::{
        FRAME_MAGIC, MAX_RPC_META_BYTES, MAX_RPC_PAYLOAD_BYTES, NETWORK_PROTOCOL_VERSION,
    };
    use fsp_sim::network::transport::read_rpc_frame;

    // 1. Construct a 14-byte frame header claiming meta_len = MAX_RPC_META_BYTES + 1
    let mut bad_meta_hdr = [0u8; 14];
    bad_meta_hdr[0..4].copy_from_slice(&FRAME_MAGIC);
    bad_meta_hdr[4..6].copy_from_slice(&NETWORK_PROTOCOL_VERSION.to_le_bytes());
    bad_meta_hdr[6..10].copy_from_slice(&(MAX_RPC_META_BYTES + 1).to_le_bytes());
    bad_meta_hdr[10..14].copy_from_slice(&0u32.to_le_bytes());

    let mut cursor = std::io::Cursor::new(bad_meta_hdr);
    let err = read_rpc_frame(&mut cursor, MAX_RPC_PAYLOAD_BYTES).unwrap_err();
    assert!(matches!(
        err,
        RpcError::FrameTooLarge {
            field: "meta_len",
            ..
        }
    ));

    // 2. Construct a 14-byte frame header claiming payload_len = MAX_RPC_PAYLOAD_BYTES + 100
    let mut bad_payload_hdr = [0u8; 14];
    bad_payload_hdr[0..4].copy_from_slice(&FRAME_MAGIC);
    bad_payload_hdr[4..6].copy_from_slice(&NETWORK_PROTOCOL_VERSION.to_le_bytes());
    bad_payload_hdr[6..10].copy_from_slice(&16u32.to_le_bytes());
    bad_payload_hdr[10..14].copy_from_slice(&(MAX_RPC_PAYLOAD_BYTES + 100).to_le_bytes());

    let mut cursor2 = std::io::Cursor::new(bad_payload_hdr);
    let err2 = read_rpc_frame(&mut cursor2, MAX_RPC_PAYLOAD_BYTES).unwrap_err();
    assert!(matches!(
        err2,
        RpcError::FrameTooLarge {
            field: "payload_len",
            ..
        }
    ));
}

#[test]
fn test_p4_16_path_traversal_symlink_nofollow_and_production_strict_trust_mode() {
    use fsp_sim::security::authorization::{
        AuthError, BoundIdentityEntry, ClusterTrustPolicy, PeerRole, TrustEnvironmentMode,
    };
    use fsp_sim::storage::filesystem::{create_file_nofollow, open_file_nofollow};

    // 1. ProductionStrict mode must reject any identity marked is_test_only = true
    let mut prod_policy = ClusterTrustPolicy::new(TrustEnvironmentMode::ProductionStrict);
    let test_entry = BoundIdentityEntry {
        identity_id: "test-coord".to_string(),
        role: PeerRole::Coordinator {
            coordinator_id: "test-coord".to_string(),
        },
        allowed_cert_fingerprints: ["aa".repeat(32)].into_iter().collect(),
        not_after_unix_secs: 4_102_444_800,
        is_test_only: true,
    };
    assert!(matches!(
        prod_policy.register_identity(test_entry),
        Err(AuthError::TestTrustRootRejectedInProduction(_))
    ));

    // 2. O_NOFOLLOW kernel-level symlink rejection on Unix
    #[cfg(unix)]
    {
        let sandbox = TempSandbox::new("nofollow_symlink");
        let real_file = sandbox.path().join("real_target.txt");
        fs::write(&real_file, b"secret").unwrap();
        let symlink_path = sandbox.path().join("evil_link.txt");
        std::os::unix::fs::symlink(&real_file, &symlink_path).unwrap();

        assert!(
            open_file_nofollow(&symlink_path).is_err(),
            "open_file_nofollow must reject symlinks via O_NOFOLLOW + symlink_metadata"
        );
        assert!(
            create_file_nofollow(&symlink_path).is_err(),
            "create_file_nofollow must reject symlinks via O_NOFOLLOW + symlink_metadata"
        );
    }
}

#[test]
fn test_p4_17_background_scrubber_cursor_persistence_across_coordinator_restart() {
    let cluster = LocalMtlsCluster::start(20, 50 * 1024 * 1024);
    let payload1 = deterministic_bytes(0x1111, 32_000);
    let payload2 = deterministic_bytes(0x2222, 32_000);

    cluster
        .coordinator
        .distributed_put_bytes("scrub-obj-a", &payload1, DistributedPutOptions::default())
        .unwrap();
    cluster
        .coordinator
        .distributed_put_bytes("scrub-obj-b", &payload2, DistributedPutOptions::default())
        .unwrap();

    let mut scheduler = DistributedRepairScheduler::new(
        cluster.coordinator.clone(),
        AdaptiveV2Config::default(),
        PlacementPolicy::default(),
    );

    // Cycle 1: scrub 15 shards (out of 40 total across 2 objects) -> stops at (object=0, shard=15)
    {
        let mut scrubber1 =
            BackgroundScrubber::open_or_init(cluster.coordinator.clone(), 15).unwrap();
        let rep1 = scrubber1.run_scrub_cycle(&mut scheduler).unwrap();
        assert_eq!(rep1.cycle_number, 1);
        assert_eq!(rep1.verifications_executed, 15);
        assert_eq!(rep1.healthy_shards_verified, 15);
        assert_eq!(rep1.next_cursor_object_index, 0);
        assert_eq!(rep1.next_cursor_shard_index, 15);
    }

    // Re-open BackgroundScrubber from persisted cursor on disk and run Cycle 2 (15 shards -> (object=1, shard=10))
    {
        let mut scrubber2 =
            BackgroundScrubber::open_or_init(cluster.coordinator.clone(), 15).unwrap();
        let rep2 = scrubber2.run_scrub_cycle(&mut scheduler).unwrap();
        assert_eq!(rep2.cycle_number, 2);
        assert_eq!(rep2.verifications_executed, 15);
        assert_eq!(rep2.healthy_shards_verified, 15);
        assert_eq!(rep2.next_cursor_object_index, 1);
        assert_eq!(rep2.next_cursor_shard_index, 10);

        // Cycle 3: remaining 10 shards wrap cursor back to (0, 0)
        let rep3 = scrubber2.run_scrub_cycle(&mut scheduler).unwrap();
        assert_eq!(rep3.cycle_number, 3);
        assert_eq!(rep3.verifications_executed, 10);
        assert_eq!(rep3.healthy_shards_verified, 10);
        assert_eq!(rep3.next_cursor_object_index, 0);
        assert_eq!(rep3.next_cursor_shard_index, 0);
    }
}

#[test]
fn test_p4_18_concurrent_same_object_local_and_distributed_mutations_serialized() {
    let cluster = LocalMtlsCluster::start(20, 100 * 1024 * 1024);
    let payload = deterministic_bytes(0x8888, 64_000);
    cluster
        .coordinator
        .distributed_put_bytes("concurrent-obj", &payload, DistributedPutOptions::default())
        .unwrap();

    // Spawn 4 concurrent threads reading and verifying the same object while another thread triggers repair checks
    thread::scope(|s| {
        for _ in 0..4 {
            let coord = cluster.coordinator.clone();
            let expected = payload.clone();
            s.spawn(move || {
                for _ in 0..3 {
                    let (got, _) = coord.distributed_get_to_bytes("concurrent-obj").unwrap();
                    assert_eq!(got, expected);
                }
            });
        }
        let coord = cluster.coordinator.clone();
        s.spawn(move || {
            let mut sched = DistributedRepairScheduler::new(
                coord,
                AdaptiveV2Config::default(),
                PlacementPolicy::default(),
            );
            let _ = sched.run_repair_pass(500.0).unwrap();
        });
    });
}

#[test]
fn test_p4_19_coordinator_metadata_loss_and_signed_backup_bundle_recovery() {
    let cluster = LocalMtlsCluster::start(20, 50 * 1024 * 1024);
    let p1 = deterministic_bytes(0xAAAA, 40_000);
    let p2 = deterministic_bytes(0xBBBB, 50_000);

    cluster
        .coordinator
        .distributed_put_bytes("backup-obj-1", &p1, DistributedPutOptions::default())
        .unwrap();
    cluster
        .coordinator
        .distributed_put_bytes("backup-obj-2", &p2, DistributedPutOptions::default())
        .unwrap();

    // Export Ed25519-signed MetadataBackupBundle
    let bundle = cluster.coordinator.export_metadata_backup().unwrap();
    assert_eq!(bundle.signed_manifests.len(), 2);

    // Tampered backup bundle must be rejected
    let mut tampered_bundle = bundle.clone();
    tampered_bundle.bundle_digest = "00".repeat(32);
    assert!(cluster
        .coordinator
        .restore_metadata_backup(&tampered_bundle)
        .is_err());

    // Simulate catastrophic loss of coordinator's local manifest files
    let manifests_dir = cluster.coordinator.coord_root().join("manifests");
    for entry in fs::read_dir(&manifests_dir).unwrap() {
        fs::remove_file(entry.unwrap().path()).unwrap();
    }
    assert!(matches!(
        cluster.coordinator.distributed_get_to_bytes("backup-obj-1"),
        Err(StorageError::ObjectNotFound(_))
    ));

    // Restore from signed MetadataBackupBundle -> both objects are immediately recoverable from remote storage nodes!
    let restored = cluster
        .coordinator
        .restore_metadata_backup(&bundle)
        .unwrap();
    assert_eq!(restored, 2);

    let (got1, _) = cluster
        .coordinator
        .distributed_get_to_bytes("backup-obj-1")
        .unwrap();
    let (got2, _) = cluster
        .coordinator
        .distributed_get_to_bytes("backup-obj-2")
        .unwrap();
    assert_eq!(got1, p1);
    assert_eq!(got2, p2);
}

#[test]
fn test_p4_20_deterministic_repeat_execution_of_distributed_workflows() {
    let payload = deterministic_bytes(0x4242_4242, 100_000);

    let run_once = || -> (String, String, PlacementPolicy, usize) {
        let cluster = LocalMtlsCluster::start(20, 50 * 1024 * 1024);
        let opts = DistributedPutOptions {
            coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
            stripe_size: 65_536,
            placement_policy: PlacementPolicy::default(),
            commit_policy: DegradedCommitPolicy::StrictAllShards,
            tx_id_override: Some("deterministic-tx-001".to_string()),
        };
        let rep = cluster
            .coordinator
            .distributed_put_bytes("det-obj", &payload, opts)
            .unwrap();
        (
            rep.signed_manifest.manifest.manifest_digest.clone(),
            rep.signed_manifest.signature_hex.clone(),
            PlacementPolicy::default(),
            rep.placement_report.distinct_racks,
        )
    };

    let r1 = run_once();
    let r2 = run_once();
    assert_eq!(
        r1, r2,
        "identical inputs, keys, and topology must produce bit-identical manifest digest and Ed25519 signature"
    );
}
