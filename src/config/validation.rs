//! Deployment Configuration Validation and Data Structures (`src/config/validation.rs`).
//!
//! Provides strictly validated configuration structures for physical multi-host coordinator
//! and storage node daemon deployments.

use std::fmt;
use std::path::PathBuf;

/// Errors encountered while parsing or validating deployment configuration files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValidationError {
    Io(String),
    SyntaxError(String),
    MissingField(&'static str),
    UnknownField(String),
    InvalidValue { field: &'static str, reason: String },
}

impl fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "Configuration I/O error: {}", msg),
            Self::SyntaxError(msg) => write!(f, "Configuration syntax error: {}", msg),
            Self::MissingField(field) => {
                write!(f, "Missing mandatory configuration field: {}", field)
            }
            Self::UnknownField(field) => {
                write!(f, "Strict validation rejected unknown field: {}", field)
            }
            Self::InvalidValue { field, reason } => {
                write!(f, "Invalid value for {}: {}", field, reason)
            }
        }
    }
}

impl std::error::Error for ConfigValidationError {}

/// Configured storage node entry in a coordinator's cluster configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployNodeEntry {
    pub node_id: usize,
    pub identity_id: String,
    pub listen_addr: String,
    pub host_domain: String,
    pub disk_domain: String,
    pub rack_domain: String,
    pub region_domain: String,
}

/// Strictly validated coordinator configuration (`deploy/coordinator.example.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployCoordinatorConfig {
    pub coordinator_id: String,
    pub metadata_root: PathBuf,
    pub placement_policy: String,
    pub max_shards_per_host: usize,
    pub max_shards_per_rack: usize,
    pub default_coding_scheme: String,
    pub default_stripe_size_bytes: usize,
    pub degraded_commit_policy: String,

    pub ca_cert_pem: PathBuf,
    pub coordinator_cert_pem: PathBuf,
    pub coordinator_key_pem: PathBuf,
    pub manifest_signer_key_file: PathBuf,

    pub heartbeat_interval_secs: u64,
    pub scrub_max_verifications_per_cycle: usize,
    pub effective_repair_bandwidth_bps: u64,

    pub nodes: Vec<DeployNodeEntry>,
}

