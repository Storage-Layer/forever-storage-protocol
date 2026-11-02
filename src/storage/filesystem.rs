//! Local persistent filesystem storage backend with symlink/path-traversal defense,
//! atomic file publication, capacity quota enforcement, and sandboxed fault injection.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::model::failure_domain::FailureDomainId;
use crate::model::node::NodeId;
use crate::model::shard::ShardId;
use crate::storage::backend::{NodeStorageStatus, StorageError};
use crate::storage::manifest::{validate_safe_identifier, validate_safe_relative_path};

/// Sentinel file placed at `<root>/.fsp-marker` to prove a directory is a designated FSP storage root.
pub const FSP_STORAGE_MARKER_FILE: &str = ".fsp-marker";

/// Expected header line inside `.fsp-marker`.
pub const FSP_STORAGE_MARKER_MAGIC: &str = "FSP_STORAGE_ROOT_V1";

/// Persistent local filesystem backend managing `node-XX/` and `metadata/` directories.
#[derive(Debug, Clone)]
pub struct FilesystemBackend {
    root: PathBuf,
    num_nodes: usize,
    num_domains: usize,
    offline_nodes: BTreeSet<NodeId>,
    node_quotas: BTreeMap<NodeId, u64>,
}

impl FilesystemBackend {
    /// Initializes a new FSP storage root at `root` with `num_nodes` node directories (`node-00`..)
    /// mapped across `num_domains` logical failure domains.
    pub fn init_root(
        root: &Path,
        num_nodes: usize,
        num_domains: usize,
    ) -> Result<Self, StorageError> {
        if num_nodes == 0 || num_domains == 0 {
            return Err(StorageError::UnsafePathRejected(
                "num_nodes and num_domains must be >= 1".to_string(),
            ));
        }
        check_path_has_no_traversal(root)?;

        if root.exists() {
            ensure_not_symlink(root)?;
        } else {
            fs::create_dir_all(root)?;
        }

        let marker_path = root.join(FSP_STORAGE_MARKER_FILE);
        if marker_path.exists() {
            ensure_not_symlink(&marker_path)?;
        }
        let marker_contents = format!(
            "{}\nnum_nodes={}\nnum_domains={}\n",
            FSP_STORAGE_MARKER_MAGIC, num_nodes, num_domains
        );
        write_and_sync_file(&marker_path, marker_contents.as_bytes())?;

        for node_idx in 0..num_nodes {
            let node_dir = root.join(format!("node-{:02}", node_idx));
            let obj_dir = node_dir.join("objects");
            fs::create_dir_all(&obj_dir)?;
            ensure_not_symlink(&node_dir)?;
            ensure_not_symlink(&obj_dir)?;
        }

        let meta_dir = root.join("metadata");
        let manifests_dir = meta_dir.join("manifests");
        let tx_dir = meta_dir.join("transactions");
        let staging_dir = meta_dir.join("staging");
        fs::create_dir_all(&manifests_dir)?;
        fs::create_dir_all(&tx_dir)?;
        fs::create_dir_all(&staging_dir)?;
        ensure_not_symlink(&meta_dir)?;
        ensure_not_symlink(&manifests_dir)?;
        ensure_not_symlink(&tx_dir)?;
        ensure_not_symlink(&staging_dir)?;
        sync_directory(root)?;

        Ok(Self {
            root: root.to_path_buf(),
            num_nodes,
            num_domains,
            offline_nodes: BTreeSet::new(),
            node_quotas: BTreeMap::new(),
        })
    }

    /// Opens an existing initialized FSP storage root after verifying `.fsp-marker` and directory safety.
    pub fn open_existing(root: &Path) -> Result<Self, StorageError> {
        check_path_has_no_traversal(root)?;
        if !root.exists() {
            return Err(StorageError::UninitializedStorageRoot(
                root.display().to_string(),
            ));
        }
        ensure_not_symlink(root)?;

        let marker_path = root.join(FSP_STORAGE_MARKER_FILE);
        if !marker_path.exists() {
            return Err(StorageError::UninitializedStorageRoot(
                root.display().to_string(),
            ));
        }
        ensure_not_symlink(&marker_path)?;

        let content = fs::read_to_string(&marker_path)?;
        let mut lines = content.lines();
        if lines.next() != Some(FSP_STORAGE_MARKER_MAGIC) {
            return Err(StorageError::UninitializedStorageRoot(format!(
                "invalid marker magic in {}",
                marker_path.display()
            )));
        }

        let mut num_nodes = 20usize;
        let mut num_domains = 20usize;
        for line in lines {
            if let Some(rest) = line.strip_prefix("num_nodes=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    num_nodes = v;
                }
            } else if let Some(rest) = line.strip_prefix("num_domains=") {
                if let Ok(v) = rest.trim().parse::<usize>() {
                    num_domains = v;
                }
            }
        }

