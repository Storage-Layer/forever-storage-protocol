//! Durable Metadata Replication and State Machine Replay (`src/coordinator/replication.rs`).
//!
//! Provides an append-only Write-Ahead Log (`metadata_replication.wal`) for replicating
//! committed manifests, monotonic generation watermarks, and repair journal states from the
//! active coordinator to standby coordinator replicas.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::coordinator::leadership::CoordinatorId;
use crate::coordinator::repair_journal::RepairJobState;
use crate::model::node::NodeId;
use crate::storage::backend::StorageError;
use crate::storage::filesystem::sync_directory;
use crate::storage::integrity::sha256_bytes;

/// Record in the metadata replication write-ahead log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplicationRecord {
    ManifestCommitted {
        object_id: String,
        generation: u64,
        manifest_json_or_text: String,
        manifest_sha256_hex: String,
    },
    GenerationWatermarkAdvanced {
        object_id: String,
        generation: u64,
    },
    RepairJobStateChanged {
        job_id: String,
        state: RepairJobState,
        target_node: Option<NodeId>,
        error_msg: Option<String>,
        updated_unix_secs: u64,
    },
    NodeRegistered {
        node_id: NodeId,
        listen_addr: String,
        host_domain: String,
        rack_domain: String,
        region_domain: String,
    },
}

impl ReplicationRecord {
    /// Serializes a replication record into a delimited text block.
    pub fn encode(&self) -> String {
        match self {
            Self::ManifestCommitted {
                object_id,
                generation,
                manifest_json_or_text,
                manifest_sha256_hex,
            } => {
                format!(
                    "REC=ManifestCommitted\nobject_id={}\ngen={}\nsha256={}\nbody={}\nENDREC\n",
                    object_id, generation, manifest_sha256_hex, manifest_json_or_text
                )
            }
            Self::GenerationWatermarkAdvanced {
                object_id,
                generation,
            } => {
                format!(
                    "REC=GenerationWatermarkAdvanced\nobject_id={}\ngen={}\nENDREC\n",
                    object_id, generation
                )
            }
            Self::RepairJobStateChanged {
                job_id,
                state,
                target_node,
                error_msg,
                updated_unix_secs,
            } => {
                let target_str = target_node
                    .map(|n| n.0.to_string())
                    .unwrap_or_else(|| "NONE".to_string());
                let err_str = error_msg.as_deref().unwrap_or("NONE");
                format!(
                    "REC=RepairJobStateChanged\njob_id={}\nstate={:?}\ntarget={}\nerr={}\nts={}\nENDREC\n",
                    job_id, state, target_str, err_str, updated_unix_secs
                )
            }
            Self::NodeRegistered {
                node_id,
                listen_addr,
                host_domain,
                rack_domain,
                region_domain,
            } => {
                format!(
                    "REC=NodeRegistered\nnode_id={}\naddr={}\nhost={}\nrack={}\nregion={}\nENDREC\n",
                    node_id.0, listen_addr, host_domain, rack_domain, region_domain
                )
            }
        }
    }

