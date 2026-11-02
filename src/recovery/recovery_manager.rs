//! Idempotent crash recovery manager for interrupted upload transactions and interrupted repairs.

use std::fs;

use crate::storage::backend::StorageError;
use crate::storage::filesystem::{
    ensure_not_symlink, sync_directory, write_and_sync_file, FilesystemBackend,
};
use crate::storage::manifest::ObjectManifest;
use crate::storage::transaction::{TransactionRecord, TransactionState};

/// Summary of crash-recovery actions performed on startup or explicit `recover()` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrashRecoveryReport {
    /// Number of interrupted uncommitted transactions rolled back and transitioned to `Aborted`.
    pub transactions_aborted: usize,
    /// Number of `Committing` transactions whose manifest was already valid on disk and finalized to `Committed`.
    pub transactions_finalized_committed: usize,
    /// Number of uncommitted partial shard files removed from node directories.
    pub partial_shard_files_cleaned: usize,
    /// Number of abandoned staging directories or `.tmp` / `.repair.tmp` files removed.
    pub temp_artifacts_cleaned: usize,
}

/// Executes idempotent crash recovery across transactions, staging directories, and node directories.
pub fn recover_storage_state(
    backend: &FilesystemBackend,
) -> Result<CrashRecoveryReport, StorageError> {
    backend.verify_sandbox_root()?;

    let mut transactions_aborted = 0usize;
    let mut transactions_finalized_committed = 0usize;
    let mut partial_shard_files_cleaned = 0usize;
    let mut temp_artifacts_cleaned = 0usize;

    let tx_dir = backend.root().join("metadata").join("transactions");
    ensure_not_symlink(&tx_dir)?;

    if tx_dir.exists() {
        for entry in fs::read_dir(&tx_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.exists() {
                continue;
            }
            if ensure_not_symlink(&path).is_err() {
                continue;
            }
            let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };

            // Clean up any torn `.tx.json.tmp` journal files
            if fname.ends_with(".tx.json.tmp") {
                let _ = fs::remove_file(&path);
                temp_artifacts_cleaned += 1;
                continue;
            }

            if !fname.ends_with(".tx.json") {
                continue;
            }

            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Some(mut record) = TransactionRecord::from_json(&content) else {
                continue;
            };

            match record.state {
                TransactionState::Committed | TransactionState::Aborted => {
                    // Already terminal; ensure staging directory is removed if still present
                    if let Ok(staging) = backend.staging_dir(&record.tx_id) {
                        if staging.exists() {
                            fs::remove_dir_all(&staging)?;
                            temp_artifacts_cleaned += 1;
                        }
                    }
                }
                TransactionState::Preparing
                | TransactionState::Writing
                | TransactionState::Verifying => {
                    // Commit point was never reached; clean up staging and mark Aborted
                    if let Ok(staging) = backend.staging_dir(&record.tx_id) {
                        if staging.exists() {
                            fs::remove_dir_all(&staging)?;
                            temp_artifacts_cleaned += 1;
                        }
                    }
                    record.state = TransactionState::Aborted;
                    write_and_sync_file(&path, record.to_json().as_bytes())?;
                    transactions_aborted += 1;
                }
                TransactionState::Committing => {
                    // Check if the final manifest exists and is valid
                    let manifest_path = backend.manifest_path(&record.object_id)?;
                    let manifest_valid = if manifest_path.exists() {
                        fs::read_to_string(&manifest_path)
                            .ok()
                            .and_then(|s| ObjectManifest::from_json(&s).ok())
                            .is_some()
                    } else {
                        false
                    };

                    if manifest_valid {
                        if let Ok(staging) = backend.staging_dir(&record.tx_id) {
                            if staging.exists() {
                                fs::remove_dir_all(&staging)?;
                                temp_artifacts_cleaned += 1;
                            }
                        }
                        record.state = TransactionState::Committed;
                        write_and_sync_file(&path, record.to_json().as_bytes())?;
                        transactions_finalized_committed += 1;
                    } else {
                        // Manifest commit point was not reached: roll back published partial shards
                        for rel in &record.published_relative_paths {
                            if let Ok(full) = backend.resolve_safe_relative_path(rel) {
                                if full.exists() {
                                    fs::remove_file(&full)?;
                                    partial_shard_files_cleaned += 1;
                                }
                            }
                        }
                        if let Ok(staging) = backend.staging_dir(&record.tx_id) {
                            if staging.exists() {
                                fs::remove_dir_all(&staging)?;
                                temp_artifacts_cleaned += 1;
                            }
                        }
                        record.state = TransactionState::Aborted;
                        write_and_sync_file(&path, record.to_json().as_bytes())?;
                        transactions_aborted += 1;
                    }
                }
            }
        }
        sync_directory(&tx_dir)?;
    }

    // Clean up any orphaned staging directories not referenced by an active transaction
    let staging_root = backend.root().join("metadata").join("staging");
    if staging_root.exists() {
        ensure_not_symlink(&staging_root)?;
        for entry in fs::read_dir(&staging_root)? {
            let entry = entry?;
            let p = entry.path();
            ensure_not_symlink(&p)?;
            if p.is_dir() {
                fs::remove_dir_all(&p)?;
            } else {
                fs::remove_file(&p)?;
            }
            temp_artifacts_cleaned += 1;
        }
        sync_directory(&staging_root)?;
    }

    // Clean up any orphaned `.tmp` or `.repair.tmp` files inside `metadata/manifests` and `node-XX/objects`
    let manifests_dir = backend.root().join("metadata").join("manifests");
    if manifests_dir.exists() {
        ensure_not_symlink(&manifests_dir)?;
        for entry in fs::read_dir(&manifests_dir)? {
            let entry = entry?;
            let p = entry.path();
            ensure_not_symlink(&p)?;
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".tmp") {
                    fs::remove_file(&p)?;
                    temp_artifacts_cleaned += 1;
                }
            }
        }
    }

    for node_idx in 0..backend.num_nodes() {
        let node_obj_dir = backend
            .root()
            .join(format!("node-{:02}", node_idx))
            .join("objects");
        if !node_obj_dir.exists() {
            continue;
        }
        ensure_not_symlink(&node_obj_dir)?;
        for obj_entry in fs::read_dir(&node_obj_dir)? {
            let obj_entry = obj_entry?;
            let obj_path = obj_entry.path();
            ensure_not_symlink(&obj_path)?;
            if obj_path.is_dir() {
                for file_entry in fs::read_dir(&obj_path)? {
                    let file_entry = file_entry?;
                    let fp = file_entry.path();
                    ensure_not_symlink(&fp)?;
                    if let Some(fname) = fp.file_name().and_then(|n| n.to_str()) {
                        if fname.ends_with(".tmp") {
                            fs::remove_file(&fp)?;
                            temp_artifacts_cleaned += 1;
                        }
                    }
                }
            }
        }
    }

    Ok(CrashRecoveryReport {
        transactions_aborted,
        transactions_finalized_committed,
        partial_shard_files_cleaned,
        temp_artifacts_cleaned,
    })
}
