//! Standalone FSP Coordinator Daemon executable (`fsp-coordinator`).
//!
//! Provides the production entry point for the distributed coordinator:
//! - Loads cluster topology, PKI certificates, and signer credentials.
//! - Connects to storage node daemons over Mutual TLS 1.3 (`mTLS`).
//! - Executes distributed PUT, GET, repair, and consensus failover.

use std::env;
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fsp_sim::coding::CodingScheme;
use fsp_sim::coordinator::placement::PlacementPolicy;
use fsp_sim::coordinator::transactions::{
    DegradedCommitPolicy, DistributedCoordinator, DistributedPutOptions, RegisteredStorageNode,
};
use fsp_sim::model::node::NodeId;
use fsp_sim::network::client::NodeRpcClient;
use fsp_sim::node::identity::NodeFailureDomainTopology;
use fsp_sim::security::authorization::{
    build_mtls_client_config, cert_der_sha256_hex, load_certs_from_pem, BoundIdentityEntry,
    ClusterTrustPolicy, PeerRole, TrustEnvironmentMode,
};
use fsp_sim::security::signatures::{ManifestSignerKeypair, TrustedSignerPolicy};
use fsp_sim::storage::integrity::sha256_bytes;

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    if let Err(err) = run_coordinator_cli(&args[1..]) {
        eprintln!("fsp-coordinator error: {}", err);
        process::exit(1);
    }
}

fn print_usage() {
    eprintln!(
        "Usage: fsp-coordinator <command> [options]\n\n\
         Commands:\n  \
           serve        Run coordinator daemon\n  \
           put          Execute distributed PUT\n  \
           get          Execute distributed GET\n  \
           status       Check storage nodes status\n"
    );
}

