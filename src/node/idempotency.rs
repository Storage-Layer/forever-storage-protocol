//! Persistent On-Disk Node Idempotency Store (`src/node/idempotency.rs`).
//!
//! Provides durable crash-recoverable idempotency tracking for node mutations:
//! - `PutShard`
//! - `RepairShard`
//! - `DeleteShard`
//!
//! Transaction outcomes are persisted to `<node_root>/idempotency/<tx_id>_<shard_id>.rec`
//! with payload digest binding and atomic sync using a simple line-based text format.
//! Duplicate requests with matching payloads return identical replay acknowledgments
//! without re-executing disk operations. Requests with the same transaction ID but
//! conflicting payloads are rejected with `RpcError::IntegrityFailure` (idempotency conflict).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::model::node::NodeId;
use crate::network::protocol::{PutShardAck, RpcError};
use crate::storage::backend::StorageError;
use crate::storage::filesystem::{
    check_path_has_no_traversal, ensure_not_symlink, sync_directory, write_and_sync_file,
};
use crate::storage::integrity::sha256_bytes;
use crate::storage::DurabilityLevel;

/// Stored idempotency record covering a committed or executed transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdempotencyRecord {
    pub tx_id: String,
    pub object_id: String,
    pub shard_id: usize,
    pub version: u64,
    pub payload_sha256: String,
    pub operation: String,
    pub ack: Option<PutShardAck>,
    pub deleted: Option<bool>,
    pub recorded_at_unix_secs: u64,
}

impl IdempotencyRecord {
    /// Serializes this record into a deterministic line-oriented format.
    pub fn encode(&self) -> String {
        let mut out = format!(
            "TX_ID={}\nOBJECT_ID={}\nSHARD_ID={}\nVERSION={}\nPAYLOAD_SHA={}\nOP={}\nRECORDED_AT={}\n",
            self.tx_id,
            self.object_id,
            self.shard_id,
            self.version,
            self.payload_sha256,
            self.operation,
            self.recorded_at_unix_secs
        );
        if let Some(ref ack) = self.ack {
            out.push_str(&format!(
                "ACK_NODE={}\nACK_STRIPES={}\nACK_SIZE={}\nACK_BODY_SHA={}\nACK_HDR_SHA={}\n",
                ack.node_id.0,
                ack.stripe_count,
                ack.container_size_bytes,
                ack.container_body_sha256,
                ack.header_and_index_sha256
            ));
        }
        if let Some(del) = self.deleted {
            out.push_str(&format!("DELETED={}\n", if del { "true" } else { "false" }));
        }
        out
    }

    /// Deserializes a record from a deterministic line-oriented format.
    pub fn decode(s: &str) -> Option<Self> {
        let mut tx_id = None;
        let mut object_id = None;
        let mut shard_id = None;
        let mut version = None;
        let mut payload_sha256 = None;
        let mut operation = None;
        let mut recorded_at_unix_secs = 0u64;
        let mut ack_node = None;
        let mut ack_stripes = None;
        let mut ack_size = None;
        let mut ack_body_sha = None;
        let mut ack_hdr_sha = None;
        let mut deleted = None;

        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let (k, v) = line.split_once('=')?;
            match k {
                "TX_ID" => tx_id = Some(v.to_string()),
                "OBJECT_ID" => object_id = Some(v.to_string()),
                "SHARD_ID" => shard_id = v.parse::<usize>().ok(),
                "VERSION" => version = v.parse::<u64>().ok(),
                "PAYLOAD_SHA" => payload_sha256 = Some(v.to_string()),
                "OP" => operation = Some(v.to_string()),
                "RECORDED_AT" => recorded_at_unix_secs = v.parse::<u64>().unwrap_or(0),
                "ACK_NODE" => ack_node = v.parse::<usize>().ok(),
                "ACK_STRIPES" => ack_stripes = v.parse::<u32>().ok(),
                "ACK_SIZE" => ack_size = v.parse::<u64>().ok(),
                "ACK_BODY_SHA" => ack_body_sha = Some(v.to_string()),
                "ACK_HDR_SHA" => ack_hdr_sha = Some(v.to_string()),
                "DELETED" => deleted = Some(v == "true"),
                _ => {}
            }
        }

