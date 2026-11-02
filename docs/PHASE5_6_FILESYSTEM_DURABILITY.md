# FSP Phase 5.6: Filesystem Durability & Disk Sync Barriers

## 1. The Journaling Filesystem Persistence Problem

On modern Linux filesystems (ext4, XFS, Btrfs), creating or writing to a file involves two distinct layers:
1. **File Data & Inode Metadata:** Written to dirty page cache blocks.
2. **Directory Entry Metadata (dentry):** Links the file name to its inode in the parent directory.

If an application writes data and calls `close()`, or even calls `fsync()` on the file descriptor alone without syncing the parent directory, a sudden crash or power cut can leave the file detached from the filesystem directory tree, resulting in file loss or orphaned inodes in `lost+found`.

---

## 2. The Four-Step Hardware Barrier Sequence

To eliminate torn files and directory desynchronization, FSP executes a strict 4-step barrier sequence in [`src/storage/container.rs`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs):

```
Step 1: Write payload into temporary file
        File path: `<storage_root>/objects/<object_id>/.tmp.<uuid>.<shard_id>`
               │
               ▼
Step 2: Flush file data & inode to non-volatile disk
        Call: `temp_file.sync_all()`
               │
               ▼
Step 3: Atomic directory link commit
        Call: POSIX `rename(temp_path, final_shard_path)`
               │
               ▼
Step 4: Flush parent directory dentry to disk
        Call: `parent_dir.sync_all()`
```

---

## 3. Failure Behavior at Every Step

| Step | Crash Point | Invariant Maintained |
|:---|:---|:---|
| **During Step 1** | Crash during write | Incomplete temporary file discarded. Destination shard file never created. |
| **During Step 2** | Crash during `sync_all(file)` | Fsync fails closed with `io::Error`. Operation aborted; no ACK returned. |
| **During Step 3** | Crash during `rename()` | POSIX `rename` is atomic in kernel. Either old file exists or new file exists. |
| **During Step 4** | Crash during `sync_all(dir)` | Destination file already on disk; ext4 journal replay resolves directory entry. |

Validated under tests `test_p5_6_06_durable_ack_after_file_and_directory_synchronization` and `test_p5_6_07_rejection_of_failed_filesystem_synchronization`.
