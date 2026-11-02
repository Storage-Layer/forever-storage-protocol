//! Crash-Recoverable Repair Journal and State Machine (`src/coordinator/repair_journal.rs`).
//!
//! Provides a durable on-disk repair job journal with state transitions:
//! - `Detected`: Corruption or missing shard observed.
//! - `Pending`: Enqueued for repair execution.
//! - `CapacityCheck`: Evaluating cluster spare capacity and failure domain constraints.
//! - `WaitingForCapacity`: Blocked due to lack of eligible healthy nodes; backed off with jitter.
//! - `Scheduled`: Eligible target node assigned; repair dispatched.
//! - `Repairing`: Reconstructing shard bytes from surviving quorum.
//! - `Verifying`: Coordinator streaming independent integrity check.
//! - `Completed`: Successfully committed to target node and signed manifest advanced.
//! - `RetryPending`: Transient failure encountered; backoff applied.
//! - `Cancelled`: Shard or host recovered, or object deleted.
//!
//! All state transitions are fsynced to `<coord_root>/repair_jobs/<job_id>.rec`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::model::node::NodeId;
use crate::storage::backend::StorageError;
use crate::storage::filesystem::{
    check_path_has_no_traversal, ensure_not_symlink, sync_directory, write_and_sync_file,
};

/// High-level cluster health status as required by Phase 4.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterHealthState {
    /// All objects have full $n$ valid replicas/shards placed according to domain constraints.
    FullyProtected,
    /// One or more objects are degraded (have lost shards) but surviving shards $\ge k$ can recover.
    DegradedButRecoverable,
    /// Sufficient surviving shards exist, but temporary network partitions or transient outages
    /// prevent immediate access.
    TemporarilyUnavailable,
    /// Degraded objects exist, but repair cannot proceed due to insufficient eligible spare nodes
    /// meeting failure domain isolation rules.
    RepairBlockedByCapacity,
    /// Catastrophic loss: surviving shards $< k$ for one or more stripes; data cannot be recovered.
    IrrecoverablyLost,
}

/// Durable states in the repair job lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairJobState {
    Detected,
    Pending,
    CapacityCheck,
    WaitingForCapacity,
    Scheduled,
    Repairing,
    Verifying,
    Completed,
    RetryPending,
    Cancelled,
}

impl RepairJobState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Detected => "Detected",
            Self::Pending => "Pending",
            Self::CapacityCheck => "CapacityCheck",
            Self::WaitingForCapacity => "WaitingForCapacity",
            Self::Scheduled => "Scheduled",
            Self::Repairing => "Repairing",
            Self::Verifying => "Verifying",
            Self::Completed => "Completed",
            Self::RetryPending => "RetryPending",
            Self::Cancelled => "Cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Detected" => Some(Self::Detected),
            "Pending" => Some(Self::Pending),
            "CapacityCheck" => Some(Self::CapacityCheck),
            "WaitingForCapacity" => Some(Self::WaitingForCapacity),
            "Scheduled" => Some(Self::Scheduled),
            "Repairing" => Some(Self::Repairing),
            "Verifying" => Some(Self::Verifying),
            "Completed" => Some(Self::Completed),
            "RetryPending" => Some(Self::RetryPending),
            "Cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A durable repair job tracking a specific `(object_id, shard_id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableRepairJob {
    pub job_id: String,
    pub object_id: String,
    pub shard_id: usize,
    pub state: RepairJobState,
    pub detected_at_unix_secs: u64,
    pub last_state_change_unix_secs: u64,
    pub retry_count: u32,
    pub target_node: Option<NodeId>,
    pub reason: String,
    pub error_msg: Option<String>,
}

