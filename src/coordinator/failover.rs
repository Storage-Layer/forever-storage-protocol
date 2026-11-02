//! Active-Passive Coordinator Failover Orchestrator (`src/coordinator/failover.rs`).
//!
//! Coordinates role management, quorum elections, standby replay, and repair journal
//! reconciliation upon leader failover or uncoordinated process crash.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::coordinator::fencing::FencingToken;
use crate::coordinator::leadership::{CoordinatorId, CoordinatorRole, QuorumState};
use crate::coordinator::repair_journal::{DurableRepairJournal, RepairJobState};
use crate::coordinator::replication::{MetadataReplicationWal, ReplicationRecord};
use crate::security::signatures::TrustedSignerPolicy;
use crate::storage::backend::StorageError;

/// Performance and operational telemetry from failover events.
#[derive(Debug, Default)]
pub struct FailoverTelemetry {
    pub elections_started: AtomicU64,
    pub elections_won: AtomicU64,
    pub failovers_completed: AtomicU64,
    pub last_failover_duration_ms: AtomicU64,
    pub last_unavailability_duration_ms: AtomicU64,
}

/// Orchestrates active-passive coordinator failover and metadata state reconciliation.
pub struct CoordinatorFailoverManager {
    pub quorum: Arc<QuorumState>,
    pub wal: Arc<MetadataReplicationWal>,
    pub repair_journal: Arc<DurableRepairJournal>,
    pub signer_policy: Arc<Mutex<TrustedSignerPolicy>>,
    pub telemetry: Arc<FailoverTelemetry>,
}

impl CoordinatorFailoverManager {
    pub fn new(
        quorum: Arc<QuorumState>,
        wal: Arc<MetadataReplicationWal>,
        repair_journal: Arc<DurableRepairJournal>,
        signer_policy: Arc<Mutex<TrustedSignerPolicy>>,
    ) -> Self {
        Self {
            quorum,
            wal,
            repair_journal,
            signer_policy,
            telemetry: Arc::new(FailoverTelemetry::default()),
        }
    }

    /// Returns the current coordinator role.
    pub fn current_role(&self) -> CoordinatorRole {
        self.quorum.current_role()
    }

    /// Generates an active fencing token if this coordinator is the authoritative leader.
    pub fn issue_fencing_token(&self, unix_secs: u64) -> Result<FencingToken, StorageError> {
        let role = self.quorum.current_role();
        match role {
            CoordinatorRole::Leader { epoch, .. } => Ok(FencingToken::new(
                epoch,
                self.quorum.local_id.clone(),
                unix_secs,
            )),
            _ => Err(StorageError::QuorumUnavailable(
                "cannot issue fencing token: coordinator is not the authoritative leader".into(),
            )),
        }
    }

    /// Executes failover transition from Standby to Leader.
    ///
    /// 1. Solicits votes from configured quorum members for proposed epoch.
    /// 2. If majority is achieved, advances durable epoch store to proposed epoch.
    /// 3. Reconciles the durable repair journal: any interrupted `Repairing` jobs are reset to `Pending`.
    /// 4. Replays any unapplied replication records from WAL.
    pub fn execute_failover(
        &self,
        proposed_epoch: u64,
        unix_secs: u64,
        peer_responses: Vec<(CoordinatorId, bool)>,
    ) -> Result<CoordinatorRole, StorageError> {
        let start = Instant::now();
        self.telemetry
            .elections_started
            .fetch_add(1, Ordering::SeqCst);

        // 1. Start election and vote for self
        let _req = self.quorum.start_election(proposed_epoch)?;

        // 2. Deliver votes
        let mut final_role = self.quorum.current_role();
        for (voter, granted) in peer_responses {
            final_role = self.quorum.record_vote(voter, granted, unix_secs)?;
            if final_role.is_leader() {
                break;
            }
        }

        if !final_role.is_leader() {
            return Err(StorageError::QuorumUnavailable(format!(
                "election failed: quorum majority not achieved for epoch {}",
                proposed_epoch
            )));
        }

        self.telemetry.elections_won.fetch_add(1, Ordering::SeqCst);

        // 3. Reconcile repair journal: reset incomplete Repairing jobs to Pending
        let all_jobs = self.repair_journal.all_jobs();
        for job in all_jobs {
            if job.state == RepairJobState::Repairing || job.state == RepairJobState::CapacityCheck
            {
                let _ = self.repair_journal.transition_state(
                    &job.job_id,
                    RepairJobState::Pending,
                    None,
                    Some("reconciled after coordinator failover".to_string()),
                    unix_secs,
                );
            }
        }

        // 4. Standby metadata catch-up: replay WAL records to ensure local policy is synchronized
        let records = self.wal.read_all()?;
        let mut policy = self.signer_policy.lock().unwrap();
        for rec in records {
            if let ReplicationRecord::GenerationWatermarkAdvanced {
                object_id,
                generation,
            } = rec
            {
                policy.record_generation_watermark(&object_id, generation);
            }
        }

        let dur_ms = start.elapsed().as_millis() as u64;
        self.telemetry
            .last_failover_duration_ms
            .store(dur_ms, Ordering::SeqCst);
        self.telemetry
            .last_unavailability_duration_ms
            .store(dur_ms, Ordering::SeqCst);
        self.telemetry
            .failovers_completed
            .fetch_add(1, Ordering::SeqCst);

        Ok(final_role)
    }

    /// Resigns leadership gracefully and transitions to Standby or Fenced.
    pub fn resign_leadership(&self, reason: &str, superseding_epoch: Option<u64>) {
        self.quorum
            .step_down_to_fenced(reason.to_string(), superseding_epoch);
    }
}