    /// Parses a single record block from key-value pairs.
    pub fn decode_block(lines: &[String]) -> Result<Self, StorageError> {
        let mut kv = std::collections::BTreeMap::new();
        let mut rec_type = String::new();
        for line in lines {
            if let Some(r) = line.strip_prefix("REC=") {
                rec_type = r.to_string();
            } else if let Some((k, v)) = line.split_once('=') {
                kv.insert(k.to_string(), v.to_string());
            }
        }

        match rec_type.as_str() {
            "ManifestCommitted" => {
                let object_id = kv.get("object_id").cloned().unwrap_or_default();
                let gen = kv
                    .get("gen")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                let sha256 = kv.get("sha256").cloned().unwrap_or_default();
                let body = kv.get("body").cloned().unwrap_or_default();
                Ok(Self::ManifestCommitted {
                    object_id,
                    generation: gen,
                    manifest_json_or_text: body,
                    manifest_sha256_hex: sha256,
                })
            }
            "GenerationWatermarkAdvanced" => {
                let object_id = kv.get("object_id").cloned().unwrap_or_default();
                let gen = kv
                    .get("gen")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                Ok(Self::GenerationWatermarkAdvanced {
                    object_id,
                    generation: gen,
                })
            }
            "RepairJobStateChanged" => {
                let job_id = kv.get("job_id").cloned().unwrap_or_default();
                let state_str = kv.get("state").cloned().unwrap_or_default();
                let state = match state_str.as_str() {
                    "Pending" => RepairJobState::Pending,
                    "CapacityCheck" => RepairJobState::CapacityCheck,
                    "Scheduled" => RepairJobState::Scheduled,
                    "Repairing" => RepairJobState::Repairing,
                    "Verifying" => RepairJobState::Verifying,
                    "Completed" => RepairJobState::Completed,
                    "WaitingForCapacity" => RepairJobState::WaitingForCapacity,
                    "RetryPending" => RepairJobState::RetryPending,
                    "Cancelled" => RepairJobState::Cancelled,
                    _ => RepairJobState::Pending,
                };
                let target_node = kv
                    .get("target")
                    .filter(|s| *s != "NONE")
                    .and_then(|s| s.parse::<usize>().ok())
                    .map(NodeId);
                let error_msg = kv.get("err").filter(|s| *s != "NONE").cloned();
                let ts = kv
                    .get("ts")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                Ok(Self::RepairJobStateChanged {
                    job_id,
                    state,
                    target_node,
                    error_msg,
                    updated_unix_secs: ts,
                })
            }
            "NodeRegistered" => {
                let node_id = kv
                    .get("node_id")
                    .and_then(|s| s.parse::<usize>().ok())
                    .map(NodeId)
                    .unwrap_or(NodeId(0));
                let addr = kv.get("addr").cloned().unwrap_or_default();
                let host = kv.get("host").cloned().unwrap_or_default();
                let rack = kv.get("rack").cloned().unwrap_or_default();
                let region = kv.get("region").cloned().unwrap_or_default();
                Ok(Self::NodeRegistered {
                    node_id,
                    listen_addr: addr,
                    host_domain: host,
                    rack_domain: rack,
                    region_domain: region,
                })
            }
            other => Err(StorageError::CorruptMetadata(format!(
                "unknown replication record type {}",
                other
            ))),
        }
    }
}

/// Append-only Write-Ahead Log for metadata replication.
#[derive(Debug)]
pub struct MetadataReplicationWal {
    wal_path: PathBuf,
    lock: Mutex<()>,
}

impl MetadataReplicationWal {
    pub const FILE_NAME: &'static str = "metadata_replication.wal";

    pub fn open_or_init(dir: &Path) -> Result<Self, StorageError> {
        fs::create_dir_all(dir).map_err(StorageError::Io)?;
        let wal_path = dir.join(Self::FILE_NAME);

        if !wal_path.exists() {
            let mut file = File::create(&wal_path).map_err(StorageError::Io)?;
            file.write_all(b"# FSP Metadata Replication WAL v1\n")
                .map_err(StorageError::Io)?;
            file.sync_all().map_err(StorageError::Io)?;
            sync_directory(dir)?;
        }

        Ok(Self {
            wal_path,
            lock: Mutex::new(()),
        })
    }

    /// Appends and fsyncs a record to the WAL.
    pub fn append(&self, record: &ReplicationRecord) -> Result<(), StorageError> {
        let _guard = self.lock.lock().unwrap();
        let encoded = record.encode();

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.wal_path)
            .map_err(StorageError::Io)?;

        file.write_all(encoded.as_bytes())
            .map_err(StorageError::Io)?;
        file.sync_all().map_err(StorageError::Io)?;

