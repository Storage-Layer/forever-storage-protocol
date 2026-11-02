# FSP Phase 5.6: Power-Loss Durability & Failure Taxonomy

## 1. Failure Class Taxonomy

To eliminate the conflation between software crashes and hardware power failures, Phase 5.6 formalizes three distinct failure classes:

### Class A: Process Failure (Tested & Verified)
- **Examples:** `SIGKILL` (`kill -9`), unhandled exceptions, daemon abort, coordinator crash.
- **System State:** The operating system kernel remains active. The OS page cache, dirty inodes, and filesystem journal remain preserved in RAM and continue flushing to physical storage in the background.
- **FSP Validation:** Fully verified via [`tests/phase5_6_validation.rs`](file:///home/rade/Project/Jobs/fsp/tests/phase5_6_validation.rs) (Tests 10, 13, 14, 25). Zero data loss observed for acknowledged writes.

### Class B: Operating System or Hypervisor Failure (Untested / NOT EXECUTED)
- **Examples:** Kernel panic (`panic()`), hypervisor crash, forced virtual machine power-off.
- **System State:** Volatile RAM and unwritten page cache are lost instantly. Hardware disk caches may persist data depending on disk controller battery/capacitor backup (`PLP`).
- **Status:** `NOT EXECUTED` (disposable isolated hypervisor environment was not provisioned).

### Class C: True Physical Power Loss (Untested / NOT EXECUTED)
- **Examples:** Sudden utility power loss, UPS failure, pull-plug event on physical server.
- **System State:** All volatile memory in host CPU and RAM disappears. On-drive DRAM write caches without power-loss protection (non-PLP consumer SSDs) can lose unwritten blocks or write torn sectors.
- **Status:** `NOT EXECUTED`.
- **Constraint:** Physical power-cut experiments require dedicated staging hardware with explicit authorization and backup procedures. They were **never** executed on the primary workstation.

---

## 2. Why Process SIGKILL Is Not Proof of Power-Loss Durability

A common error in storage benchmarking is treating `SIGKILL` as a proof of crash durability:
1. When a process terminates abruptly via `SIGKILL`, the Linux kernel closes file descriptors and queues dirty pages for flushing.
2. Even if the process never called `fsync()`, the operating system will flush those pages to disk within `dirty_writeback_centisecs` (default 500 centiseconds = 5s).
3. In a real power cut, the OS has zero time to flush dirty pages. Any data not explicitly flushed via hardware barrier (`fdatasync`/`fsync`) is permanently lost.

FSP guarantees that a write is only reported as `LocallyDurable` after **both** `sync_all()` on the temporary shard file and `sync_all()` on the parent container directory have returned success.
