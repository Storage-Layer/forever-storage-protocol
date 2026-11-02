//! Strict TOML Configuration File Loader (`src/config/loader.rs`).
//!
//! Provides a zero-dependency, strict parser for FSP deployment configurations.
//! Enforces unknown field rejection in strict mode and populates validated structures.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::validation::{
    ConfigValidationError, DeployCoordinatorConfig, DeployNodeConfig, DeployNodeEntry,
};

/// A parsed section of key-value pairs from a TOML file.
#[derive(Debug, Default, Clone)]
pub struct RawTomlSection {
    pub fields: BTreeMap<String, String>,
}

/// A parsed TOML document structure.
#[derive(Debug, Default)]
pub struct RawTomlDoc {
    pub sections: BTreeMap<String, RawTomlSection>,
    pub table_arrays: BTreeMap<String, Vec<RawTomlSection>>,
}

impl RawTomlDoc {
    /// Parses raw TOML text into structured sections.
    pub fn parse(content: &str) -> Result<Self, ConfigValidationError> {
        let mut doc = Self::default();
        let mut cur_section: Option<String> = None;
        let mut cur_array: Option<String> = None;

        for (line_no, line) in content.lines().enumerate() {
            let mut l = line.trim();
            // Strip comments
            if let Some(idx) = l.find('#') {
                l = l[..idx].trim();
            }
            if l.is_empty() {
                continue;
            }

            // Check array of tables `[[table_name]]`
            if l.starts_with("[[") && l.ends_with("]]") {
                let name = l[2..l.len() - 2].trim().to_string();
                cur_array = Some(name.clone());
                cur_section = None;
                doc.table_arrays
                    .entry(name)
                    .or_default()
                    .push(RawTomlSection::default());
                continue;
            }

            // Check standard table `[section_name]`
            if l.starts_with('[') && l.ends_with(']') {
                let name = l[1..l.len() - 1].trim().to_string();
                cur_section = Some(name.clone());
                cur_array = None;
                doc.sections.entry(name).or_default();
                continue;
            }

            // Parse `key = value`
            if let Some((k, v)) = l.split_once('=') {
                let key = k.trim().to_string();
                let mut val = v.trim().to_string();

                // Unquote string if enclosed in double quotes
                if val.starts_with('"') && val.ends_with('"') && val.len() >= 2 {
                    val = val[1..val.len() - 1].to_string();
                }

                if let Some(ref arr_name) = cur_array {
                    let entries = doc.table_arrays.get_mut(arr_name).unwrap();
                    let last = entries.last_mut().ok_or_else(|| {
                        ConfigValidationError::SyntaxError(format!(
                            "line {}: empty array entry",
                            line_no + 1
                        ))
                    })?;
                    last.fields.insert(key, val);
                } else if let Some(ref sec_name) = cur_section {
                    let sec = doc.sections.get_mut(sec_name).unwrap();
                    sec.fields.insert(key, val);
                } else {
                    return Err(ConfigValidationError::SyntaxError(format!(
                        "line {}: key-value pair outside of any section: {}",
                        line_no + 1,
                        l
                    )));
                }
            } else {
                return Err(ConfigValidationError::SyntaxError(format!(
                    "line {}: malformed TOML line: {}",
                    line_no + 1,
                    l
                )));
            }
        }

        Ok(doc)
    }

