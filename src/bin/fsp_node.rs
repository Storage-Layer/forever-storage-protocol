//! Standalone FSP Storage Node Daemon executable (`fsp-node`).
//!
//! Runs an independent OS storage node daemon process over Mutual TLS 1.3 (`mTLS`) with its own
//! dedicated storage root (`.fsp-node-marker`), persistent node identity, local v2 shard container
//! index, transaction journal, and startup crash recovery.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;
use std::thread;
use std::time::Duration;

use fsp_sim::model::node::NodeId;
use fsp_sim::network::server::NodeRpcServer;
use fsp_sim::node::daemon::{StorageNodeConfig, StorageNodeDaemon};
use fsp_sim::security::authorization::{
    build_mtls_server_config, cert_der_sha256_hex, load_certs_from_pem, BoundIdentityEntry,
    ClusterTrustPolicy, PeerRole, TrustEnvironmentMode,
};

fn main() {
    let args: Vec<String> = env::args().collect();
    if let Err(err) = run_node_cli(&args[1..]) {
        eprintln!("fsp-node error: {}", err);
        process::exit(1);
    }
}

fn run_node_cli(args: &[String]) -> Result<(), String> {
    let mut node_id_num: Option<usize> = None;
    let mut listen_addr = "127.0.0.1:0".to_string();
    let mut storage_root: Option<PathBuf> = None;
    let mut ca_cert: Option<PathBuf> = None;
    let mut node_cert: Option<PathBuf> = None;
    let mut node_key: Option<PathBuf> = None;
    let mut coord_certs: Vec<PathBuf> = Vec::new();
    let mut observer_cert: Option<PathBuf> = None;
    let mut capacity_bytes: u64 = 10 * 1024 * 1024 * 1024;
    let mut ready_file: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--node-id" => {
                i += 1;
                node_id_num = Some(
                    args.get(i)
                        .ok_or("missing --node-id value")?
                        .parse()
                        .map_err(|e| format!("invalid --node-id: {}", e))?,
                );
            }
            "--listen" => {
                i += 1;
                listen_addr = args.get(i).ok_or("missing --listen value")?.clone();
            }
            "--storage-root" => {
                i += 1;
                storage_root = Some(PathBuf::from(
                    args.get(i).ok_or("missing --storage-root value")?,
                ));
            }
            "--ca-cert" => {
                i += 1;
                ca_cert = Some(PathBuf::from(args.get(i).ok_or("missing --ca-cert value")?));
            }
            "--node-cert" => {
                i += 1;
                node_cert = Some(PathBuf::from(
                    args.get(i).ok_or("missing --node-cert value")?,
                ));
            }
            "--node-key" => {
                i += 1;
                node_key = Some(PathBuf::from(
                    args.get(i).ok_or("missing --node-key value")?,
                ));
            }
            "--coord-cert" => {
                i += 1;
                coord_certs.push(PathBuf::from(
                    args.get(i).ok_or("missing --coord-cert value")?,
                ));
            }
            "--observer-cert" => {
                i += 1;
                observer_cert = Some(PathBuf::from(
                    args.get(i).ok_or("missing --observer-cert value")?,
                ));
            }
            "--capacity-bytes" => {
                i += 1;
                capacity_bytes = args
                    .get(i)
                    .ok_or("missing --capacity-bytes value")?
                    .parse()
                    .map_err(|e| format!("invalid --capacity-bytes: {}", e))?;
            }
            "--ready-file" => {
                i += 1;
                ready_file = Some(PathBuf::from(
                    args.get(i).ok_or("missing --ready-file value")?,
                ));
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
        i += 1;
    }

    let node_id = NodeId(node_id_num.ok_or("--node-id is required")?);
    let storage_root = storage_root.ok_or("--storage-root is required")?;
    let ca_cert = ca_cert.ok_or("--ca-cert is required")?;
    let node_cert = node_cert.ok_or("--node-cert is required")?;
    let node_key = node_key.ok_or("--node-key is required")?;

    let node_certs = load_certs_from_pem(&node_cert).map_err(|e| e.to_string())?;
    let node_fp = cert_der_sha256_hex(node_certs[0].as_ref());

    let mut trust_policy = ClusterTrustPolicy::new(TrustEnvironmentMode::DevelopmentSandbox);

    for path in &coord_certs {
        if let Ok(certs) = load_certs_from_pem(path) {
            let fp = cert_der_sha256_hex(certs[0].as_ref());
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("coordinator");
            let _ = trust_policy.register_identity(BoundIdentityEntry {
                identity_id: id.to_string(),
                role: PeerRole::Coordinator {
                    coordinator_id: id.to_string(),
                },
                allowed_cert_fingerprints: [fp].into_iter().collect(),
                not_after_unix_secs: 4_102_444_800,
                is_test_only: true,
            });
        }
    }

    // Auto-discover any coordinator*.crt certificates in ca_cert parent directory
    if let Some(pki_dir) = ca_cert.parent() {
        if let Ok(entries) = fs::read_dir(pki_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if s.starts_with("coordinator") && s.ends_with(".crt") {
                    if let Ok(certs) = load_certs_from_pem(&entry.path()) {
                        let fp = cert_der_sha256_hex(certs[0].as_ref());
                        let id = entry
                            .path()
                            .file_stem()
                            .unwrap()
                            .to_string_lossy()
                            .to_string();
                        let _ = trust_policy.register_identity(BoundIdentityEntry {
                            identity_id: id.clone(),
                            role: PeerRole::Coordinator { coordinator_id: id },
                            allowed_cert_fingerprints: [fp].into_iter().collect(),
                            not_after_unix_secs: 4_102_444_800,
                            is_test_only: true,
                        });
                    }
                }
            }
        }
    }

    if let Some(obs_path) = observer_cert {
        let obs_certs = load_certs_from_pem(&obs_path).map_err(|e| e.to_string())?;
        let obs_fp = cert_der_sha256_hex(obs_certs[0].as_ref());
        trust_policy
            .register_identity(BoundIdentityEntry {
                identity_id: "observer-readonly".to_string(),
                role: PeerRole::ReadOnlyObserver {
                    observer_id: "observer-readonly".to_string(),
                },
                allowed_cert_fingerprints: [obs_fp].into_iter().collect(),
                not_after_unix_secs: 4_102_444_800,
                is_test_only: true,
            })
            .map_err(|e| e.to_string())?;
    }

    let cfg = StorageNodeConfig::localhost(
        node_id,
        &listen_addr,
        &storage_root,
        &node_fp,
        capacity_bytes,
    );
    let (daemon, recovery_report) =
        StorageNodeDaemon::init_or_open(cfg).map_err(|e| e.to_string())?;
    let tls_cfg =
        build_mtls_server_config(&ca_cert, &node_cert, &node_key).map_err(|e| e.to_string())?;

    let server = NodeRpcServer::start(daemon, tls_cfg, trust_policy, Duration::from_secs(10))
        .map_err(|e| e.to_string())?;

    let bound_str = server.bound_addr().to_string();
    if let Some(rf) = ready_file {
        let ready_info = format!(
            "ADDR={}\nRECOVERED_CONTAINERS={}\nABORTED_TXS={}\n",
            bound_str,
            recovery_report.valid_containers_rediscovered,
            recovery_report.uncommitted_journal_entries_aborted
        );
        fs::write(&rf, ready_info.as_bytes()).map_err(|e| e.to_string())?;
    }

    loop {
        thread::sleep(Duration::from_secs(3600));
    }
}
