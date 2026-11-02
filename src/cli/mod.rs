//! Command-line interface for FSP persistent storage operations, preflight validation,
//! topology auditing, cluster status, and emergency offline reads.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::coding::reed_solomon::ReedSolomonCodec;
use crate::coding::{CodingScheme, LrcConfig};
use crate::config::loader::load_coordinator_config_file;
use crate::config::validation::TopologyPreflightReport;
use crate::model::shard::ShardId;
pub use crate::security::recovery_auth::execute_secure_emergency_read;
use crate::storage::backend::StorageError;
use crate::storage::container::ShardContainerReader;
use crate::storage::engine::StorageEngine;
use crate::storage::filesystem::write_and_sync_file;
use crate::storage::object::PutOptions;
use crate::storage::transaction::{CrashMode, CrashPoint, CrashTrigger};

/// Top-level parsed FSP CLI commands.
#[derive(Debug, Clone)]
pub enum FspCliCommand {
    Storage(StorageCliCommand),
    Preflight {
        config: PathBuf,
    },
    TopologyValidate {
        config: PathBuf,
    },
    ClusterStatus {
        config: PathBuf,
    },
    EmergencyRead {
        object_id: String,
        config: PathBuf,
        output: PathBuf,
    },
}

/// Parsed `storage` CLI subcommand.
#[derive(Debug, Clone)]
pub enum StorageCliCommand {
    Init {
        root: PathBuf,
        nodes: usize,
        domains: usize,
    },
    Put {
        input: PathBuf,
        root: PathBuf,
        object_id: Option<String>,
        scheme: CodingScheme,
        stripe_size: usize,
        crash_point: Option<CrashPoint>,
    },
    Get {
        object_id: String,
        root: PathBuf,
        output: PathBuf,
    },
    Verify {
        object_id: String,
        root: PathBuf,
    },
    Inspect {
        object_id: String,
        root: PathBuf,
    },
    Repair {
        object_id: String,
        root: PathBuf,
    },
    Status {
        root: PathBuf,
    },
    Recover {
        root: PathBuf,
    },
}

/// Parses `storage <subcommand> [args...]` CLI arguments.
pub fn parse_storage_args(args: &[String]) -> Result<StorageCliCommand, String> {
    let Some(sub) = args.first().map(|s| s.as_str()) else {
        return Err(
            "Missing storage subcommand (expected: init|put|get|verify|inspect|repair|status|recover)"
                .to_string(),
        );
    };
    let rest = &args[1..];

    let root = find_flag_value(rest, "--root")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".fsp-dev"));

    match sub {
        "init" => {
            let nodes = find_flag_value(rest, "--nodes")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(20);
            let domains = find_flag_value(rest, "--domains")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(nodes);
            Ok(StorageCliCommand::Init {
                root,
                nodes,
                domains,
            })
        }
        "put" | "crash-put" => {
            let positional = first_positional(rest).ok_or_else(|| {
                "Usage: fsp storage put <file-path> --root <dir> [--id <object-id>]".to_string()
            })?;
            let object_id = find_flag_value(rest, "--id");
            let stripe_size = find_flag_value(rest, "--stripe-size")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(1_048_576);
            let scheme = match find_flag_value(rest, "--scheme").as_deref() {
                Some("lrc-22-12-4") => {
                    CodingScheme::Lrc(LrcConfig::new(22, 12, 4).map_err(|e| e.to_string())?)
                }
                Some("lrc-20-12-4") => {
                    CodingScheme::Lrc(LrcConfig::new(20, 12, 4).map_err(|e| e.to_string())?)
                }
                Some("rs-20-12") | None => CodingScheme::ReedSolomon { n: 20, k: 12 },
                Some(other) => return Err(format!("Unsupported --scheme {:?}", other)),
            };
            let crash_point = match find_flag_value(rest, "--crash-point").as_deref() {
                Some("AfterPreparing") => Some(CrashPoint::AfterPreparing),
                Some("MidWriting") => Some(CrashPoint::MidWriting),
                Some("AfterVerifying") => Some(CrashPoint::AfterVerifying),
                Some("MidCommittingBeforeManifest") => {
                    Some(CrashPoint::MidCommittingBeforeManifest)
                }
                Some(other) => return Err(format!("Unknown --crash-point {:?}", other)),
                None => None,
            };

            Ok(StorageCliCommand::Put {
                input: PathBuf::from(positional),
                root,
                object_id,
                scheme,
                stripe_size,
                crash_point,
            })
        }
        "get" => {
            let object_id = first_positional(rest).ok_or_else(|| {
                "Usage: fsp storage get <object-id> --root <dir> --output <path>".to_string()
            })?;
            let output = find_flag_value(rest, "--output")
                .map(PathBuf::from)
                .ok_or_else(|| "Missing required --output <path> for storage get".to_string())?;
            Ok(StorageCliCommand::Get {
                object_id,
                root,
                output,
            })
        }
        "verify" => {
            let object_id = first_positional(rest)
                .ok_or_else(|| "Usage: fsp storage verify <object-id> --root <dir>".to_string())?;
            Ok(StorageCliCommand::Verify { object_id, root })
        }
        "inspect" => {
            let object_id = first_positional(rest)
                .ok_or_else(|| "Usage: fsp storage inspect <object-id> --root <dir>".to_string())?;
            Ok(StorageCliCommand::Inspect { object_id, root })
        }
        "repair" => {
            let object_id = first_positional(rest)
                .ok_or_else(|| "Usage: fsp storage repair <object-id> --root <dir>".to_string())?;
            Ok(StorageCliCommand::Repair { object_id, root })
        }
        "status" => Ok(StorageCliCommand::Status { root }),
        "recover" => Ok(StorageCliCommand::Recover { root }),
        other => Err(format!("Unknown storage subcommand: {:?}", other)),
    }
}