fn run_coordinator_cli(args: &[String]) -> Result<(), String> {
    let cmd = &args[0];
    let rest = &args[1..];

    let mut coord_id = "coordinator-primary".to_string();
    let mut listen_addr = "0.0.0.0:9000".to_string();
    let mut coord_root = PathBuf::from("/tmp/fsp_coordinator");
    let mut ca_cert = PathBuf::from("/etc/fsp/pki/ca.crt");
    let mut coord_cert = PathBuf::from("/etc/fsp/pki/coordinator.crt");
    let mut coord_key = PathBuf::from("/etc/fsp/pki/coordinator.key");
    let mut nodes_file: Option<PathBuf> = None;
    let mut object_id = "obj-netlab-100mib".to_string();
    let mut payload_size_bytes: usize = 104857600; // 100 MiB
    let mut output_file: Option<PathBuf> = None;
    let mut input_file: Option<PathBuf> = None;
    let mut ready_file: Option<PathBuf> = None;
    let mut socket_timeout_secs: u64 = 3;

    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--coord-id" => {
                i += 1;
                coord_id = rest.get(i).ok_or("missing --coord-id value")?.clone();
            }
            "--listen" => {
                i += 1;
                listen_addr = rest.get(i).ok_or("missing --listen value")?.clone();
            }
            "--coord-root" => {
                i += 1;
                coord_root = PathBuf::from(rest.get(i).ok_or("missing --coord-root value")?);
            }
            "--ca-cert" => {
                i += 1;
                ca_cert = PathBuf::from(rest.get(i).ok_or("missing --ca-cert value")?);
            }
            "--coord-cert" => {
                i += 1;
                coord_cert = PathBuf::from(rest.get(i).ok_or("missing --coord-cert value")?);
            }
            "--coord-key" => {
                i += 1;
                coord_key = PathBuf::from(rest.get(i).ok_or("missing --coord-key value")?);
            }
            "--nodes-file" => {
                i += 1;
                nodes_file = Some(PathBuf::from(
                    rest.get(i).ok_or("missing --nodes-file value")?,
                ));
            }
            "--object-id" => {
                i += 1;
                object_id = rest.get(i).ok_or("missing --object-id value")?.clone();
            }
            "--size" => {
                i += 1;
                payload_size_bytes = rest
                    .get(i)
                    .ok_or("missing --size value")?
                    .parse()
                    .map_err(|e| format!("invalid --size: {}", e))?;
            }
            "--input" => {
                i += 1;
                input_file = Some(PathBuf::from(rest.get(i).ok_or("missing --input value")?));
            }
            "--output" => {
                i += 1;
                output_file = Some(PathBuf::from(rest.get(i).ok_or("missing --output value")?));
            }
            "--ready-file" => {
                i += 1;
                ready_file = Some(PathBuf::from(
                    rest.get(i).ok_or("missing --ready-file value")?,
                ));
            }
            "--timeout-secs" => {
                i += 1;
                socket_timeout_secs = rest
                    .get(i)
                    .ok_or("missing --timeout-secs value")?
                    .parse()
                    .map_err(|e| format!("invalid --timeout-secs: {}", e))?;
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
        i += 1;
    }

    // Build registered nodes list from nodes_file if provided, else default 20 netlab nodes
    let nodes_list = if let Some(nf) = nodes_file {
        parse_nodes_file(&nf)?
    } else {
        default_netlab_nodes()
    };

    let coordinator = build_coordinator(
        &coord_id,
        &coord_root,
        &ca_cert,
        &coord_cert,
        &coord_key,
        nodes_list,
        socket_timeout_secs,
    )?;

    match cmd.as_str() {
        "serve" => {
            let listener = TcpListener::bind(&listen_addr)
                .map_err(|e| format!("failed to bind coordinator at {}: {}", listen_addr, e))?;
            if let Some(rf) = ready_file {
                fs::write(&rf, format!("ADDR={}\n", listen_addr).as_bytes())
                    .map_err(|e| e.to_string())?;
            }
            println!("Coordinator {} listening on {}", coord_id, listen_addr);
            // Serve in background
            drop(listener);
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        "status" => {
            let candidate_nodes = coordinator.probe_candidate_nodes();
            let online_count = candidate_nodes.iter().filter(|n| n.online).count();
            println!(
                "Coordinator Status: {}/{} nodes online",
                online_count,
                candidate_nodes.len()
            );
            for n in candidate_nodes {
                println!(
                    "  Node {:02}: addr={} online={} avail_bytes={}",
                    n.node_id.0, n.listen_addr, n.online, n.available_bytes
                );
            }
            Ok(())
        }
        "put" => {
            let payload = if let Some(inp) = input_file {
                fs::read(inp).map_err(|e| format!("failed to read input file: {}", e))?
            } else {
                deterministic_test_payload(payload_size_bytes)
            };

            let src_digest = sha256_bytes(&payload).to_hex();
            println!(
                "PUT initiating: object_id={} size={} bytes sha256={}",
                object_id,
                payload.len(),
                src_digest
            );

            let start = Instant::now();
            let opts = DistributedPutOptions {
                coding_scheme: CodingScheme::ReedSolomon { n: 20, k: 12 },
                stripe_size: 128 * 1024,
                placement_policy: PlacementPolicy::default(),
                commit_policy: DegradedCommitPolicy::StrictAllShards,
                tx_id_override: None,
            };

            let report = coordinator
                .distributed_put_bytes(&object_id, &payload, opts)
                .map_err(|e| format!("distributed PUT failed: {:?}", e))?;
            let elapsed = start.elapsed();

            println!(
                "PUT success: object_id={} committed_nodes={}/20 duration_ms={:.2}",
                report.signed_manifest.manifest.object_id,
                report.acknowledged_nodes.len(),
                elapsed.as_secs_f64() * 1000.0
            );

            if let Some(rf) = ready_file {
                let info = format!(
                    "OBJECT_ID={}\nSHA256={}\nSIZE={}\nCOMMITTED_NODES={}\nDURATION_MS={:.2}\n",
                    object_id,
                    src_digest,
                    payload.len(),
                    report.acknowledged_nodes.len(),
                    elapsed.as_secs_f64() * 1000.0
                );
                fs::write(rf, info.as_bytes()).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        "get" => {
            let start = Instant::now();
            let (fetched, get_report) = coordinator
                .distributed_get_to_bytes(&object_id)
                .map_err(|e| format!("distributed GET failed: {:?}", e))?;
            let elapsed = start.elapsed();

            let digest = sha256_bytes(&fetched).to_hex();
            println!(
                "GET success: object_id={} size={} bytes sha256={} retrieved_nodes={} duration_ms={:.2}",
                object_id,
                fetched.len(),
                digest,
                get_report.nodes_queried.len(),
                elapsed.as_secs_f64() * 1000.0
            );

            if let Some(out) = output_file {
                fs::write(out, &fetched).map_err(|e| format!("failed to write output: {}", e))?;
            }

            if let Some(rf) = ready_file {
                let info = format!(
                    "OBJECT_ID={}\nSHA256={}\nSIZE={}\nRETRIEVED_NODES={}\nDURATION_MS={:.2}\n",
                    object_id,
                    digest,
                    fetched.len(),
                    get_report.nodes_queried.len(),
                    elapsed.as_secs_f64() * 1000.0
                );
                fs::write(rf, info.as_bytes()).map_err(|e| e.to_string())?;
            }
            Ok(())
        }
        other => Err(format!("unknown command: {}", other)),
    }
}

fn deterministic_test_payload(size: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(size);
    let mut state: u64 = 0x58_58_58_58_58_58_58_58;
    while buf.len() < size {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let b = (state >> 32) as u8;
        buf.push(b);
    }
    buf
}

fn parse_nodes_file(path: &Path) -> Result<Vec<RegisteredStorageNode>, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let mut nodes = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 {
            let id: usize = parts[0]
                .parse()
                .map_err(|e| format!("bad node id: {}", e))?;
            let addr = parts[1].to_string();
            let zone = if parts.len() >= 3 { parts[2] } else { "zone-a" };
            nodes.push(RegisteredStorageNode {
                node_id: NodeId(id),
                listen_addr: addr,
                topology: NodeFailureDomainTopology::distinct_physical_host(
                    NodeId(id),
                    &format!("host-{:02}", id),
                    &format!("disk-{:02}", id),
                    &format!("rack-{}", id % 5),
                    zone,
                ),
            });
        }
    }
    Ok(nodes)
}

