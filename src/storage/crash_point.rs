//! Deterministic Crash-Point Injection Framework for Phase 5.5 (`crash_point`).
//!
//! Provides 15 deterministic crash points corresponding to exact write and commit
//! boundaries across the FSP storage lifecycle, enabling repeatable process crash
//! and filesystem crash-consistency validation without relying on arbitrary sleep delays.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::RwLock;

/// The 15 canonical crash points mandated for Phase 5.5 crash consistency verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CrashPoint {
    /// 1. Before temporary file creation.
    BeforeTempFileCreate = 0,
    /// 2. During partial shard payload write.
    DuringPartialShardWrite = 1,
    /// 3. After all bytes written but before file synchronization (fsync).
    AfterBytesWrittenBeforeFsync = 2,
    /// 4. After file synchronization (fsync) but before atomic rename.
    AfterFsyncBeforeRename = 3,
    /// 5. After atomic rename but before parent-directory synchronization.
    AfterRenameBeforeParentDirSync = 4,
    /// 6. After parent-directory synchronization.
    AfterParentDirSync = 5,
    /// 7. After metadata WAL append but before WAL synchronization.
    AfterWalAppendBeforeWalSync = 6,
    /// 8. After WAL synchronization but before quorum commit.
    AfterWalSyncBeforeQuorumCommit = 7,
    /// 9. After manifest quorum commit but before client acknowledgement.
    AfterManifestCommitBeforeClientAck = 8,
    /// 10. Immediately after a successful client acknowledgement.
    ImmediatelyAfterClientAck = 9,
    /// 11. During repair journal update.
    DuringRepairJournalUpdate = 10,
    /// 12. During shard replacement.
    DuringShardReplacement = 11,
    /// 13. During metadata snapshot replacement.
    DuringMetadataSnapshotReplacement = 12,
    /// 14. During garbage collection quarantine.
    DuringGcQuarantine = 13,
    /// 15. During physical shard deletion.
    DuringPhysicalShardDelete = 14,
}

impl CrashPoint {
    pub const ALL: [CrashPoint; 15] = [
        CrashPoint::BeforeTempFileCreate,
        CrashPoint::DuringPartialShardWrite,
        CrashPoint::AfterBytesWrittenBeforeFsync,
        CrashPoint::AfterFsyncBeforeRename,
        CrashPoint::AfterRenameBeforeParentDirSync,
        CrashPoint::AfterParentDirSync,
        CrashPoint::AfterWalAppendBeforeWalSync,
        CrashPoint::AfterWalSyncBeforeQuorumCommit,
        CrashPoint::AfterManifestCommitBeforeClientAck,
        CrashPoint::ImmediatelyAfterClientAck,
        CrashPoint::DuringRepairJournalUpdate,
        CrashPoint::DuringShardReplacement,
        CrashPoint::DuringMetadataSnapshotReplacement,
        CrashPoint::DuringGcQuarantine,
        CrashPoint::DuringPhysicalShardDelete,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Self::BeforeTempFileCreate => "BeforeTempFileCreate",
            Self::DuringPartialShardWrite => "DuringPartialShardWrite",
            Self::AfterBytesWrittenBeforeFsync => "AfterBytesWrittenBeforeFsync",
            Self::AfterFsyncBeforeRename => "AfterFsyncBeforeRename",
            Self::AfterRenameBeforeParentDirSync => "AfterRenameBeforeParentDirSync",
            Self::AfterParentDirSync => "AfterParentDirSync",
            Self::AfterWalAppendBeforeWalSync => "AfterWalAppendBeforeWalSync",
            Self::AfterWalSyncBeforeQuorumCommit => "AfterWalSyncBeforeQuorumCommit",
            Self::AfterManifestCommitBeforeClientAck => "AfterManifestCommitBeforeClientAck",
            Self::ImmediatelyAfterClientAck => "ImmediatelyAfterClientAck",
            Self::DuringRepairJournalUpdate => "DuringRepairJournalUpdate",
            Self::DuringShardReplacement => "DuringShardReplacement",
            Self::DuringMetadataSnapshotReplacement => "DuringMetadataSnapshotReplacement",
            Self::DuringGcQuarantine => "DuringGcQuarantine",
            Self::DuringPhysicalShardDelete => "DuringPhysicalShardDelete",
        }
    }
}

/// Action to execute when an armed crash point is hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashAction {
    /// Injects an I/O error (`ErrorKind::Other`) simulating write/fsync failure.
    ReturnIoError,
    /// Abruptly exits process via SIGKILL.
    KillProcess,
    /// Records the hit counter without interrupting execution.
    RecordOnly,
}

struct ArmedState {
    point: CrashPoint,
    action: CrashAction,
}

static ARMED_CRASH_POINT: RwLock<Option<ArmedState>> = RwLock::new(None);
static CRASH_HITS: [AtomicUsize; 15] = [
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
    AtomicUsize::new(0),
];

/// Arms a specific crash point with the specified action.
pub fn arm_crash_point(point: CrashPoint, action: CrashAction) {
    let mut lock = ARMED_CRASH_POINT.write().unwrap_or_else(|p| p.into_inner());
    *lock = Some(ArmedState { point, action });
}

/// Disarms any active crash point.
pub fn disarm_crash_point() {
    let mut lock = ARMED_CRASH_POINT.write().unwrap_or_else(|p| p.into_inner());
    *lock = None;
}

/// Returns true if `point` is currently armed.
pub fn is_crash_point_armed(point: CrashPoint) -> bool {
    let lock = ARMED_CRASH_POINT.read().unwrap_or_else(|p| p.into_inner());
    lock.as_ref().map(|s| s.point == point).unwrap_or(false)
}

/// Triggers crash point inspection at an internal execution boundary.
pub fn trigger_crash_point(point: CrashPoint) -> Result<(), io::Error> {
    let idx = point as usize;
    CRASH_HITS[idx].fetch_add(1, Ordering::Relaxed);

    let lock = ARMED_CRASH_POINT.read().unwrap_or_else(|p| p.into_inner());
    if let Some(armed) = lock.as_ref() {
        if armed.point == point {
            match armed.action {
                CrashAction::ReturnIoError => {
                    return Err(io::Error::other(format!(
                        "Injected crash fault at crash point: {}",
                        point.name()
                    )));
                }
                CrashAction::KillProcess => {
                    #[cfg(target_os = "linux")]
                    unsafe {
                        extern "C" {
                            fn kill(pid: i32, sig: i32) -> i32;
                            fn getpid() -> i32;
                        }
                        kill(getpid(), 9);
                    }
                    std::process::exit(137);
                }
                CrashAction::RecordOnly => {}
            }
        }
    }
    Ok(())
}

/// Returns the recorded hit count for a specific crash point.
pub fn get_crash_point_hits(point: CrashPoint) -> usize {
    CRASH_HITS[point as usize].load(Ordering::Relaxed)
}

/// Resets all crash point hit counters.
pub fn reset_crash_point_hits() {
    for hit in &CRASH_HITS {
        hit.store(0, Ordering::Relaxed);
    }
}