    /// Validates that a section contains only allowed fields.
    pub fn verify_no_unknown_fields(
        &self,
        section_name: &str,
        allowed_keys: &[&str],
    ) -> Result<(), ConfigValidationError> {
        if let Some(sec) = self.sections.get(section_name) {
            let allowed: BTreeSet<&str> = allowed_keys.iter().copied().collect();
            for k in sec.fields.keys() {
                if !allowed.contains(k.as_str()) {
                    return Err(ConfigValidationError::UnknownField(format!(
                        "{}.{}",
                        section_name, k
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Parses and validates a coordinator configuration from TOML text.
pub fn parse_coordinator_config(
    toml_str: &str,
    strict_unknown_check: bool,
) -> Result<DeployCoordinatorConfig, ConfigValidationError> {
    let doc = RawTomlDoc::parse(toml_str)?;

    if strict_unknown_check {
        doc.verify_no_unknown_fields(
            "coordinator",
            &[
                "coordinator_id",
                "metadata_root",
                "placement_policy",
                "max_shards_per_host",
                "max_shards_per_rack",
                "default_coding_scheme",
                "default_stripe_size_bytes",
                "degraded_commit_policy",
            ],
        )?;
        doc.verify_no_unknown_fields(
            "security",
            &[
                "trust_environment_mode",
                "ca_cert_pem",
                "coordinator_cert_pem",
                "coordinator_key_pem",
                "ed25519_manifest_signer_key_file",
            ],
        )?;
        doc.verify_no_unknown_fields(
            "repair_and_scrub",
            &[
                "heartbeat_interval_secs",
                "scrub_max_verifications_per_cycle",
                "effective_repair_bandwidth_bps",
            ],
        )?;
    }

    let coord = doc
        .sections
        .get("coordinator")
        .ok_or(ConfigValidationError::MissingField("[coordinator]"))?;
    let sec = doc
        .sections
        .get("security")
        .ok_or(ConfigValidationError::MissingField("[security]"))?;
    let repair = doc
        .sections
        .get("repair_and_scrub")
        .ok_or(ConfigValidationError::MissingField("[repair_and_scrub]"))?;

    let get_val = |map: &BTreeMap<String, String>,
                   k: &'static str|
     -> Result<String, ConfigValidationError> {
        map.get(k)
            .cloned()
            .ok_or(ConfigValidationError::MissingField(k))
    };

    let coordinator_id = get_val(&coord.fields, "coordinator_id")?;
    let metadata_root = PathBuf::from(get_val(&coord.fields, "metadata_root")?);
    let placement_policy = get_val(&coord.fields, "placement_policy")?;
    let max_shards_per_host = get_val(&coord.fields, "max_shards_per_host")?
        .parse::<usize>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "max_shards_per_host",
            reason: e.to_string(),
        })?;
    let max_shards_per_rack = get_val(&coord.fields, "max_shards_per_rack")?
        .parse::<usize>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "max_shards_per_rack",
            reason: e.to_string(),
        })?;
    let default_coding_scheme = get_val(&coord.fields, "default_coding_scheme")?;
    let default_stripe_size_bytes = get_val(&coord.fields, "default_stripe_size_bytes")?
        .parse::<usize>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "default_stripe_size_bytes",
            reason: e.to_string(),
        })?;
    let degraded_commit_policy = get_val(&coord.fields, "degraded_commit_policy")?;

    let ca_cert_pem = PathBuf::from(get_val(&sec.fields, "ca_cert_pem")?);
    let coordinator_cert_pem = PathBuf::from(get_val(&sec.fields, "coordinator_cert_pem")?);
    let coordinator_key_pem = PathBuf::from(get_val(&sec.fields, "coordinator_key_pem")?);
    let manifest_signer_key_file =
        PathBuf::from(get_val(&sec.fields, "ed25519_manifest_signer_key_file")?);

    let heartbeat_interval_secs = get_val(&repair.fields, "heartbeat_interval_secs")?
        .parse::<u64>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "heartbeat_interval_secs",
            reason: e.to_string(),
        })?;
    let scrub_max_verifications_per_cycle =
        get_val(&repair.fields, "scrub_max_verifications_per_cycle")?
            .parse::<usize>()
            .map_err(|e| ConfigValidationError::InvalidValue {
                field: "scrub_max_verifications_per_cycle",
                reason: e.to_string(),
            })?;
    let effective_repair_bandwidth_bps = get_val(&repair.fields, "effective_repair_bandwidth_bps")?
        .parse::<u64>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "effective_repair_bandwidth_bps",
            reason: e.to_string(),
        })?;

    let mut nodes = Vec::new();
    if let Some(node_list) = doc.table_arrays.get("nodes") {
        for n in node_list {
            let nid = get_val(&n.fields, "node_id")?
                .parse::<usize>()
                .map_err(|e| ConfigValidationError::InvalidValue {
                    field: "nodes.node_id",
                    reason: e.to_string(),
                })?;
            let entry = DeployNodeEntry {
                node_id: nid,
                identity_id: get_val(&n.fields, "identity_id")?,
                listen_addr: get_val(&n.fields, "listen_addr")?,
                host_domain: get_val(&n.fields, "host_domain")?,
                disk_domain: get_val(&n.fields, "disk_domain")?,
                rack_domain: get_val(&n.fields, "rack_domain")?,
                region_domain: get_val(&n.fields, "region_domain")?,
            };
            nodes.push(entry);
        }
    }

    let config = DeployCoordinatorConfig {
        coordinator_id,
        metadata_root,
        placement_policy,
        max_shards_per_host,
        max_shards_per_rack,
        default_coding_scheme,
        default_stripe_size_bytes,
        degraded_commit_policy,
        ca_cert_pem,
        coordinator_cert_pem,
        coordinator_key_pem,
        manifest_signer_key_file,
        heartbeat_interval_secs,
        scrub_max_verifications_per_cycle,
        effective_repair_bandwidth_bps,
        nodes,
    };

    config.validate()?;
    Ok(config)
}