/// Executes a parsed [`StorageCliCommand`], printing structured output and returning an exit code (`0` on success, non-zero on error).
pub fn execute_storage_cli(cmd: StorageCliCommand) -> Result<(), StorageError> {
    match cmd {
        StorageCliCommand::Init {
            root,
            nodes,
            domains,
        } => {
            let engine = StorageEngine::init(&root, nodes, domains)?;
            let st = engine.status()?;
            println!(
                "Initialized FSP storage root at {} (nodes={}, domains={})",
                st.root_path, st.num_nodes, st.num_domains
            );
            Ok(())
        }
        StorageCliCommand::Put {
            input,
            root,
            object_id,
            scheme,
            stripe_size,
            crash_point,
        } => {
            let mut engine = StorageEngine::open(&root)?;
            let crash_trigger = crash_point.map(|point| CrashTrigger {
                point,
                mode: CrashMode::KillProcess { exit_code: 137 },
            });
            let opts = PutOptions {
                object_id,
                stripe_size,
                coding_scheme: scheme,
                crash_trigger,
                use_container_v2: false,
            };
            let receipt = engine.put_file(&input, &opts)?;
            println!(
                "PUT OK | object_id={} | size={} B | sha256={} | stripes={} | shards={} | disk_written={} B",
                receipt.object_id,
                receipt.object_size,
                receipt.object_digest,
                receipt.stripe_count,
                receipt.total_shards_written,
                receipt.total_disk_bytes_written
            );
            Ok(())
        }
        StorageCliCommand::Get {
            object_id,
            root,
            output,
        } => {
            let engine = StorageEngine::open(&root)?;
            let rep = engine.get_to_file(&object_id, &output)?;
            println!(
                "GET OK | object_id={} | size={} B | sha256={} | stripes={} | shards_read={} | disk_read={} B | output={}",
                rep.object_id,
                rep.object_size,
                rep.object_digest,
                rep.stripes_processed,
                rep.shards_read,
                rep.disk_bytes_read,
                output.display()
            );
            Ok(())
        }
        StorageCliCommand::Verify { object_id, root } => {
            let engine = StorageEngine::open(&root)?;
            let rep = engine.verify_object(&object_id)?;
            println!(
                "VERIFY | object_id={} | size={} B | stripes={} | all_shards_healthy={} | all_stripes_recoverable={}",
                rep.object_id,
                rep.object_size,
                rep.stripes.len(),
                rep.all_shards_healthy,
                rep.all_stripes_recoverable
            );
            for s in &rep.stripes {
                println!(
                    "  stripe {:04}: healthy={}/{} | missing={:?} | corrupt={} | unreachable={} | recoverable={}",
                    s.stripe_index,
                    s.healthy_shards,
                    rep.total_shards_per_stripe,
                    s.missing_shards.iter().map(|id| id.0).collect::<Vec<_>>(),
                    s.corrupted_shards.len(),
                    s.unreachable_shards.len(),
                    s.is_recoverable
                );
            }
            if !rep.all_stripes_recoverable {
                return Err(StorageError::InsufficientValidShards {
                    object_id: rep.object_id,
                    stripe_index: 0,
                    valid_shards: 0,
                    required_shards: rep.required_shards_per_stripe,
                });
            }
            Ok(())
        }
        StorageCliCommand::Inspect { object_id, root } => {
            let engine = StorageEngine::open(&root)?;
            let (manifest, health) = engine.inspect_object(&object_id)?;
            println!("{}", manifest.to_canonical_json());
            println!(
                "HEALTH: all_shards_healthy={}, all_stripes_recoverable={}",
                health.all_shards_healthy, health.all_stripes_recoverable
            );
            Ok(())
        }
        StorageCliCommand::Repair { object_id, root } => {
            let engine = StorageEngine::open(&root)?;
            let rep = engine.repair_object(&object_id)?;
            println!(
                "REPAIR OK | object_id={} | stripes={} | shards_repaired={} (local={}, global={}) | disk_read={} B | disk_written={} B | post_healthy={}",
                rep.object_id,
                rep.stripes_inspected,
                rep.shards_repaired,
                rep.local_group_repairs,
                rep.global_reconstruction_repairs,
                rep.disk_bytes_read,
                rep.disk_bytes_written,
                rep.post_repair_all_healthy
            );
            Ok(())
        }
        StorageCliCommand::Status { root } => {
            let engine = StorageEngine::open(&root)?;
            let st = engine.status()?;
            println!(
                "STATUS | root={} | nodes={}/{} online | domains={} | objects={} | total_shard_files={} | total_shard_bytes={} B",
                st.root_path,
                st.num_online_nodes,
                st.num_nodes,
                st.num_domains,
                st.committed_objects.len(),
                st.total_stored_shard_files,
                st.total_stored_shard_bytes
            );
            for obj in &st.committed_objects {
                println!("  object: {}", obj);
            }
            Ok(())
        }
        StorageCliCommand::Recover { root } => {
            let engine = StorageEngine::open_without_recovery(&root)?;
            let rep = engine.recover()?;
            println!(
                "RECOVER OK | aborted_tx={} | finalized_committed_tx={} | partial_shards_cleaned={} | temp_artifacts_cleaned={}",
                rep.transactions_aborted,
                rep.transactions_finalized_committed,
                rep.partial_shard_files_cleaned,
                rep.temp_artifacts_cleaned
            );
            Ok(())
        }
    }
}