        Ok(())
    }

    /// Reads and replays all records in the WAL.
    pub fn read_all(&self) -> Result<Vec<ReplicationRecord>, StorageError> {
        let _guard = self.lock.lock().unwrap();
        let file = File::open(&self.wal_path).map_err(StorageError::Io)?;
        let reader = BufReader::new(file);

        let mut records = Vec::new();
        let mut cur_block = Vec::new();

        for line in reader.lines() {
            let l = line.map_err(StorageError::Io)?;
            let trimmed = l.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed == "ENDREC" {
                if !cur_block.is_empty() {
                    let rec = ReplicationRecord::decode_block(&cur_block)?;
                    records.push(rec);
                    cur_block.clear();
                }
            } else {
                cur_block.push(l);
            }
        }

        Ok(records)
    }
}

/// A versioned, indexed log entry for distributed state machine replication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub term: u64,
    pub index: u64,
    pub record: ReplicationRecord,
}

impl LogEntry {
    pub fn new(term: u64, index: u64, record: ReplicationRecord) -> Self {
        Self {
            term,
            index,
            record,
        }
    }

    pub fn encode(&self) -> String {
        format!(
            "ENTRY_TERM={}\nENTRY_INDEX={}\n{}",
            self.term,
            self.index,
            self.record.encode()
        )
    }

    pub fn decode_block(lines: &[String]) -> Result<Self, StorageError> {
        let mut term = 0u64;
        let mut index = 0u64;
        let mut rec_lines = Vec::new();
        for line in lines {
            if let Some(t) = line.strip_prefix("ENTRY_TERM=") {
                term = t
                    .parse::<u64>()
                    .map_err(|e| StorageError::CorruptMetadata(e.to_string()))?;
            } else if let Some(idx) = line.strip_prefix("ENTRY_INDEX=") {
                index = idx
                    .parse::<u64>()
                    .map_err(|e| StorageError::CorruptMetadata(e.to_string()))?;
            } else {
                rec_lines.push(line.clone());
            }
        }
        let record = ReplicationRecord::decode_block(&rec_lines)?;
        Ok(Self {
            term,
            index,
            record,
        })
    }
}

/// Request to append entries to a standby replica (Raft AppendEntries RPC model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendEntriesRequest {
    pub term: u64,
    pub leader_id: CoordinatorId,
    pub prev_log_index: u64,
    pub prev_log_term: u64,
    pub entries: Vec<LogEntry>,
    pub leader_commit_index: u64,
}

/// Response from a replica to an AppendEntries request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendEntriesResponse {
    pub voter_id: CoordinatorId,
    pub term: u64,
    pub match_index: u64,
    pub success: bool,
    pub error_msg: Option<String>,
}

/// Durable snapshot of metadata state machine up to a given committed log index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataSnapshot {
    pub snapshot_index: u64,
    pub snapshot_term: u64,
    pub committed_manifest_generations: BTreeMap<String, u64>,
    pub snapshot_digest_hex: String,
}

impl MetadataSnapshot {
    pub fn new(
        snapshot_index: u64,
        snapshot_term: u64,
        generations: BTreeMap<String, u64>,
    ) -> Self {
        let digest = Self::calculate_digest(snapshot_index, snapshot_term, &generations);
        Self {
            snapshot_index,
            snapshot_term,
            committed_manifest_generations: generations,
            snapshot_digest_hex: digest,
        }
    }

    pub fn calculate_digest(
        snapshot_index: u64,
        snapshot_term: u64,
        generations: &BTreeMap<String, u64>,
    ) -> String {
        let mut text = format!(
            "FSP-SNAPSHOT-V1\nindex={}\nterm={}\n",
            snapshot_index, snapshot_term
        );
        for (k, v) in generations {
            text.push_str(&format!("gen:{}={}\n", k, v));
        }
        sha256_bytes(text.as_bytes()).to_hex()
    }

    pub fn verify_integrity(&self) -> bool {
        let expected = Self::calculate_digest(
            self.snapshot_index,
            self.snapshot_term,
            &self.committed_manifest_generations,
        );
        self.snapshot_digest_hex == expected
    }