/// Parses and validates a storage node configuration from TOML text.
pub fn parse_node_config(
    toml_str: &str,
    strict_unknown_check: bool,
) -> Result<DeployNodeConfig, ConfigValidationError> {
    let doc = RawTomlDoc::parse(toml_str)?;

    if strict_unknown_check {
        doc.verify_no_unknown_fields(
            "node",
            &[
                "node_id",
                "identity_id",
                "listen_addr",
                "storage_root",
                "capacity_limit_bytes",
            ],
        )?;
        doc.verify_no_unknown_fields(
            "topology",
            &["host_domain", "disk_domain", "rack_domain", "region_domain"],
        )?;
        doc.verify_no_unknown_fields(
            "tls",
            &[
                "trust_environment_mode",
                "ca_cert_pem",
                "node_cert_pem",
                "node_key_pem",
                "socket_timeout_secs",
                "max_rpc_payload_bytes",
            ],
        )?;
    }

    let node = doc
        .sections
        .get("node")
        .ok_or(ConfigValidationError::MissingField("[node]"))?;
    let topo = doc
        .sections
        .get("topology")
        .ok_or(ConfigValidationError::MissingField("[topology]"))?;
    let tls = doc
        .sections
        .get("tls")
        .ok_or(ConfigValidationError::MissingField("[tls]"))?;

    let get_val = |map: &BTreeMap<String, String>,
                   k: &'static str|
     -> Result<String, ConfigValidationError> {
        map.get(k)
            .cloned()
            .ok_or(ConfigValidationError::MissingField(k))
    };

    let node_id = get_val(&node.fields, "node_id")?
        .parse::<usize>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "node_id",
            reason: e.to_string(),
        })?;
    let identity_id = get_val(&node.fields, "identity_id")?;
    let listen_addr = get_val(&node.fields, "listen_addr")?;
    let storage_root = PathBuf::from(get_val(&node.fields, "storage_root")?);
    let capacity_limit_bytes = get_val(&node.fields, "capacity_limit_bytes")?
        .parse::<u64>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "capacity_limit_bytes",
            reason: e.to_string(),
        })?;

    let host_domain = get_val(&topo.fields, "host_domain")?;
    let disk_domain = get_val(&topo.fields, "disk_domain")?;
    let rack_domain = get_val(&topo.fields, "rack_domain")?;
    let region_domain = get_val(&topo.fields, "region_domain")?;

    let ca_cert_pem = PathBuf::from(get_val(&tls.fields, "ca_cert_pem")?);
    let node_cert_pem = PathBuf::from(get_val(&tls.fields, "node_cert_pem")?);
    let node_key_pem = PathBuf::from(get_val(&tls.fields, "node_key_pem")?);
    let socket_timeout_secs = get_val(&tls.fields, "socket_timeout_secs")?
        .parse::<u64>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "socket_timeout_secs",
            reason: e.to_string(),
        })?;
    let max_rpc_payload_bytes = get_val(&tls.fields, "max_rpc_payload_bytes")?
        .parse::<u32>()
        .map_err(|e| ConfigValidationError::InvalidValue {
            field: "max_rpc_payload_bytes",
            reason: e.to_string(),
        })?;

    let config = DeployNodeConfig {
        node_id,
        identity_id,
        listen_addr,
        storage_root,
        capacity_limit_bytes,
        host_domain,
        disk_domain,
        rack_domain,
        region_domain,
        ca_cert_pem,
        node_cert_pem,
        node_key_pem,
        socket_timeout_secs,
        max_rpc_payload_bytes,
    };

    config.validate()?;
    Ok(config)
}

/// Loads and validates a coordinator configuration directly from a file path.
pub fn load_coordinator_config_file<P: AsRef<Path>>(
    path: P,
    strict_check: bool,
) -> Result<DeployCoordinatorConfig, ConfigValidationError> {
    let content = fs::read_to_string(path).map_err(|e| ConfigValidationError::Io(e.to_string()))?;
    parse_coordinator_config(&content, strict_check)
}

/// Loads and validates a storage node configuration directly from a file path.
pub fn load_node_config_file<P: AsRef<Path>>(
    path: P,
    strict_check: bool,
) -> Result<DeployNodeConfig, ConfigValidationError> {
    let content = fs::read_to_string(path).map_err(|e| ConfigValidationError::Io(e.to_string()))?;
    parse_node_config(&content, strict_check)
}