/// Parses top-level `fsp <command> [args...]` arguments.
pub fn parse_fsp_cli_args(args: &[String]) -> Result<FspCliCommand, String> {
    let Some(first) = args.first().map(|s| s.as_str()) else {
        return Err(
            "Missing fsp command (expected: storage|preflight|topology|cluster|emergency-read)"
                .to_string(),
        );
    };

    match first {
        "storage" => {
            let cmd = parse_storage_args(&args[1..])?;
            Ok(FspCliCommand::Storage(cmd))
        }
        "preflight" => {
            let rest = &args[1..];
            let config = find_flag_value(rest, "--config")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/configs/cluster_5host.toml"));
            Ok(FspCliCommand::Preflight { config })
        }
        "topology" => {
            let rest = &args[1..];
            let sub = rest.first().map(|s| s.as_str()).unwrap_or("validate");
            if sub != "validate" {
                return Err(format!(
                    "Unknown topology subcommand: {:?}, expected 'validate'",
                    sub
                ));
            }
            let config = find_flag_value(&rest[1..], "--config")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/configs/cluster_5host.toml"));
            Ok(FspCliCommand::TopologyValidate { config })
        }
        "cluster" => {
            let rest = &args[1..];
            let sub = rest.first().map(|s| s.as_str()).unwrap_or("status");
            if sub != "status" {
                return Err(format!(
                    "Unknown cluster subcommand: {:?}, expected 'status'",
                    sub
                ));
            }
            let config = find_flag_value(&rest[1..], "--config")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/configs/cluster_5host.toml"));
            Ok(FspCliCommand::ClusterStatus { config })
        }
        "emergency-read" => {
            let rest = &args[1..];
            let object_id = find_flag_value(rest, "--id")
                .or_else(|| first_positional(rest))
                .ok_or_else(|| {
                    "Missing required --id <object-id> for emergency-read".to_string()
                })?;
            let config = find_flag_value(rest, "--config")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("deploy/configs/cluster_5host.toml"));
            let output = find_flag_value(rest, "--output")
                .map(PathBuf::from)
                .ok_or_else(|| "Missing required --output <path> for emergency-read".to_string())?;
            Ok(FspCliCommand::EmergencyRead {
                object_id,
                config,
                output,
            })
        }
        other => Err(format!("Unknown fsp command: {:?}", other)),
    }
}