    pub fn encode(&self) -> String {
        let mut out = format!(
            "SNAPSHOT_INDEX={}\nSNAPSHOT_TERM={}\nSNAPSHOT_DIGEST={}\n",
            self.snapshot_index, self.snapshot_term, self.snapshot_digest_hex
        );
        for (k, v) in &self.committed_manifest_generations {
            out.push_str(&format!("GEN:{}={}\n", k, v));
        }
        out.push_str("END_SNAPSHOT\n");
        out
    }

    pub fn decode(text: &str) -> Result<Self, StorageError> {
        let mut index = 0u64;
        let mut term = 0u64;
        let mut digest = String::new();
        let mut generations = BTreeMap::new();

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if trimmed == "END_SNAPSHOT" {
                break;
            }
            if let Some(val) = trimmed.strip_prefix("SNAPSHOT_INDEX=") {
                index = val
                    .parse::<u64>()
                    .map_err(|e| StorageError::CorruptMetadata(e.to_string()))?;
            } else if let Some(val) = trimmed.strip_prefix("SNAPSHOT_TERM=") {
                term = val
                    .parse::<u64>()
                    .map_err(|e| StorageError::CorruptMetadata(e.to_string()))?;
            } else if let Some(val) = trimmed.strip_prefix("SNAPSHOT_DIGEST=") {
                digest = val.to_string();
            } else if let Some(val) = trimmed.strip_prefix("GEN:") {
                if let Some((k, v)) = val.split_once('=') {
                    let g = v
                        .parse::<u64>()
                        .map_err(|e| StorageError::CorruptMetadata(e.to_string()))?;
                    generations.insert(k.to_string(), g);
                }
            }
        }

        let snap = Self {
            snapshot_index: index,
            snapshot_term: term,
            committed_manifest_generations: generations,
            snapshot_digest_hex: digest,
        };
        if !snap.verify_integrity() {
            return Err(StorageError::CorruptMetadata(
                "snapshot digest mismatch".to_string(),
            ));
        }
        Ok(snap)
    }
}

/// Manages a durable replicated log with majority quorum commit semantics.
#[derive(Debug)]
pub struct ReplicatedLogManager {
    wal_path: PathBuf,
    current_term: Arc<AtomicU64>,
    last_log_index: Arc<AtomicU64>,
    commit_index: Arc<AtomicU64>,
    applied_index: Arc<AtomicU64>,
    entries: Mutex<Vec<LogEntry>>,
    latest_snapshot: Mutex<Option<MetadataSnapshot>>,
}

impl ReplicatedLogManager {
    pub const FILE_NAME: &'static str = "replicated_log.wal";
    pub const SNAPSHOT_FILE_NAME: &'static str = "metadata_snapshot.dat";

