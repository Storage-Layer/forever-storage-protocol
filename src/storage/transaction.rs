//! Crash-safe atomic storage transaction state machine (`Preparing -> Writing -> Verifying -> Committing -> Committed | Aborted`).

use std::fmt;
use std::fs;
use std::path::PathBuf;

use crate::storage::backend::StorageError;
use crate::storage::container::ShardContainerReader;
use crate::storage::filesystem::{sync_directory, write_and_sync_file, FilesystemBackend};
use crate::storage::integrity::sha256_file;
use crate::storage::manifest::ObjectManifest;

/// Explicit 6-state crash-consistency transaction state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    /// Transaction record created and staging directory initialized.
    Preparing,
    /// Temporary shard files are being written and flushed to staging.
    Writing,
    /// Written temporary shard files are being re-read from disk and SHA-256 verified.
    Verifying,
    /// Verified shard files are being atomically moved to node slots and manifest is being committed.
    Committing,
    /// Manifest commit point completed and staging cleaned up.
    Committed,
    /// Transaction rolled back and aborted; no partial object is visible.
    Aborted,
}

impl TransactionState {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Preparing => "Preparing",
            Self::Writing => "Writing",
            Self::Verifying => "Verifying",
            Self::Committing => "Committing",
            Self::Committed => "Committed",
            Self::Aborted => "Aborted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "Preparing" => Some(Self::Preparing),
            "Writing" => Some(Self::Writing),
            "Verifying" => Some(Self::Verifying),
            "Committing" => Some(Self::Committing),
            "Committed" => Some(Self::Committed),
            "Aborted" => Some(Self::Aborted),
            _ => None,
        }
    }
}

impl fmt::Display for TransactionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Deterministic crash/interruption injection point during a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    /// Interrupt immediately after entering `Preparing`.
    AfterPreparing,
    /// Interrupt after writing staged shard files in `Writing`.
    MidWriting,
    /// Interrupt after SHA-256 verification of staged shards in `Verifying`.
    AfterVerifying,
    /// Interrupt during `Committing` after publishing half of the shard files, BEFORE manifest rename.
    MidCommittingBeforeManifest,
}

/// How the injected crash should terminate execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashMode {
    /// Returns `Err(StorageError::SimulatedCrash)` without rolling back, leaving on-disk state interrupted.
    SyntheticLeaveInterrupted,
    /// Immediately terminates the OS process via `std::process::exit(exit_code)` (for subprocess crash tests).
    KillProcess { exit_code: i32 },
}

/// Configured fault injection trigger for transaction testing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashTrigger {
    pub point: CrashPoint,
    pub mode: CrashMode,
}

impl CrashTrigger {
    pub fn fire_if_matched(&self, current: CrashPoint) -> Result<(), StorageError> {
        if self.point == current {
            match self.mode {
                CrashMode::SyntheticLeaveInterrupted => {
                    return Err(StorageError::SimulatedCrash(format!("{:?}", current)));
                }
                CrashMode::KillProcess { exit_code } => {
                    std::process::exit(exit_code);
                }
            }
        }
        Ok(())
    }
}

/// On-disk journal record for a write transaction (`<root>/metadata/transactions/<tx_id>.tx.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionRecord {
    /// Unique transaction identifier.
    pub tx_id: String,
    /// Target object identifier.
    pub object_id: String,
    /// Current state of the transaction.
    pub state: TransactionState,
    /// Relative paths under `<root>` of shard files published during `Committing`.
    pub published_relative_paths: Vec<String>,
}