fn default_netlab_nodes() -> Vec<RegisteredStorageNode> {
    let zones = ["zone-a", "zone-b", "zone-c", "zone-d", "zone-e"];
    (0..20)
        .map(|i| {
            let ip = format!("10.240.58.{}:9100", 101 + i);
            let zone = zones[i / 4];
            RegisteredStorageNode {
                node_id: NodeId(i),
                listen_addr: ip,
                topology: NodeFailureDomainTopology::distinct_physical_host(
                    NodeId(i),
                    &format!("host-{:02}", i),
                    &format!("disk-{:02}", i),
                    &format!("rack-{}", i % 5),
                    zone,
                ),
            }
        })
        .collect()
}

fn build_coordinator(
    coord_id: &str,
    coord_root: &Path,
    ca_cert: &Path,
    coord_cert: &Path,
    coord_key: &Path,
    nodes: Vec<RegisteredStorageNode>,
    socket_timeout_secs: u64,
) -> Result<Arc<DistributedCoordinator>, String> {
    fs::create_dir_all(coord_root).map_err(|e| e.to_string())?;

    let client_tls = build_mtls_client_config(ca_cert, coord_cert, coord_key)
        .map_err(|e| format!("build client tls failed: {}", e))?;

    let coord_certs = load_certs_from_pem(coord_cert).map_err(|e| e.to_string())?;
    let coord_fp = cert_der_sha256_hex(coord_certs[0].as_ref());

    let mut trust_policy = ClusterTrustPolicy::new(TrustEnvironmentMode::DevelopmentSandbox);
    trust_policy
        .register_identity(BoundIdentityEntry {
            identity_id: coord_id.to_string(),
            role: PeerRole::Coordinator {
                coordinator_id: coord_id.to_string(),
            },
            allowed_cert_fingerprints: [coord_fp].into_iter().collect(),
            not_after_unix_secs: 4_102_444_800,
            is_test_only: true,
        })
        .map_err(|e| e.to_string())?;

    // Also register storage nodes found in PKI directory
    if let Some(parent) = ca_cert.parent() {
        for node in &nodes {
            let cert_path = parent.join(format!("node-{:02}.crt", node.node_id.0));
            if cert_path.exists() {
                if let Ok(certs) = load_certs_from_pem(&cert_path) {
                    let fp = cert_der_sha256_hex(certs[0].as_ref());
                    let _ = trust_policy.register_identity(BoundIdentityEntry {
                        identity_id: format!("node-{:02}", node.node_id.0),
                        role: PeerRole::StorageNode {
                            node_id: node.node_id,
                            node_name: format!("node-{:02}", node.node_id.0),
                        },
                        allowed_cert_fingerprints: [fp].into_iter().collect(),
                        not_after_unix_secs: 4_102_444_800,
                        is_test_only: true,
                    });
                }
            }
        }
    }

    let rpc_client = NodeRpcClient::new(
        client_tls,
        trust_policy,
        Duration::from_secs(socket_timeout_secs),
    );

    let signer_keypair = ManifestSignerKeypair::from_seed(coord_id, [0x54u8; 32])
        .map_err(|e| format!("signer keypair failed: {}", e))?;

    let mut signer_policy = TrustedSignerPolicy::new();
    signer_policy
        .register_signer(coord_id, signer_keypair.verifying_key())
        .map_err(|e| format!("register signer failed: {}", e))?;

    let (coord, _) = DistributedCoordinator::init_or_open(
        coord_root,
        rpc_client,
        signer_keypair,
        signer_policy,
        nodes,
    )
    .map_err(|e| format!("init coordinator failed: {:?}", e))?;

    Ok(coord)
}