    /// Opens or initializes the replicated log manager inside `dir`.
    pub fn open_or_init(dir: &Path, initial_term: u64) -> Result<Self, StorageError> {
        fs::create_dir_all(dir).map_err(StorageError::Io)?;
        let wal_path = dir.join(Self::FILE_NAME);
        let snap_path = dir.join(Self::SNAPSHOT_FILE_NAME);

        let mut entries = Vec::new();
        let mut last_idx = 0u64;

        if wal_path.exists() {
            let file = File::open(&wal_path).map_err(StorageError::Io)?;
            let reader = BufReader::new(file);
            let mut cur_block = Vec::new();

            for line in reader.lines() {
                let l = line.map_err(StorageError::Io)?;
                let trimmed = l.trim().to_string();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                cur_block.push(l);
                if trimmed == "ENDREC" {
                    let entry = LogEntry::decode_block(&cur_block)?;
                    if entry.index > last_idx {
                        last_idx = entry.index;
                    }
                    entries.push(entry);
                    cur_block.clear();
                }
            }
        } else {
            let mut file = File::create(&wal_path).map_err(StorageError::Io)?;
            file.write_all(b"# FSP Replicated Log WAL v1\n")
                .map_err(StorageError::Io)?;
            file.sync_all().map_err(StorageError::Io)?;
            sync_directory(dir)?;
        }

        let loaded_snapshot = if snap_path.exists() {
            let s = fs::read_to_string(&snap_path).map_err(StorageError::Io)?;
            MetadataSnapshot::decode(&s).ok()
        } else {
            None
        };

        let initial_commit = loaded_snapshot
            .as_ref()
            .map(|s| s.snapshot_index)
            .unwrap_or(0);
        if initial_commit > last_idx {
            last_idx = initial_commit;
        }

        Ok(Self {
            wal_path,
            current_term: Arc::new(AtomicU64::new(initial_term)),
            last_log_index: Arc::new(AtomicU64::new(last_idx)),
            commit_index: Arc::new(AtomicU64::new(initial_commit)),
            applied_index: Arc::new(AtomicU64::new(initial_commit)),
            entries: Mutex::new(entries),
            latest_snapshot: Mutex::new(loaded_snapshot),
        })
    }

    pub fn current_term(&self) -> u64 {
        self.current_term.load(Ordering::SeqCst)
    }

    pub fn last_log_index(&self) -> u64 {
        self.last_log_index.load(Ordering::SeqCst)
    }

    pub fn commit_index(&self) -> u64 {
        self.commit_index.load(Ordering::SeqCst)
    }

    pub fn applied_index(&self) -> u64 {
        self.applied_index.load(Ordering::SeqCst)
    }

    /// Proposes a new record at the specified term, appending it uncommitted to disk and memory.
    pub fn propose_entry(
        &self,
        term: u64,
        record: ReplicationRecord,
    ) -> Result<LogEntry, StorageError> {
        let mut entries = self.entries.lock().unwrap();
        let new_idx = self.last_log_index.fetch_add(1, Ordering::SeqCst) + 1;
        let entry = LogEntry::new(term, new_idx, record);

        let encoded = entry.encode();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.wal_path)
            .map_err(StorageError::Io)?;
        file.write_all(encoded.as_bytes())
            .map_err(StorageError::Io)?;
        file.sync_all().map_err(StorageError::Io)?;