impl DurableRepairJob {
    pub fn encode(&self) -> String {
        let mut out = format!(
            "JOB_ID={}\nOBJECT_ID={}\nSHARD_ID={}\nSTATE={}\nDETECTED_AT={}\nUPDATED_AT={}\nRETRIES={}\nREASON={}\n",
            self.job_id,
            self.object_id,
            self.shard_id,
            self.state.as_str(),
            self.detected_at_unix_secs,
            self.last_state_change_unix_secs,
            self.retry_count,
            self.reason
        );
        if let Some(target) = self.target_node {
            out.push_str(&format!("TARGET_NODE={}\n", target.0));
        }
        if let Some(ref err) = self.error_msg {
            out.push_str(&format!("ERROR={}\n", err));
        }
        out
    }

    pub fn decode(s: &str) -> Option<Self> {
        let mut job_id = None;
        let mut object_id = None;
        let mut shard_id = None;
        let mut state = None;
        let mut detected_at = 0u64;
        let mut updated_at = 0u64;
        let mut retries = 0u32;
        let mut reason = String::new();
        let mut target_node = None;
        let mut error_msg = None;

        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                match k {
                    "JOB_ID" => job_id = Some(v.to_string()),
                    "OBJECT_ID" => object_id = Some(v.to_string()),
                    "SHARD_ID" => shard_id = v.parse::<usize>().ok(),
                    "STATE" => state = RepairJobState::parse(v),
                    "DETECTED_AT" => detected_at = v.parse::<u64>().unwrap_or(0),
                    "UPDATED_AT" => updated_at = v.parse::<u64>().unwrap_or(0),
                    "RETRIES" => retries = v.parse::<u32>().unwrap_or(0),
                    "REASON" => reason = v.to_string(),
                    "TARGET_NODE" => target_node = v.parse::<usize>().ok().map(NodeId),
                    "ERROR" => error_msg = Some(v.to_string()),
                    _ => {}
                }
            }
        }

        Some(Self {
            job_id: job_id?,
            object_id: object_id?,
            shard_id: shard_id?,
            state: state?,
            detected_at_unix_secs: detected_at,
            last_state_change_unix_secs: updated_at,
            retry_count: retries,
            target_node,
            reason,
            error_msg,
        })
    }
}

/// Durable coordinator repair journal.
#[derive(Debug)]
pub struct DurableRepairJournal {
    root_dir: PathBuf,
    jobs: RwLock<BTreeMap<String, DurableRepairJob>>,
}

impl DurableRepairJournal {
    /// Opens or initializes the repair journal directory at `<coord_root>/repair_jobs`.
    pub fn open_or_init<P: AsRef<Path>>(coord_root: P) -> Result<Self, StorageError> {
        let root_dir = coord_root.as_ref().join("repair_jobs");
        check_path_has_no_traversal(&root_dir)?;
        if root_dir.exists() {
            ensure_not_symlink(&root_dir)?;
        } else {
            fs::create_dir_all(&root_dir)?;
            sync_directory(coord_root.as_ref())?;
        }

        let mut jobs = BTreeMap::new();
        for entry in fs::read_dir(&root_dir)? {
            let entry = entry?;
            let path = entry.path();
            ensure_not_symlink(&path)?;
            let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if fname.ends_with(".rec") {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Some(mut job) = DurableRepairJob::decode(&content) {
                        // Crash reconciliation: if interrupted in Repairing or Scheduled or Verifying,
                        // reset to Pending for safe retry!
                        match job.state {
                            RepairJobState::Repairing
                            | RepairJobState::Scheduled
                            | RepairJobState::Verifying
                            | RepairJobState::CapacityCheck => {
                                job.state = RepairJobState::Pending;
                                job.error_msg =
                                    Some("interrupted by coordinator restart".to_string());
                                let _ = write_and_sync_file(&path, job.encode().as_bytes());
                            }
                            _ => {}
                        }
                        jobs.insert(job.job_id.clone(), job);
                    }
                }
            }
        }