/// Executes preflight configuration validation.
pub fn execute_preflight(config_path: &Path) -> Result<TopologyPreflightReport, String> {
    if !config_path.exists() {
        return Err(format!(
            "Config file does not exist: {}",
            config_path.display()
        ));
    }
    let config = load_coordinator_config_file(config_path, true)
        .map_err(|e| format!("Failed to load configuration: {}", e))?;
    let report = config
        .validate_preflight_topology()
        .map_err(|e| format!("Topology preflight validation failed: {}", e))?;
    Ok(report)
}

/// Executes topology validation for physical host failure domain constraints.
pub fn execute_topology_validate(config_path: &Path) -> Result<TopologyPreflightReport, String> {
    execute_preflight(config_path)
}

/// Generates a summary of configured cluster topology.
pub fn execute_cluster_status(config_path: &Path) -> Result<String, String> {
    let config = load_coordinator_config_file(config_path, true)
        .map_err(|e| format!("Failed to load configuration: {}", e))?;
    let mut status_lines = Vec::new();
    status_lines.push(format!(
        "CLUSTER STATUS | coordinator_id={}",
        config.coordinator_id
    ));
    status_lines.push(format!("Nodes configured: {}", config.nodes.len()));
    let mut host_map = BTreeMap::new();
    for n in &config.nodes {
        *host_map.entry(n.host_domain.clone()).or_insert(0usize) += 1;
    }
    for (host, count) in host_map {
        status_lines.push(format!("  host {}: {} nodes", host, count));
    }
    Ok(status_lines.join("\n"))
}

/// Executes an offline emergency read, directly reconstructing data from surviving storage nodes
/// without requiring coordinator or quorum consensus availability.
pub fn execute_emergency_read(
    object_id: &str,
    storage_roots: &[PathBuf],
    output: &Path,
) -> Result<u64, String> {
    let rs = ReedSolomonCodec::fsp_default_20_12();
    let mut collected_shards: BTreeMap<u32, Vec<u8>> = BTreeMap::new();

    for root in storage_roots {
        let obj_dir = root.join("objects").join(object_id);
        if !obj_dir.exists() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(&obj_dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if let Some(fname) = p.file_name().and_then(|n| n.to_str()) {
                    if fname.ends_with(".fspc") {
                        if let Ok(mut reader) = ShardContainerReader::open(&p) {
                            if let Ok(payload) = reader.read_stripe(0) {
                                let shard_id = reader.header().shard_id as u32;
                                collected_shards.entry(shard_id).or_insert(payload);
                            }
                        }
                    } else if fname.ends_with(".shard") {
                        if let Ok(data) = fs::read(&p) {
                            let shard_num = fname
                                .split('.')
                                .next()
                                .and_then(|s| s.split("shard").nth(1))
                                .and_then(|s| s.parse::<u32>().ok());
                            if let Some(s_id) = shard_num {
                                collected_shards.entry(s_id).or_insert(data);
                            }
                        }
                    }
                }
            }
        }
    }

    if collected_shards.len() < 12 {
        return Err(format!(
            "Insufficient surviving shards for emergency read: found {} < 12 required",
            collected_shards.len()
        ));
    }

    let shard_len = collected_shards
        .values()
        .next()
        .map(|v| v.len())
        .unwrap_or(0);
    if shard_len == 0 {
        return Err("Surviving shards have 0 byte length".into());
    }

    let available: Vec<(ShardId, &[u8])> = collected_shards
        .iter()
        .take(12)
        .map(|(&id, vec)| (ShardId(id as usize), vec.as_slice()))
        .collect();

    let decoded = rs
        .decode_full_stripe(&available, shard_len)
        .map_err(|e| format!("Erasure decoding failure: {}", e))?;

    let mut payload = Vec::new();
    for d in decoded.iter().take(12) {
        payload.extend_from_slice(d);
    }

    write_and_sync_file(output, &payload)
        .map_err(|e| format!("Failed to write reconstructed payload: {}", e))?;

    Ok(payload.len() as u64)
}

fn find_flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut i = 0usize;
    while i < args.len() {
        if args[i] == flag && i + 1 < args.len() {
            return Some(args[i + 1].clone());
        }
        i += 1;
    }
    None
}

fn first_positional(args: &[String]) -> Option<String> {
    let mut i = 0usize;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with("--") {
            i += 2;
            continue;
        }
        return Some(a.clone());
    }
    None
}