impl DeployCoordinatorConfig {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.coordinator_id.trim().is_empty() {
            return Err(ConfigValidationError::MissingField(
                "coordinator.coordinator_id",
            ));
        }
        if self.max_shards_per_host == 0 {
            return Err(ConfigValidationError::InvalidValue {
                field: "coordinator.max_shards_per_host",
                reason: "must be > 0".into(),
            });
        }
        if self.max_shards_per_rack == 0 {
            return Err(ConfigValidationError::InvalidValue {
                field: "coordinator.max_shards_per_rack",
                reason: "must be > 0".into(),
            });
        }
        if self.default_stripe_size_bytes == 0 || self.default_stripe_size_bytes > 64 * 1024 * 1024
        {
            return Err(ConfigValidationError::InvalidValue {
                field: "coordinator.default_stripe_size_bytes",
                reason: "must be between 1 and 64 MiB".into(),
            });
        }
        if self.nodes.is_empty() {
            return Err(ConfigValidationError::InvalidValue {
                field: "nodes",
                reason: "cluster must define at least 1 node".into(),
            });
        }
        for node in &self.nodes {
            if node.identity_id.trim().is_empty() {
                return Err(ConfigValidationError::InvalidValue {
                    field: "nodes.identity_id",
                    reason: "cannot be empty".into(),
                });
            }
            if node.listen_addr.trim().is_empty() {
                return Err(ConfigValidationError::InvalidValue {
                    field: "nodes.listen_addr",
                    reason: "cannot be empty".into(),
                });
            }
        }
        Ok(())
    }

    /// Preflight validation of cluster failure domain topology for RS(20,12) 2-host failure tolerance.
    pub fn validate_preflight_topology(
        &self,
    ) -> Result<TopologyPreflightReport, ConfigValidationError> {
        self.validate()?;

        let mut hosts = std::collections::BTreeSet::new();
        let mut racks = std::collections::BTreeSet::new();
        let mut host_counts = std::collections::BTreeMap::new();

        for node in &self.nodes {
            hosts.insert(node.host_domain.clone());
            racks.insert(node.rack_domain.clone());
            *host_counts
                .entry(node.host_domain.clone())
                .or_insert(0usize) += 1;
        }

        let max_nodes_per_host = host_counts.values().copied().max().unwrap_or(0);
        let total_nodes = self.nodes.len();

        // Calculate maximum loss under any 2 hosts failing
        let mut counts: Vec<usize> = host_counts.values().copied().collect();
        counts.sort_by(|a, b| b.cmp(a));
        let max_two_host_loss = counts.iter().take(2).sum::<usize>();
        let min_surviving = total_nodes.saturating_sub(max_two_host_loss);
        let required_k = 12usize;

        if total_nodes < 20 {
            return Err(ConfigValidationError::InvalidValue {
                field: "nodes",
                reason: format!(
                    "insufficient nodes for RS(20,12): found {}, required >= 20",
                    total_nodes
                ),
            });
        }

        if hosts.len() < 5 {
            return Err(ConfigValidationError::InvalidValue {
                field: "nodes.host_domain",
                reason: format!(
                    "insufficient distinct hosts: found {}, required >= 5",
                    hosts.len()
                ),
            });
        }

        if min_surviving < required_k {
            return Err(ConfigValidationError::InvalidValue {
                field: "max_shards_per_host",
                reason: format!(
                    "2-host failure tolerance violated: worst-case surviving shards {} < required k {}",
                    min_surviving, required_k
                ),
            });
        }

        Ok(TopologyPreflightReport {
            total_nodes,
            distinct_hosts: hosts.len(),
            distinct_racks: racks.len(),
            max_nodes_per_host,
            max_two_host_loss_shards: max_two_host_loss,
            min_surviving_shards: min_surviving,
            required_k,
            fault_tolerance_verified: true,
        })
    }
}

/// Preflight topology validation report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyPreflightReport {
    pub total_nodes: usize,
    pub distinct_hosts: usize,
    pub distinct_racks: usize,
    pub max_nodes_per_host: usize,
    pub max_two_host_loss_shards: usize,
    pub min_surviving_shards: usize,
    pub required_k: usize,
    pub fault_tolerance_verified: bool,
}

/// Strictly validated storage node configuration (`deploy/node.example.toml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployNodeConfig {
    pub node_id: usize,
    pub identity_id: String,
    pub listen_addr: String,
    pub storage_root: PathBuf,
    pub capacity_limit_bytes: u64,

    pub host_domain: String,
    pub disk_domain: String,
    pub rack_domain: String,
    pub region_domain: String,

    pub ca_cert_pem: PathBuf,
    pub node_cert_pem: PathBuf,
    pub node_key_pem: PathBuf,
    pub socket_timeout_secs: u64,
    pub max_rpc_payload_bytes: u32,
}

impl DeployNodeConfig {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.identity_id.trim().is_empty() {
            return Err(ConfigValidationError::MissingField("node.identity_id"));
        }
        if self.listen_addr.trim().is_empty() {
            return Err(ConfigValidationError::MissingField("node.listen_addr"));
        }
        if self.capacity_limit_bytes == 0 {
            return Err(ConfigValidationError::InvalidValue {
                field: "node.capacity_limit_bytes",
                reason: "must be > 0".into(),
            });
        }
        if self.socket_timeout_secs == 0 {
            return Err(ConfigValidationError::InvalidValue {
                field: "tls.socket_timeout_secs",
                reason: "must be > 0".into(),
            });
        }
        if self.max_rpc_payload_bytes == 0 || self.max_rpc_payload_bytes > 128 * 1024 * 1024 {
            return Err(ConfigValidationError::InvalidValue {
                field: "tls.max_rpc_payload_bytes",
                reason: "must be between 1 and 128 MiB".into(),
            });
        }
        Ok(())
    }
}