        ensure_not_symlink(&root.join("metadata"))?;
        ensure_not_symlink(&root.join("metadata").join("manifests"))?;
        ensure_not_symlink(&root.join("metadata").join("transactions"))?;
        ensure_not_symlink(&root.join("metadata").join("staging"))?;

        for node_idx in 0..num_nodes {
            let node_dir = root.join(format!("node-{:02}", node_idx));
            if node_dir.exists() {
                ensure_not_symlink(&node_dir)?;
            }
        }

        Ok(Self {
            root: root.to_path_buf(),
            num_nodes,
            num_domains,
            offline_nodes: BTreeSet::new(),
            node_quotas: BTreeMap::new(),
        })
    }

    /// Verifies that the storage root still holds a valid `.fsp-marker` before any write or delete.
    pub fn verify_sandbox_root(&self) -> Result<(), StorageError> {
        ensure_not_symlink(&self.root)?;
        let marker = self.root.join(FSP_STORAGE_MARKER_FILE);
        if !marker.exists() {
            return Err(StorageError::UninitializedStorageRoot(
                self.root.display().to_string(),
            ));
        }
        ensure_not_symlink(&marker)?;
        Ok(())
    }

    /// Root path of this storage backend.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Total configured storage nodes.
    pub const fn num_nodes(&self) -> usize {
        self.num_nodes
    }

    /// Total configured logical failure domains.
    pub const fn num_domains(&self) -> usize {
        self.num_domains
    }

    /// Deterministic failure domain assignment for `node_id`.
    pub const fn domain_for_node(&self, node_id: NodeId) -> FailureDomainId {
        FailureDomainId(node_id.0 % self.num_domains)
    }

    /// Sets whether `node_id` is online (`true`) or temporarily unreachable (`false`).
    pub fn set_node_online(&mut self, node_id: NodeId, online: bool) {
        if online {
            self.offline_nodes.remove(&node_id);
        } else {
            self.offline_nodes.insert(node_id);
        }
    }

    /// Returns `true` iff `node_id` is currently online.
    pub fn is_node_online(&self, node_id: NodeId) -> bool {
        node_id.0 < self.num_nodes && !self.offline_nodes.contains(&node_id)
    }

    /// Sets a byte capacity quota for `node_id` (`None` removes the quota).
    pub fn set_node_quota_bytes(&mut self, node_id: NodeId, quota: Option<u64>) {
        match quota {
            Some(q) => {
                self.node_quotas.insert(node_id, q);
            }
            None => {
                self.node_quotas.remove(&node_id);
            }
        }
    }

    /// Canonical relative path for a Phase 3 v1 shard file (`node-XX/objects/<object_id>/stripe-XXXXXX.shard-YY.vZ.fsp`).
    pub fn canonical_shard_relative_path(
        &self,
        node_id: NodeId,
        object_id: &str,
        stripe_index: usize,
        shard_id: ShardId,
        version: u64,
    ) -> Result<String, StorageError> {
        if !validate_safe_identifier(object_id) {
            return Err(StorageError::UnsafePathRejected(format!(
                "invalid object_id {:?}",
                object_id
            )));
        }
        Ok(format!(
            "node-{:02}/objects/{}/stripe-{:06}.shard-{:02}.v{}.fsp",
            node_id.0, object_id, stripe_index, shard_id.0, version
        ))
    }

    /// Canonical relative path for a Phase 4 v2 per-shard multi-stripe container (`node-XX/objects/<object_id>/shard-YY.vZ.fspc`).
    pub fn canonical_container_relative_path(
        &self,
        node_id: NodeId,
        object_id: &str,
        shard_id: ShardId,
        version: u64,
    ) -> Result<String, StorageError> {
        if !validate_safe_identifier(object_id) {
            return Err(StorageError::UnsafePathRejected(format!(
                "invalid object_id {:?}",
                object_id
            )));
        }
        Ok(format!(
            "node-{:02}/objects/{}/shard-{:02}.v{}.fspc",
            node_id.0, object_id, shard_id.0, version
        ))
    }

    /// Acquires an in-process shared read lock for `(root, object_id)`.
    pub fn acquire_object_read_lock(&self, object_id: &str) -> ObjectReadLockGuard {
        let arc = get_object_rwlock(&self.root, object_id);
        ObjectReadLockGuard { _arc: arc }
    }

    /// Acquires an in-process exclusive write/repair lock for `(root, object_id)`.
    pub fn acquire_object_write_lock(&self, object_id: &str) -> ObjectWriteLockGuard {
        let arc = get_object_rwlock(&self.root, object_id);
        ObjectWriteLockGuard { _arc: arc }
    }

    /// Resolves and validates a relative path inside `<root>`, ensuring no symlink traversal occurs.
    pub fn resolve_safe_relative_path(&self, rel_path: &str) -> Result<PathBuf, StorageError> {
        self.verify_sandbox_root()?;
        if !validate_safe_relative_path(rel_path) {
            return Err(StorageError::UnsafePathRejected(format!(
                "unsafe relative path {:?}",
                rel_path
            )));
        }
        let mut current = self.root.clone();
        for part in rel_path.split('/') {
            current.push(part);
            if current.exists() {
                ensure_not_symlink(&current)?;
            }
        }
        Ok(current)
    }

    /// Returns the directory path `<root>/node-XX/objects/<object_id>`.
    pub fn node_object_dir(
        &self,
        node_id: NodeId,
        object_id: &str,
    ) -> Result<PathBuf, StorageError> {
        if !validate_safe_identifier(object_id) {
            return Err(StorageError::UnsafePathRejected(format!(
                "unsafe object_id {:?}",
                object_id
            )));
        }
        let rel = format!("node-{:02}/objects/{}", node_id.0, object_id);
        self.resolve_safe_relative_path(&rel)
    }

    /// Path to `<root>/metadata/manifests/<object_id>.manifest.json`.
    pub fn manifest_path(&self, object_id: &str) -> Result<PathBuf, StorageError> {
        if !validate_safe_identifier(object_id) {
            return Err(StorageError::UnsafePathRejected(format!(
                "unsafe object_id {:?}",
                object_id
            )));
        }
        let rel = format!("metadata/manifests/{}.manifest.json", object_id);
        self.resolve_safe_relative_path(&rel)
    }

    /// Path to `<root>/metadata/transactions/<tx_id>.tx.json`.
    pub fn transaction_path(&self, tx_id: &str) -> Result<PathBuf, StorageError> {
        if !validate_safe_identifier(tx_id) {
            return Err(StorageError::UnsafePathRejected(format!(
                "unsafe tx_id {:?}",
                tx_id
            )));
        }
        let rel = format!("metadata/transactions/{}.tx.json", tx_id);
        self.resolve_safe_relative_path(&rel)
    }

    /// Path to `<root>/metadata/staging/<tx_id>`.
    pub fn staging_dir(&self, tx_id: &str) -> Result<PathBuf, StorageError> {
        if !validate_safe_identifier(tx_id) {
            return Err(StorageError::UnsafePathRejected(format!(
                "unsafe tx_id {:?}",
                tx_id
            )));
        }
        let rel = format!("metadata/staging/{}", tx_id);
        self.resolve_safe_relative_path(&rel)
    }

    /// Computes total stored bytes and shard file count on `node_id`.
    pub fn node_usage(&self, node_id: NodeId) -> Result<(u64, usize), StorageError> {
        self.verify_sandbox_root()?;
        let node_obj_dir = self
            .root
            .join(format!("node-{:02}", node_id.0))
            .join("objects");
        if !node_obj_dir.exists() {
            return Ok((0, 0));
        }
        ensure_not_symlink(&node_obj_dir)?;

        let mut total_bytes = 0u64;
        let mut count = 0usize;
        for entry in fs::read_dir(&node_obj_dir)? {
            let entry = entry?;
            let path = entry.path();
            ensure_not_symlink(&path)?;
            if path.is_dir() {
                for sub in fs::read_dir(&path)? {
                    let sub = sub?;
                    let sp = sub.path();
                    ensure_not_symlink(&sp)?;
                    if sp.is_file() {
                        let meta = fs::metadata(&sp)?;
                        let ext = sp.extension().and_then(|e| e.to_str());
                        if ext == Some("fsp") || ext == Some("fspc") {
                            total_bytes += meta.len();
                            count += 1;
                        }
                    }
                }
            }
        }
        Ok((total_bytes, count))
    }

    /// Checks whether writing `requested_bytes` to `node_id` is allowed by node availability and quota.
    pub fn check_node_write_admission(
        &self,
        node_id: NodeId,
        requested_bytes: u64,
    ) -> Result<(), StorageError> {
        if !self.is_node_online(node_id) {
            return Err(StorageError::NodeUnavailable(node_id));
        }
        if let Some(&quota_bytes) = self.node_quotas.get(&node_id) {
            let (used_bytes, _) = self.node_usage(node_id)?;
            if used_bytes.saturating_add(requested_bytes) > quota_bytes {
                return Err(StorageError::QuotaExceeded {
                    node_id,
                    used_bytes,
                    requested_bytes,
                    quota_bytes,
                });
            }
        }
        Ok(())
    }

    /// Returns status for all configured storage nodes.
    pub fn all_node_statuses(&self) -> Result<Vec<NodeStorageStatus>, StorageError> {
        let mut statuses = Vec::with_capacity(self.num_nodes);
        for i in 0..self.num_nodes {
            let node_id = NodeId(i);
            let (used_bytes, shard_file_count) = self.node_usage(node_id)?;
            statuses.push(NodeStorageStatus {
                node_id,
                domain_id: self.domain_for_node(node_id),
                is_online: self.is_node_online(node_id),
                used_bytes,
                quota_bytes: self.node_quotas.get(&node_id).copied(),
                shard_file_count,
            });
        }
        Ok(statuses)
    }

    /// Lists all committed object IDs in `<root>/metadata/manifests/`.
    pub fn list_objects(&self) -> Result<Vec<String>, StorageError> {
        self.verify_sandbox_root()?;
        let manifests_dir = self.root.join("metadata").join("manifests");
        ensure_not_symlink(&manifests_dir)?;
        let mut ids = Vec::new();
        for entry in fs::read_dir(&manifests_dir)? {
            let entry = entry?;
            let path = entry.path();
            ensure_not_symlink(&path)?;
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if let Some(id) = name.strip_suffix(".manifest.json") {
                    if validate_safe_identifier(id) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    // =========================================================================
    // Sandboxed Failure Injection Harness Methods
    // =========================================================================

    /// Deletes a specific shard file inside the sandboxed storage root.
    pub fn inject_delete_shard(&self, rel_path: &str) -> Result<(), StorageError> {
        let full = self.resolve_safe_relative_path(rel_path)?;
        if full.exists() {
            fs::remove_file(&full)?;
        }
        Ok(())
    }

    /// Flips `XOR 0xFF` at `byte_offset` in a specific shard file inside the sandboxed storage root.
    pub fn inject_corrupt_shard_byte(
        &self,
        rel_path: &str,
        byte_offset: u64,
    ) -> Result<(), StorageError> {
        let full = self.resolve_safe_relative_path(rel_path)?;
        let mut file = OpenOptions::new().read(true).write(true).open(&full)?;
        file.seek(SeekFrom::Start(byte_offset))?;
        let mut b = [0u8; 1];
        file.read_exact(&mut b)?;
        b[0] ^= 0xFF;
        file.seek(SeekFrom::Start(byte_offset))?;
        file.write_all(&b)?;
        file.sync_all()?;
        Ok(())
    }

    /// Truncates a specific shard file to `new_len` bytes inside the sandboxed storage root.
    pub fn inject_truncate_shard(&self, rel_path: &str, new_len: u64) -> Result<(), StorageError> {
        let full = self.resolve_safe_relative_path(rel_path)?;
        let file = OpenOptions::new().write(true).open(&full)?;
        file.set_len(new_len)?;
        file.sync_all()?;
        Ok(())
    }

    /// Writes a stale older version file (`.v0.fsp`) with outdated bytes alongside or in place of a shard.
    pub fn inject_stale_shard_version(
        &self,
        node_id: NodeId,
        object_id: &str,
        stripe_index: usize,
        shard_id: ShardId,
        stale_bytes: &[u8],
    ) -> Result<String, StorageError> {
        let rel =
            self.canonical_shard_relative_path(node_id, object_id, stripe_index, shard_id, 0)?;
        let full = self.resolve_safe_relative_path(&rel)?;
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        write_and_sync_file(&full, stale_bytes)?;
        Ok(rel)
    }

    /// Writes a conflicting higher-version shard file (`.v99.fsp`) with unverified bytes to test version conflict resolution.
    pub fn inject_conflicting_shard_version(
        &self,
        node_id: NodeId,
        object_id: &str,
        stripe_index: usize,
        shard_id: ShardId,
        conflicting_bytes: &[u8],
    ) -> Result<String, StorageError> {
        let rel =
            self.canonical_shard_relative_path(node_id, object_id, stripe_index, shard_id, 99)?;
        let full = self.resolve_safe_relative_path(&rel)?;
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent)?;
        }
        write_and_sync_file(&full, conflicting_bytes)?;
        Ok(rel)
    }

    /// Corrupts an object's manifest file inside the sandboxed storage root.
    pub fn inject_corrupt_manifest(
        &self,
        object_id: &str,
        corrupted_content: &str,
    ) -> Result<(), StorageError> {
        let path = self.manifest_path(object_id)?;
        write_and_sync_file(&path, corrupted_content.as_bytes())
    }
}

