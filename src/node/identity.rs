//! Persistent storage node identity, certificate fingerprint binding, and multi-level failure
//! domain metadata (`src/node/identity.rs`).

use std::fs;
use std::path::Path;

use crate::model::node::NodeId;
use crate::storage::backend::StorageError;
use crate::storage::filesystem::write_and_sync_file;
use crate::storage::manifest::validate_safe_identifier;

/// Multi-level physical/logical failure-domain topology metadata for a storage node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeFailureDomainTopology {
    /// Logical/process node identifier (`"node-00"`).
    pub node_label: String,
    /// Physical host / machine identifier (`"localhost"` or `"host-01.dc1"`).
    pub physical_host: String,
    /// Underlying block device / disk volume identifier (`"disk-00"` or `"/dev/sdb3"`).
    pub disk_volume: String,
    /// Rack or availability zone group (`"rack-00"`).
    pub rack_group: String,
    /// Geographic region (`"region-local"` or `"eu-central-1"`).
    pub region: String,
}

impl NodeFailureDomainTopology {
    /// Topology for 20 logical daemons running on a single physical host and shared disk.
    pub fn single_host_local_daemon(node_id: NodeId) -> Self {
        Self {
            node_label: format!("node-{:02}", node_id.0),
            physical_host: "localhost-single-machine".to_string(),
            disk_volume: "shared-local-ext4".to_string(),
            rack_group: format!("logical-rack-{:02}", node_id.0 % 4),
            region: "region-local".to_string(),
        }
    }

    /// Topology for multi-host deployment with distinct physical hosts and disks.
    pub fn distinct_physical_host(
        node_id: NodeId,
        host: &str,
        disk: &str,
        rack: &str,
        region: &str,
    ) -> Self {
        Self {
            node_label: format!("node-{:02}", node_id.0),
            physical_host: host.to_string(),
            disk_volume: disk.to_string(),
            rack_group: rack.to_string(),
            region: region.to_string(),
        }
    }
}

/// Persistent identity record stored at `<node_root>/identity/node_identity.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentNodeIdentity {
    pub node_id: NodeId,
    pub identity_id: String,
    pub cert_fingerprint_hex: String,
    pub topology: NodeFailureDomainTopology,
}

impl PersistentNodeIdentity {
    /// Serializes this identity record to deterministic JSON.
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                "{{\n",
                "  \"node_id\": {},\n",
                "  \"identity_id\": \"{}\",\n",
                "  \"cert_fingerprint_hex\": \"{}\",\n",
                "  \"node_label\": \"{}\",\n",
                "  \"physical_host\": \"{}\",\n",
                "  \"disk_volume\": \"{}\",\n",
                "  \"rack_group\": \"{}\",\n",
                "  \"region\": \"{}\"\n",
                "}}\n"
            ),
            self.node_id.0,
            self.identity_id,
            self.cert_fingerprint_hex,
            self.topology.node_label,
            self.topology.physical_host,
            self.topology.disk_volume,
            self.topology.rack_group,
            self.topology.region,
        )
    }

    /// Parses a [`PersistentNodeIdentity`] from JSON.
    pub fn from_json(json: &str) -> Result<Self, StorageError> {
        let mut kv: Vec<(&str, &str)> = Vec::new();
        for line in json.lines() {
            let trimmed = line.trim().trim_end_matches(',');
            if let Some((k, v)) = trimmed.split_once(':') {
                let key = k.trim().trim_matches('"');
                let val = v.trim().trim_matches('"');
                kv.push((key, val));
            }
        }
        let get = |k: &str| -> Result<&str, StorageError> {
            kv.iter()
                .find(|&&(key, _)| key == k)
                .map(|&(_, v)| v)
                .ok_or_else(|| {
                    StorageError::Authentication(format!("missing node identity field {}", k))
                })
        };

        let node_id_num = get("node_id")?
            .parse::<usize>()
            .map_err(|e| StorageError::Authentication(e.to_string()))?;
        let identity_id = get("identity_id")?.to_string();
        if !validate_safe_identifier(&identity_id) {
            return Err(StorageError::Authentication(format!(
                "unsafe node identity_id {:?}",
                identity_id
            )));
        }
        let cert_fingerprint_hex = get("cert_fingerprint_hex")?.to_string();
        let topology = NodeFailureDomainTopology {
            node_label: get("node_label")?.to_string(),
            physical_host: get("physical_host")?.to_string(),
            disk_volume: get("disk_volume")?.to_string(),
            rack_group: get("rack_group")?.to_string(),
            region: get("region")?.to_string(),
        };

        Ok(Self {
            node_id: NodeId(node_id_num),
            identity_id,
            cert_fingerprint_hex,
            topology,
        })
    }

    /// Saves or verifies the persistent identity at `<node_root>/identity/node_identity.json`.
    pub fn save_or_verify(&self, node_root: &Path) -> Result<(), StorageError> {
        let id_dir = node_root.join("identity");
        fs::create_dir_all(&id_dir)?;
        let path = id_dir.join("node_identity.json");
        if path.exists() {
            let existing = Self::from_json(&fs::read_to_string(&path)?)?;
            if existing.node_id != self.node_id || existing.identity_id != self.identity_id {
                return Err(StorageError::Authentication(format!(
                    "persistent node identity mismatch at {}: on-disk ({}, {}) != configured ({}, {})",
                    path.display(),
                    existing.node_id.0,
                    existing.identity_id,
                    self.node_id.0,
                    self.identity_id
                )));
            }
        }
        write_and_sync_file(&path, self.to_json().as_bytes())
    }
}
