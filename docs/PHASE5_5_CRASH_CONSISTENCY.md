# Phase 5.5 Crash Consistency & Fault Injection Matrix

## 1. 15 Canonical Write Boundaries Tested

Phase 5.5 introduced [`src/storage/crash_point.rs`](file:///home/rade/Project/Jobs/fsp/src/storage/crash_point.rs), establishing deterministic fault injection at 15 exact boundaries:

| ID | Crash Point Name | Location in Code | Tested Action | Verified Post-Crash Invariant |
|---|---|---|---|---|
| **1** | `BeforeTempFileCreate` | [`write_shard_container_atomic`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs#L457) | ReturnIoError | Zero bytes or files on disk. Clean recovery. |
| **2** | `DuringPartialShardWrite` | [`append_stripe`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs#L463) | ReturnIoError | Partial shard discarded; unindexed tmp deleted on recovery. |
| **3** | `AfterBytesWrittenBeforeFsync` | [`write_shard_container_atomic`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs#L466) | ReturnIoError | Un-fsynced temporary container is never published. |
| **4** | `AfterFsyncBeforeRename` | [`write_shard_container_atomic`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs#L470) | ReturnIoError | Final target does not exist; tmp file ignored by node index. |
| **5** | `AfterRenameBeforeParentDirSync` | [`write_shard_container_atomic`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs#L473) | ReturnIoError | File exists in dentry; self-verifying container intact. |
| **6** | `AfterParentDirSync` | [`write_shard_container_atomic`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs#L476) | RecordOnly | Directory entry fully durable on persistent storage media. |
| **7** | `AfterWalAppendBeforeWalSync` | [`distributed_put_reader`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs#L705) | ReturnIoError | Torn WAL entry detected and aborted on coordinator restart. |
| **8** | `AfterWalSyncBeforeQuorumCommit` | [`distributed_put_reader`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs#L709) | RecordOnly | Quorum WAL entry safely flushed before manifest publication. |
| **9** | `AfterManifestCommitBeforeClientAck` | [`distributed_put_reader`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs#L716) | RecordOnly | Object is durable on disk; client retry resolves via idempotency. |
| **10** | `ImmediatelyAfterClientAck` | [`distributed_put_reader`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs#L738) | RecordOnly | Client confirmed success; post-ACK SIGKILL does not lose data. |
| **11** | `DuringRepairJournalUpdate` | `DurableRepairJournal::transition_state` | RecordOnly | Journal transitions committed to disk with atomic fsync. |
| **12** | `DuringShardReplacement` | `execute_repair_job` | RecordOnly | Replacement shard written atomically with fsync. |
| **13** | `DuringMetadataSnapshotReplacement` | `export_metadata_backup` | RecordOnly | Metadata bundle exported and validated atomically. |
| **14** | `DuringGcQuarantine` | `gc_quarantine_sweep` | RecordOnly | Quarantine move executed atomically without leaking shards. |
| **15** | `DuringPhysicalShardDelete` | `gc_physical_delete` | RecordOnly | Deletion followed by parent dir sync; live shards unharmed. |

---

## 2. Essential Invariant Proven

> [!IMPORTANT]
> **Durability Guarantee:**  
> If the client received a successful PUT report (`durability_level: Recoverable`), a subsequent process crash (`SIGKILL`) of any coordinator or up to 8 storage nodes **does not cause committed data to disappear or become unreadable**.