        let tx_id = tx_id?;
        let object_id = object_id?;
        let shard_id = shard_id?;
        let version = version?;
        let payload_sha256 = payload_sha256.unwrap_or_default();
        let operation = operation.unwrap_or_else(|| "PutShard".to_string());

        let ack = if let (Some(node), Some(stripes), Some(size), Some(bsha), Some(hsha)) =
            (ack_node, ack_stripes, ack_size, ack_body_sha, ack_hdr_sha)
        {
            Some(PutShardAck {
                node_id: NodeId(node),
                tx_id: tx_id.clone(),
                object_id: object_id.clone(),
                shard_id,
                version,
                stripe_count: stripes,
                container_size_bytes: size,
                container_body_sha256: bsha,
                header_and_index_sha256: hsha,
                idempotent_replay: true,
                durability_level: DurabilityLevel::LocallyDurable,
            })
        } else {
            None
        };

        Some(Self {
            tx_id,
            object_id,
            shard_id,
            version,
            payload_sha256,
            operation,
            ack,
            deleted,
            recorded_at_unix_secs,
        })
    }
}

/// Durable idempotency store managing in-memory fast lookup and on-disk persistent journals.
#[derive(Debug)]
pub struct DurableIdempotencyStore {
    root_dir: PathBuf,
    records: RwLock<BTreeMap<String, IdempotencyRecord>>,
}