impl TransactionRecord {
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        out.push_str("{\n");
        out.push_str(&format!("  \"tx_id\": \"{}\",\n", self.tx_id));
        out.push_str(&format!("  \"object_id\": \"{}\",\n", self.object_id));
        out.push_str(&format!("  \"state\": \"{}\",\n", self.state.as_str()));
        out.push_str("  \"published_paths\": [\n");
        for (i, p) in self.published_relative_paths.iter().enumerate() {
            let comma = if i + 1 < self.published_relative_paths.len() {
                ","
            } else {
                ""
            };
            out.push_str(&format!("    \"{}\"{}\n", p, comma));
        }
        out.push_str("  ]\n}\n");
        out
    }

    pub fn from_json(json: &str) -> Option<Self> {
        let tx_id = extract_quoted(json, "\"tx_id\":")?;
        let object_id = extract_quoted(json, "\"object_id\":")?;
        let state_str = extract_quoted(json, "\"state\":")?;
        let state = TransactionState::parse(&state_str)?;

        let arr_pos = json.find("\"published_paths\":")?;
        let after = &json[arr_pos..];
        let open = after.find('[')?;
        let close = after[open..].find(']')? + open;
        let inner = &after[open + 1..close];
        let mut published_relative_paths = Vec::new();
        for line in inner.lines() {
            let trimmed = line.trim().trim_end_matches(',');
            if trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2 {
                published_relative_paths.push(trimmed[1..trimmed.len() - 1].to_string());
            }
        }

        Some(Self {
            tx_id,
            object_id,
            state,
            published_relative_paths,
        })
    }
}