        entries.push(entry.clone());
        Ok(entry)
    }

    /// Advances `commit_index` if `acks >= quorum_threshold`.
    /// Returns true if commit_index advanced, false if quorum not met.
    pub fn replicate_and_commit(
        &self,
        index: u64,
        acks: usize,
        quorum_threshold: usize,
    ) -> Result<bool, StorageError> {
        if acks >= quorum_threshold {
            let cur = self.commit_index.load(Ordering::SeqCst);
            if index > cur {
                self.commit_index.store(index, Ordering::SeqCst);
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Standby replica handles AppendEntries RPC from leader.
    pub fn handle_append_entries(
        &self,
        req: &AppendEntriesRequest,
        local_id: CoordinatorId,
    ) -> AppendEntriesResponse {
        let cur_term = self.current_term.load(Ordering::SeqCst);
        if req.term < cur_term {
            return AppendEntriesResponse {
                voter_id: local_id,
                term: cur_term,
                match_index: self.commit_index.load(Ordering::SeqCst),
                success: false,
                error_msg: Some(format!(
                    "request term {} < local term {}",
                    req.term, cur_term
                )),
            };
        }

        if req.term > cur_term {
            self.current_term.store(req.term, Ordering::SeqCst);
        }

        let mut entries = self.entries.lock().unwrap();

        // Check log matching invariant at prev_log_index
        if req.prev_log_index > 0 {
            let prev_match = entries
                .iter()
                .find(|e| e.index == req.prev_log_index && e.term == req.prev_log_term);
            if prev_match.is_none() {
                return AppendEntriesResponse {
                    voter_id: local_id,
                    term: self.current_term.load(Ordering::SeqCst),
                    match_index: self.commit_index.load(Ordering::SeqCst),
                    success: false,
                    error_msg: Some(format!(
                        "log mismatch at index {} term {}",
                        req.prev_log_index, req.prev_log_term
                    )),
                };
            }
        }

        // Truncate conflicting entries and append new entries
        for entry in &req.entries {
            if let Some(pos) = entries.iter().position(|e| e.index == entry.index) {
                if entries[pos].term != entry.term {
                    entries.truncate(pos);
                    entries.push(entry.clone());
                }
            } else {
                entries.push(entry.clone());
            }
        }

        // Update last log index
        if let Some(last) = entries.last() {
            self.last_log_index.store(last.index, Ordering::SeqCst);
        }

        // Rewrite and sync WAL
        let mut file_content = String::from("# FSP Replicated Log WAL v1\n");
        for e in entries.iter() {
            file_content.push_str(&e.encode());
        }
        if let Ok(mut file) = File::create(&self.wal_path) {
            let _ = file.write_all(file_content.as_bytes());
            let _ = file.sync_all();
        }

        // Update commit index
        let cur_last = self.last_log_index.load(Ordering::SeqCst);
        let new_commit = req.leader_commit_index.min(cur_last);
        if new_commit > self.commit_index.load(Ordering::SeqCst) {
            self.commit_index.store(new_commit, Ordering::SeqCst);
        }

        AppendEntriesResponse {
            voter_id: local_id,
            term: self.current_term.load(Ordering::SeqCst),
            match_index: self.last_log_index.load(Ordering::SeqCst),
            success: true,
            error_msg: None,
        }
    }

    /// Returns all log entries committed by quorum.
    pub fn committed_records(&self) -> Vec<ReplicationRecord> {
        let entries = self.entries.lock().unwrap();
        let commit_idx = self.commit_index.load(Ordering::SeqCst);
        entries
            .iter()
            .filter(|e| e.index <= commit_idx)
            .map(|e| e.record.clone())
            .collect()
    }

    pub fn latest_snapshot(&self) -> Option<MetadataSnapshot> {
        self.latest_snapshot.lock().unwrap().clone()
    }

    /// Creates and persists a metadata snapshot up to `up_to_index`.
    /// Truncates committed entries up to `up_to_index` to reclaim log space.
    pub fn create_snapshot(&self, up_to_index: u64) -> Result<MetadataSnapshot, StorageError> {
        let commit_idx = self.commit_index.load(Ordering::SeqCst);
        if up_to_index > commit_idx {
            return Err(StorageError::CorruptMetadata(format!(
                "cannot snapshot up to uncommitted index {} (current commit: {})",
                up_to_index, commit_idx
            )));
        }

        let mut entries = self.entries.lock().unwrap();
        let mut term = self.current_term.load(Ordering::SeqCst);
        let mut generations = self
            .latest_snapshot
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.committed_manifest_generations.clone())
            .unwrap_or_default();

        for e in entries.iter() {
            if e.index <= up_to_index {
                term = e.term;
                match &e.record {
                    ReplicationRecord::ManifestCommitted {
                        object_id,
                        generation,
                        ..
                    } => {
                        generations.insert(object_id.clone(), *generation);
                    }
                    ReplicationRecord::GenerationWatermarkAdvanced {
                        object_id,
                        generation,
                    } => {
                        generations.insert(object_id.clone(), *generation);
                    }
                    _ => {}
                }
            }
        }

        let snapshot = MetadataSnapshot::new(up_to_index, term, generations);

        // Persist snapshot atomically
        let dir = self.wal_path.parent().ok_or_else(|| {
            StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing parent dir",
            ))
        })?;
        let snap_path = dir.join(Self::SNAPSHOT_FILE_NAME);
        let snap_tmp = dir.join(format!("{}.tmp", Self::SNAPSHOT_FILE_NAME));
        {
            let mut file = File::create(&snap_tmp).map_err(StorageError::Io)?;
            file.write_all(snapshot.encode().as_bytes())
                .map_err(StorageError::Io)?;
            file.sync_all().map_err(StorageError::Io)?;
        }
        fs::rename(&snap_tmp, &snap_path).map_err(StorageError::Io)?;
        sync_directory(dir)?;

        // Compact in-memory and on-disk WAL entries
        entries.retain(|e| e.index > up_to_index);

        let mut file_content = String::from("# FSP Replicated Log WAL v1\n");
        for e in entries.iter() {
            file_content.push_str(&e.encode());
        }
        let wal_tmp = dir.join(format!("{}.tmp", Self::FILE_NAME));
        {
            let mut file = File::create(&wal_tmp).map_err(StorageError::Io)?;
            file.write_all(file_content.as_bytes())
                .map_err(StorageError::Io)?;
            file.sync_all().map_err(StorageError::Io)?;
        }
        fs::rename(&wal_tmp, &self.wal_path).map_err(StorageError::Io)?;
        sync_directory(dir)?;

        *self.latest_snapshot.lock().unwrap() = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Installs a received metadata snapshot, enforcing rollback prevention and digest validation.
    pub fn install_snapshot(&self, snapshot: &MetadataSnapshot) -> Result<(), StorageError> {
        if !snapshot.verify_integrity() {
            return Err(StorageError::CorruptMetadata(
                "snapshot integrity verification failed: digest mismatch".to_string(),
            ));
        }

        let cur_commit = self.commit_index.load(Ordering::SeqCst);
        if snapshot.snapshot_index < cur_commit {
            return Err(StorageError::CorruptMetadata(format!(
                "snapshot rollback rejected: snapshot index {} < current commit index {}",
                snapshot.snapshot_index, cur_commit
            )));
        }

        let dir = self.wal_path.parent().ok_or_else(|| {
            StorageError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "missing parent dir",
            ))
        })?;
        let snap_path = dir.join(Self::SNAPSHOT_FILE_NAME);
        let snap_tmp = dir.join(format!("{}.tmp", Self::SNAPSHOT_FILE_NAME));
        {
            let mut file = File::create(&snap_tmp).map_err(StorageError::Io)?;
            file.write_all(snapshot.encode().as_bytes())
                .map_err(StorageError::Io)?;
            file.sync_all().map_err(StorageError::Io)?;
        }
        fs::rename(&snap_tmp, &snap_path).map_err(StorageError::Io)?;
        sync_directory(dir)?;

        let mut entries = self.entries.lock().unwrap();
        entries.retain(|e| e.index > snapshot.snapshot_index);

        let mut file_content = String::from("# FSP Replicated Log WAL v1\n");
        for e in entries.iter() {
            file_content.push_str(&e.encode());
        }
        let wal_tmp = dir.join(format!("{}.tmp", Self::FILE_NAME));
        {
            let mut file = File::create(&wal_tmp).map_err(StorageError::Io)?;
            file.write_all(file_content.as_bytes())
                .map_err(StorageError::Io)?;
            file.sync_all().map_err(StorageError::Io)?;
        }
        fs::rename(&wal_tmp, &self.wal_path).map_err(StorageError::Io)?;
        sync_directory(dir)?;

        if snapshot.snapshot_term > self.current_term.load(Ordering::SeqCst) {
            self.current_term
                .store(snapshot.snapshot_term, Ordering::SeqCst);
        }
        self.commit_index
            .store(snapshot.snapshot_index, Ordering::SeqCst);
        self.applied_index
            .store(snapshot.snapshot_index, Ordering::SeqCst);
        if self.last_log_index.load(Ordering::SeqCst) < snapshot.snapshot_index {
            self.last_log_index
                .store(snapshot.snapshot_index, Ordering::SeqCst);
        }

        *self.latest_snapshot.lock().unwrap() = Some(snapshot.clone());
        Ok(())
    }
}