impl DurableIdempotencyStore {
    /// Opens or initializes the idempotency directory at `<node_root>/idempotency`.
    pub fn open_or_init<P: AsRef<Path>>(storage_root: P) -> Result<Self, StorageError> {
        let root_dir = storage_root.as_ref().join("idempotency");
        check_path_has_no_traversal(&root_dir)?;
        if root_dir.exists() {
            ensure_not_symlink(&root_dir)?;
        } else {
            fs::create_dir_all(&root_dir)?;
            sync_directory(storage_root.as_ref())?;
        }

        let mut records = BTreeMap::new();
        for entry in fs::read_dir(&root_dir)? {
            let entry = entry?;
            let path = entry.path();
            ensure_not_symlink(&path)?;
            let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if fname.ends_with(".rec") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Some(record) = IdempotencyRecord::decode(&content) {
                        let key = Self::record_key(&record.tx_id, record.shard_id);
                        records.insert(key, record);
                    }
                }
            }
        }

        Ok(Self {
            root_dir,
            records: RwLock::new(records),
        })
    }

    /// Forms a canonical map key for a given `tx_id` and `shard_id`.
    pub fn record_key(tx_id: &str, shard_id: usize) -> String {
        format!("{}:{}", tx_id, shard_id)
    }

    fn file_path_for_key(&self, tx_id: &str, shard_id: usize) -> PathBuf {
        let safe_tx = tx_id.replace(['/', '\\', '.', ' '], "_");
        self.root_dir
            .join(format!("{}_{:02}.rec", safe_tx, shard_id))
    }

    /// Checks whether an operation with `(tx_id, shard_id)` was previously recorded.
    pub fn check_idempotency(
        &self,
        tx_id: &str,
        shard_id: usize,
        expected_object_id: &str,
        expected_version: u64,
        payload_bytes: &[u8],
    ) -> Result<Option<IdempotencyRecord>, RpcError> {
        let key = Self::record_key(tx_id, shard_id);
        let records = self.records.read().unwrap_or_else(|p| p.into_inner());
        if let Some(rec) = records.get(&key) {
            let req_sha = sha256_bytes(payload_bytes).to_hex();
            if rec.object_id != expected_object_id || rec.version != expected_version {
                return Err(RpcError::IntegrityFailure(format!(
                    "idempotency conflict for tx_id {}: existing obj={}/v{}, requested obj={}/v{}",
                    tx_id, rec.object_id, rec.version, expected_object_id, expected_version
                )));
            }
            if !payload_bytes.is_empty() && rec.payload_sha256 != req_sha {
                return Err(RpcError::IntegrityFailure(format!(
                    "idempotency conflict for tx_id {}: existing payload digest {} != requested {}",
                    tx_id, rec.payload_sha256, req_sha
                )));
            }
            return Ok(Some(rec.clone()));
        }
        Ok(None)
    }

    /// Persists a committed transaction idempotency record to disk with `fsync`, then updates RAM.
    #[allow(clippy::too_many_arguments)]
    pub fn record_put_shard(
        &self,
        tx_id: &str,
        object_id: &str,
        shard_id: usize,
        version: u64,
        payload_bytes: &[u8],
        ack: PutShardAck,
        now_unix_secs: u64,
    ) -> Result<(), RpcError> {
        let payload_sha256 = sha256_bytes(payload_bytes).to_hex();
        let record = IdempotencyRecord {
            tx_id: tx_id.to_string(),
            object_id: object_id.to_string(),
            shard_id,
            version,
            payload_sha256,
            operation: "PutShard".to_string(),
            ack: Some(ack),
            deleted: None,
            recorded_at_unix_secs: now_unix_secs,
        };

        let file_path = self.file_path_for_key(tx_id, shard_id);
        let encoded = record.encode();

        write_and_sync_file(&file_path, encoded.as_bytes())
            .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        sync_directory(&self.root_dir).map_err(|e| RpcError::InternalNodeError(e.to_string()))?;

        let key = Self::record_key(tx_id, shard_id);
        self.records
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, record);

        Ok(())
    }

    /// Persists a committed delete transaction idempotency record.
    pub fn record_delete_shard(
        &self,
        tx_id: &str,
        object_id: &str,
        shard_id: usize,
        version: u64,
        deleted: bool,
        now_unix_secs: u64,
    ) -> Result<(), RpcError> {
        let record = IdempotencyRecord {
            tx_id: tx_id.to_string(),
            object_id: object_id.to_string(),
            shard_id,
            version,
            payload_sha256: String::new(),
            operation: "DeleteShard".to_string(),
            ack: None,
            deleted: Some(deleted),
            recorded_at_unix_secs: now_unix_secs,
        };

        let file_path = self.file_path_for_key(tx_id, shard_id);
        let encoded = record.encode();

        write_and_sync_file(&file_path, encoded.as_bytes())
            .map_err(|e| RpcError::InternalNodeError(e.to_string()))?;
        sync_directory(&self.root_dir).map_err(|e| RpcError::InternalNodeError(e.to_string()))?;

        let key = Self::record_key(tx_id, shard_id);
        self.records
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, record);

        Ok(())
    }

    /// Returns the number of persisted idempotency records.
    pub fn record_count(&self) -> usize {
        self.records.read().unwrap_or_else(|p| p.into_inner()).len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_durable_idempotency_store_lifecycle() {
        let tmp = env::temp_dir().join(format!("fsp_idemp_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        let store = DurableIdempotencyStore::open_or_init(&tmp).unwrap();

        let ack = PutShardAck {
            node_id: NodeId(1),
            tx_id: "tx-1".to_string(),
            object_id: "obj-1".to_string(),
            shard_id: 0,
            version: 1,
            stripe_count: 1,
            container_size_bytes: 1024,
            container_body_sha256: "body-sha".to_string(),
            header_and_index_sha256: "hdr-sha".to_string(),
            idempotent_replay: false,
            durability_level: DurabilityLevel::LocallyDurable,
        };

        let payload = b"hello world data payload";
        store
            .record_put_shard("tx-1", "obj-1", 0, 1, payload, ack.clone(), 1000)
            .unwrap();

        // 1. Same payload returns idempotent record
        let res = store
            .check_idempotency("tx-1", 0, "obj-1", 1, payload)
            .unwrap();
        assert!(res.is_some());
        let rec = res.unwrap();
        assert_eq!(rec.ack.unwrap().container_size_bytes, 1024);

        // 2. Conflicting payload is rejected
        let conflict = store.check_idempotency("tx-1", 0, "obj-1", 1, b"different payload");
        assert!(conflict.is_err());

        // 3. Restart recovery from disk
        drop(store);
        let store2 = DurableIdempotencyStore::open_or_init(&tmp).unwrap();
        assert_eq!(store2.record_count(), 1);
        let res2 = store2
            .check_idempotency("tx-1", 0, "obj-1", 1, payload)
            .unwrap();
        assert!(res2.is_some());

        let _ = fs::remove_dir_all(&tmp);
    }
}