fn extract_quoted(haystack: &str, key: &str) -> Option<String> {
    let pos = haystack.find(key)?;
    let rest = haystack[pos + key.len()..].trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let after = &rest[1..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

/// Staged shard artifact written during the `Writing` phase of a transaction.
#[derive(Debug, Clone)]
pub struct StagedShardFile {
    pub stripe_index: usize,
    pub shard_id: usize,
    pub staged_path: PathBuf,
    pub target_relative_path: String,
    pub expected_size_bytes: u64,
    pub expected_sha256_hex: String,
    pub is_container_v2: bool,
}

/// Active write transaction handle enforcing the 6-state transition machine.
pub struct WriteTransaction<'a> {
    backend: &'a FilesystemBackend,
    record: TransactionRecord,
    staging_dir: PathBuf,
    staged_shards: Vec<StagedShardFile>,
    crash_trigger: Option<CrashTrigger>,
}

impl<'a> WriteTransaction<'a> {
    /// Begins a new write transaction in `Preparing` state.
    pub fn begin(
        backend: &'a FilesystemBackend,
        tx_id: &str,
        object_id: &str,
        crash_trigger: Option<CrashTrigger>,
    ) -> Result<Self, StorageError> {
        backend.verify_sandbox_root()?;
        let staging_dir = backend.staging_dir(tx_id)?;
        if staging_dir.exists() {
            fs::remove_dir_all(&staging_dir)?;
        }
        fs::create_dir_all(&staging_dir)?;

        let record = TransactionRecord {
            tx_id: tx_id.to_string(),
            object_id: object_id.to_string(),
            state: TransactionState::Preparing,
            published_relative_paths: Vec::new(),
        };
        Self::persist_record(backend, &record)?;

        if let Some(trig) = crash_trigger {
            trig.fire_if_matched(CrashPoint::AfterPreparing)?;
        }

        Ok(Self {
            backend,
            record,
            staging_dir,
            staged_shards: Vec::new(),
            crash_trigger,
        })
    }

    /// Transitions from `Preparing` to `Writing`.
    pub fn enter_writing(&mut self) -> Result<(), StorageError> {
        assert_eq!(self.record.state, TransactionState::Preparing);
        self.record.state = TransactionState::Writing;
        Self::persist_record(self.backend, &self.record)
    }

    /// Writes a single shard to the transaction's staging directory and flushes/fsyncs it.
    #[allow(clippy::too_many_arguments)]
    pub fn stage_shard(
        &mut self,
        stripe_index: usize,
        shard_id: usize,
        bytes: &[u8],
        expected_sha256_hex: String,
        target_relative_path: String,
    ) -> Result<(), StorageError> {
        assert_eq!(self.record.state, TransactionState::Writing);
        let staged_name = format!("stripe-{:06}.shard-{:02}.tmp", stripe_index, shard_id);
        let staged_path = self.staging_dir.join(staged_name);
        write_and_sync_file(&staged_path, bytes)?;

        self.staged_shards.push(StagedShardFile {
            stripe_index,
            shard_id,
            staged_path,
            target_relative_path,
            expected_size_bytes: bytes.len() as u64,
            expected_sha256_hex,
            is_container_v2: false,
        });
        Ok(())
    }

    /// Returns the transaction's staging directory path.
    pub fn staging_dir_path(&self) -> &std::path::Path {
        &self.staging_dir
    }

    /// Registers a finalized v2 multi-stripe shard container in the transaction's staged list.
    pub fn register_staged_container(
        &mut self,
        shard_id: usize,
        staged_path: PathBuf,
        target_relative_path: String,
        expected_size_bytes: u64,
        expected_body_sha256_hex: String,
    ) {
        assert_eq!(self.record.state, TransactionState::Writing);
        self.staged_shards.push(StagedShardFile {
            stripe_index: 0,
            shard_id,
            staged_path,
            target_relative_path,
            expected_size_bytes,
            expected_sha256_hex: expected_body_sha256_hex,
            is_container_v2: true,
        });
    }

    /// Updates the final `object_id` and resolves `target_relative_path` for all staged shards
    /// when `object_id` is content-addressed from the streaming SHA-256 digest.
    pub fn set_final_object_id(&mut self, final_object_id: &str) -> Result<(), StorageError> {
        self.record.object_id = final_object_id.to_string();
        for item in &mut self.staged_shards {
            let node_id = crate::model::node::NodeId(item.shard_id % self.backend.num_nodes());
            if item.is_container_v2 {
                item.target_relative_path = self.backend.canonical_container_relative_path(
                    node_id,
                    final_object_id,
                    crate::model::shard::ShardId(item.shard_id),
                    1,
                )?;
            } else {
                item.target_relative_path = self.backend.canonical_shard_relative_path(
                    node_id,
                    final_object_id,
                    item.stripe_index,
                    crate::model::shard::ShardId(item.shard_id),
                    1,
                )?;
            }
        }
        Self::persist_record(self.backend, &self.record)
    }

    /// Transitions from `Writing` to `Verifying` and re-verifies every staged shard file on disk.
    pub fn verify_staged_shards(&mut self) -> Result<(), StorageError> {
        assert_eq!(self.record.state, TransactionState::Writing);
        if let Some(trig) = self.crash_trigger {
            trig.fire_if_matched(CrashPoint::MidWriting)?;
        }

        self.record.state = TransactionState::Verifying;
        Self::persist_record(self.backend, &self.record)?;

        let mut verification_err: Option<StorageError> = None;
        for item in &self.staged_shards {
            if item.is_container_v2 {
                match ShardContainerReader::open(&item.staged_path) {
                    Ok(mut reader) => {
                        if reader.header().total_file_size != item.expected_size_bytes {
                            verification_err = Some(StorageError::ShardIntegrityFailure {
                                object_id: self.record.object_id.clone(),
                                stripe_index: 0,
                                shard_id: crate::model::shard::ShardId(item.shard_id),
                                reason: format!(
                                    "staged container size mismatch: expected {}, got {}",
                                    item.expected_size_bytes,
                                    reader.header().total_file_size
                                ),
                            });
                            break;
                        }
                        if let Err(e) = reader.verify_all() {
                            verification_err = Some(StorageError::Container(e));
                            break;
                        }
                    }
                    Err(e) => {
                        verification_err = Some(StorageError::Container(e));
                        break;
                    }
                }
                continue;
            }
            let (actual_len, digest) = sha256_file(&item.staged_path)?;
            if actual_len != item.expected_size_bytes {
                verification_err = Some(StorageError::ShardIntegrityFailure {
                    object_id: self.record.object_id.clone(),
                    stripe_index: item.stripe_index,
                    shard_id: crate::model::shard::ShardId(item.shard_id),
                    reason: format!(
                        "staged shard size mismatch: expected {}, got {}",
                        item.expected_size_bytes, actual_len
                    ),
                });
                break;
            }
            let actual_hex = digest.to_hex();
            if actual_hex != item.expected_sha256_hex {
                verification_err = Some(StorageError::ShardIntegrityFailure {
                    object_id: self.record.object_id.clone(),
                    stripe_index: item.stripe_index,
                    shard_id: crate::model::shard::ShardId(item.shard_id),
                    reason: format!(
                        "staged shard SHA-256 mismatch: expected {}, got {}",
                        item.expected_sha256_hex, actual_hex
                    ),
                });
                break;
            }
        }
        if let Some(err) = verification_err {
            self.abort()?;
            return Err(err);
        }

        if let Some(trig) = self.crash_trigger {
            trig.fire_if_matched(CrashPoint::AfterVerifying)?;
        }

        Ok(())
    }

    /// Transitions from `Verifying` to `Committing`, publishes all verified shard files atomically,
    /// commits the validated manifest via atomic rename, and transitions to `Committed`.
    pub fn commit(mut self, manifest: &ObjectManifest) -> Result<(), StorageError> {
        assert_eq!(self.record.state, TransactionState::Verifying);
        manifest.validate()?;

        // Record all target shard paths in the journal BEFORE moving files so crash recovery knows
        // which shard files belong to this transaction if interrupted before manifest commit.
        self.record.state = TransactionState::Committing;
        self.record.published_relative_paths = self
            .staged_shards
            .iter()
            .map(|s| s.target_relative_path.clone())
            .collect();
        Self::persist_record(self.backend, &self.record)?;

        let half = self.staged_shards.len() / 2;
        for (idx, item) in self.staged_shards.iter().enumerate() {
            let target_full = self
                .backend
                .resolve_safe_relative_path(&item.target_relative_path)?;
            if let Some(parent) = target_full.parent() {
                let parent_existed = parent.exists();
                if !parent_existed {
                    fs::create_dir_all(parent)?;
                    if let Some(grandparent) = parent.parent() {
                        sync_directory(grandparent)?;
                    }
                }
            }
            fs::rename(&item.staged_path, &target_full)?;
            if let Some(parent) = target_full.parent() {
                sync_directory(parent)?;
            }

            if idx == half {
                if let Some(trig) = self.crash_trigger {
                    trig.fire_if_matched(CrashPoint::MidCommittingBeforeManifest)?;
                }
            }
        }

        // Write validated manifest to staging and fsync
        let staged_manifest_path = self.staging_dir.join("manifest.json.tmp");
        let manifest_json = manifest.to_canonical_json();
        write_and_sync_file(&staged_manifest_path, manifest_json.as_bytes())?;

        // UNAMBIGUOUS ATOMIC COMMIT POINT: rename manifest.json.tmp -> metadata/manifests/<object_id>.manifest.json
        let final_manifest_path = self.backend.manifest_path(&self.record.object_id)?;
        fs::rename(&staged_manifest_path, &final_manifest_path)?;
        if let Some(manifests_dir) = final_manifest_path.parent() {
            sync_directory(manifests_dir)?;
        }

        // Transition journal to Committed and remove staging directory
        self.record.state = TransactionState::Committed;
        Self::persist_record(self.backend, &self.record)?;
        if self.staging_dir.exists() {
            let _ = fs::remove_dir_all(&self.staging_dir);
        }

        Ok(())
    }

    /// Explicitly aborts the transaction and cleans up staged and uncommitted shard files.
    pub fn abort(&mut self) -> Result<(), StorageError> {
        for rel in &self.record.published_relative_paths {
            if let Ok(full) = self.backend.resolve_safe_relative_path(rel) {
                if full.exists() {
                    let _ = fs::remove_file(&full);
                }
            }
        }
        if self.staging_dir.exists() {
            let _ = fs::remove_dir_all(&self.staging_dir);
        }
        self.record.state = TransactionState::Aborted;
        Self::persist_record(self.backend, &self.record)
    }

    fn persist_record(
        backend: &FilesystemBackend,
        record: &TransactionRecord,
    ) -> Result<(), StorageError> {
        let tx_path = backend.transaction_path(&record.tx_id)?;
        let tmp_path = tx_path.with_extension("tx.json.tmp");
        write_and_sync_file(&tmp_path, record.to_json().as_bytes())?;
        fs::rename(&tmp_path, &tx_path)?;
        if let Some(parent) = tx_path.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }
}