/// Checks that `path` contains no `..` traversal components or NUL bytes.
pub fn check_path_has_no_traversal(path: &Path) -> Result<(), StorageError> {
    let s = path.to_string_lossy();
    if s.is_empty() || s.contains('\0') {
        return Err(StorageError::UnsafePathRejected(format!(
            "empty or NUL path: {:?}",
            path
        )));
    }
    for comp in path.components() {
        if matches!(comp, std::path::Component::ParentDir) {
            return Err(StorageError::UnsafePathRejected(format!(
                "parent directory traversal '..' rejected in {:?}",
                path
            )));
        }
    }
    Ok(())
}

/// Verifies via `symlink_metadata` that `path` is NOT a symbolic link.
pub fn ensure_not_symlink(path: &Path) -> Result<(), StorageError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(StorageError::UnsafePathRejected(format!(
            "symlink redirection rejected at {:?}",
            path
        )));
    }
    Ok(())
}

/// Writes `bytes` to `path` (using `O_NOFOLLOW` on Linux), flushes, and calls `File::sync_all()`.
pub fn write_and_sync_file(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    if path.exists() {
        ensure_not_symlink(path)?;
    }
    let mut file = create_file_nofollow(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Opens a file for reading while atomically rejecting symlinks (`O_NOFOLLOW` on Linux).
pub fn open_file_nofollow(path: &Path) -> Result<File, StorageError> {
    ensure_not_symlink(path)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW on Linux x86_64/aarch64 is 0o400000
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(0o400000)
            .open(path)?;
        Ok(f)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(File::open(path)?)
    }
}

/// Creates/truncates a file for writing while atomically rejecting symlinks (`O_NOFOLLOW` on Linux).
pub fn create_file_nofollow(path: &Path) -> Result<File, StorageError> {
    if path.exists() {
        ensure_not_symlink(path)?;
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .custom_flags(0o400000)
            .open(path)?;
        Ok(f)
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(File::create(path)?)
    }
}

/// Calls `sync_all()` on a directory handle to persist directory entry changes (`rename` / `create`).
pub fn sync_directory(dir: &Path) -> Result<(), StorageError> {
    let d = File::open(dir)?;
    d.sync_all()?;
    Ok(())
}

static OBJECT_LOCK_REGISTRY: OnceLock<Mutex<BTreeMap<String, Arc<RwLock<()>>>>> = OnceLock::new();

fn get_object_rwlock(root: &Path, object_id: &str) -> Arc<RwLock<()>> {
    let registry = OBJECT_LOCK_REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()));
    let key = format!("{}::{}", root.display(), object_id);
    let mut map = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    map.entry(key)
        .or_insert_with(|| Arc::new(RwLock::new(())))
        .clone()
}

/// Per-object shared reader lock guard (`GET` / `VERIFY`).
pub struct ObjectReadLockGuard {
    _arc: Arc<RwLock<()>>,
}

impl ObjectReadLockGuard {
    /// Acquires the underlying `RwLockReadGuard` for the duration of a read operation.
    pub fn read(&self) -> RwLockReadGuard<'_, ()> {
        self._arc
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Per-object exclusive writer/repair lock guard (`PUT` / `REPAIR` / `MIGRATE`).
pub struct ObjectWriteLockGuard {
    _arc: Arc<RwLock<()>>,
}

impl ObjectWriteLockGuard {
    /// Acquires the underlying `RwLockWriteGuard` for the duration of a mutation/commit operation.
    pub fn write(&self) -> RwLockWriteGuard<'_, ()> {
        self._arc
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
