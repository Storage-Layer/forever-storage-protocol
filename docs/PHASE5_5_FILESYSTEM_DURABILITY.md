# Phase 5.5 Linux Filesystem Durability & Disk Barrier Analysis

## 1. Filesystem Publication Ordering

In Linux filesystems (ext4, xfs, btrfs), atomic file replacement requires an explicit multi-step sequence to guarantee crash-consistent publication across unexpected power outages or operating system crashes:

```text
1. Open staging temporary file (O_NOFOLLOW | O_CREAT | O_WRONLY)
      ↓
2. Write full payload and container index headers
      ↓
3. Flush user-space buffers (file.flush())
      ↓
4. Force disk barrier sync (file.sync_all() / fsync)
      ↓
5. Atomically rename into final path (fs::rename())
      ↓
6. Force directory entry sync (dir.sync_all() on parent directory)
      ↓
7. Issue durable network acknowledgement (PutShardAck)
```

## 2. Distinction Between Buffered vs Fsync-Committed Writes

The benchmark in [`tests/phase5_5_validation.rs:test_p5_5_22`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs#L765) demonstrated the true physical cost of disk barriers on an NVMe SSD:

| Write Mode | Mechanism | Measured Throughput (10x 1MB) | Measured Duration | Hardware Barrier Enforced? |
|---|---|---|---|---|
| **Buffered Write** | `write_all()` (OS page cache) | **2744.5 MiB/s** | 3.82 ms | **NO** (volatile in RAM) |
| **LocallyDurable Write** | `write_all()` + `sync_all()` | **248.7 MiB/s** | 42.15 ms | **YES** (flushed to non-volatile NAND) |

### Overhead Analysis:
Enforcing a genuine hardware barrier via `fsync` / `sync_all()` introduces an **~11.03x latency overhead**. Previous benchmark reports claiming multi-gigabyte PUT throughput were measuring memory-speed buffered writes to the Linux kernel page cache rather than durable storage.

## 3. Supported Filesystems & Mount Assumptions

- **Primary Supported:** `ext4` (with `barrier=1` default), `xfs`.
- **Assumptions:**
  - Storage controllers honor flush/FUA commands (`sync_all()`).
  - Disks do not exhibit silent write-cache loss or lying hardware firmware.
  - Physical disk damage or controller burn-out requires quorum replacement from independent failure domains.
