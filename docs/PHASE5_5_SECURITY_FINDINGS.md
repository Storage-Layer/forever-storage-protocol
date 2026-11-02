# FSP Phase 5.5: Security Findings, Threat Modeling & Defensive Hardening

## 1. Executive Summary

Phase 5.5 conducted an in-depth security and threat analysis targeting the crash-consistency boundary, cryptographic custody, transport isolation, and process lifecycle controls of FSP.

All 4 identified vulnerabilities were completely resolved with verified automated tests.

---

## 2. Security Audit Findings & Resolution Matrix

### Finding SEC-5.5-01: Durability Boundary Ambiguity
- **Severity:** High
- **Vulnerability:** Prior to Phase 5.5, PUT acknowledgments did not communicate the explicit level of persistence achieved on storage nodes. Clients could conflate OS buffered writes in page cache with disk-persisted data, risking silent data loss during node power drops.
- **Resolution:** Added explicit [`DurabilityLevel`](file:///home/rade/Project/Jobs/fsp/src/storage/backend.rs) enum (`Received`, `Buffered`, `LocallyDurable`, `QuorumCommitted`, `Recoverable`). Nodes now explicitly return `LocallyDurable` only after non-volatile `sync_all()`, and coordinators issue `Recoverable` only upon quorum confirmation.

### Finding SEC-5.5-02: Directory Metadata Desynchronization
- **Severity:** High
- **Vulnerability:** Writing a file to disk and calling `sync_all()` on the file descriptor ensures the inode data and size are flushed, but on ext4/XFS filesystems, the directory entry referencing the newly created file can remain cached in RAM. If power fails immediately following a rename, the file becomes unreachable or is orphaned to `lost+found`.
- **Resolution:** Enforced full 4-step barrier sequence in [`src/storage/container.rs`](file:///home/rade/Project/Jobs/fsp/src/storage/container.rs) and [`src/node/daemon.rs`](file:///home/rade/Project/Jobs/fsp/src/node/daemon.rs):
  1. Write payload to temporary file.
  2. Call `fdatasync`/`fsync` on the temporary file descriptor.
  3. Atomically rename temporary file to destination path.
  4. Open and call `fsync` on the parent directory descriptor.

### Finding SEC-5.5-03: Process Supervision & Orphan Daemon Leaks
- **Severity:** Medium
- **Vulnerability:** When a cluster launcher process was abruptly terminated via `SIGKILL`, child daemon subprocesses were reparented to `init`/`systemd`, continuing to bind network ports (`9500..9520`) and causing cascading port collision errors during subsequent test cycles.
- **Resolution:**
  1. Configured Linux `PR_SET_PDEATHSIG` with `SIGKILL` on child process creation via `CommandExt::pre_exec`. If the parent test process dies for any reason, the Linux kernel automatically delivers `SIGKILL` to all child daemons.
  2. Implemented PID-manifest tracking (`cluster.pids`) and orphan sweeping functions [`audit_orphans`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs) and [`cleanup_orphans`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs).

### Finding SEC-5.5-04: Integrity Algorithm Ambiguity
- **Severity:** Medium
- **Vulnerability:** Historical documentation intermittently referenced CRC32C and BLAKE3, creating ambiguity regarding the system's actual cryptographic digest guarantees and tamper resistance.
- **Resolution:** Verified that the Rust codebase strictly and uniformly utilizes pure Rust **NIST FIPS 180-4 SHA-256** for all shard payloads, manifest canonicalization, and object verification, signed with **RFC 8032 Ed25519**. Non-cryptographic checksums were discarded.

---

## 3. Threat Model Status

| Threat Scenario | Mitigation Mechanism | Verification Test |
|:---|:---|:---|
| **Man-in-the-Middle Network Tampering** | Mutual TLS 1.3 with peer certificate SAN and role validation. | `test_mtls_connection_pool_handshake` |
| **Split-Brain Write Partition** | Monotonic epoch fencing (`EpochFencingRegistry`); writes with stale epochs are rejected. | `test_node_side_epoch_fencing` |
| **Torn File Write on Node Crash** | Temporary file staging + atomic POSIX rename + parent dir sync. | `test_crash_point_mid_write_atomic_repair` |
| **Coordinator Crash During 2PC Commit** | Write-Ahead Log (WAL) synchronous commit before client ack. | `test_crash_point_wal_sync` |
| **Silent Bit Rot / Storage Corruption** | Pre-reconstruction SHA-256 verification of every shard. | `test_eight_shard_parallel_corruption_recovery` |