        Ok(Self {
            root_dir,
            jobs: RwLock::new(jobs),
        })
    }

    pub fn make_job_id(object_id: &str, shard_id: usize) -> String {
        format!("{}_s{:02}", object_id, shard_id)
    }

    fn file_path_for_job(&self, job_id: &str) -> PathBuf {
        let safe = job_id.replace(['/', '\\', '.', ' '], "_");
        self.root_dir.join(format!("{}.rec", safe))
    }

    /// Enqueues a new repair job if an active (incomplete) job does not already exist.
    pub fn enqueue_or_get(
        &self,
        object_id: &str,
        shard_id: usize,
        reason: &str,
        now_unix_secs: u64,
    ) -> Result<DurableRepairJob, StorageError> {
        let job_id = Self::make_job_id(object_id, shard_id);
        let mut jobs = self.jobs.write().unwrap_or_else(|p| p.into_inner());

        if let Some(existing) = jobs.get(&job_id) {
            if existing.state != RepairJobState::Completed
                && existing.state != RepairJobState::Cancelled
            {
                return Ok(existing.clone());
            }
        }

        let new_job = DurableRepairJob {
            job_id: job_id.clone(),
            object_id: object_id.to_string(),
            shard_id,
            state: RepairJobState::Pending,
            detected_at_unix_secs: now_unix_secs,
            last_state_change_unix_secs: now_unix_secs,
            retry_count: 0,
            target_node: None,
            reason: reason.to_string(),
            error_msg: None,
        };

        let file_path = self.file_path_for_job(&job_id);
        write_and_sync_file(&file_path, new_job.encode().as_bytes())?;
        sync_directory(&self.root_dir)?;

        jobs.insert(job_id, new_job.clone());
        Ok(new_job)
    }

    /// Transitions a job to a new state and commits to disk with `fsync`.
    pub fn transition_state(
        &self,
        job_id: &str,
        new_state: RepairJobState,
        target_node: Option<NodeId>,
        error_msg: Option<String>,
        now_unix_secs: u64,
    ) -> Result<DurableRepairJob, StorageError> {
        let mut jobs = self.jobs.write().unwrap_or_else(|p| p.into_inner());
        let job = jobs
            .get_mut(job_id)
            .ok_or_else(|| StorageError::ObjectNotFound(format!("repair job {}", job_id)))?;

        job.state = new_state;
        job.last_state_change_unix_secs = now_unix_secs;
        if target_node.is_some() {
            job.target_node = target_node;
        }
        if error_msg.is_some() {
            job.error_msg = error_msg;
        }
        if new_state == RepairJobState::RetryPending {
            job.retry_count += 1;
        }

        let file_path = self.file_path_for_job(job_id);
        write_and_sync_file(&file_path, job.encode().as_bytes())?;
        sync_directory(&self.root_dir)?;

        Ok(job.clone())
    }

    /// Wakes all jobs currently in `WaitingForCapacity` or `RetryPending` back to `Pending`
    /// (e.g. after a new node joins the cluster).
    pub fn wake_pending_repairs(&self, now_unix_secs: u64) -> Result<usize, StorageError> {
        let mut jobs = self.jobs.write().unwrap_or_else(|p| p.into_inner());
        let mut count = 0;
        for job in jobs.values_mut() {
            if job.state == RepairJobState::WaitingForCapacity
                || job.state == RepairJobState::RetryPending
            {
                job.state = RepairJobState::Pending;
                job.last_state_change_unix_secs = now_unix_secs;
                let file_path = self.file_path_for_job(&job.job_id);
                write_and_sync_file(&file_path, job.encode().as_bytes())?;
                count += 1;
            }
        }
        if count > 0 {
            sync_directory(&self.root_dir)?;
        }
        Ok(count)
    }

    /// Returns a list of all active jobs matching a given state.
    pub fn list_by_state(&self, state: RepairJobState) -> Vec<DurableRepairJob> {
        self.jobs
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .filter(|j| j.state == state)
            .cloned()
            .collect()
    }

    /// Returns all jobs currently managed in the journal.
    pub fn all_jobs(&self) -> Vec<DurableRepairJob> {
        self.jobs
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect()
    }
}
