# FSP Phase 5.7: Process Supervision & PID Reuse Race Elimination

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_PROCESS_SUPERVISION.md`  
**Audited Code:** [`src/cluster/launcher.rs`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs)  
**Machine-Readable Proof:** [`results/phase5_7/process_supervision.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/process_supervision.json)  
**Sweeper Script:** [`deploy/phase5_7/supervision/orphan_sweeper.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/supervision/orphan_sweeper.sh)  

---

## 1. The PID Reuse TOCTOU Race Hazard

In multi-process systems where a supervisor or launcher spawns numerous background worker processes and records their process identifiers (PIDs) in a file (e.g. `cluster.pids`), a dangerous race condition exists:

```
[Time T0]: Launcher spawns child fsp-node (PID 45120). Writes "PID=45120" to cluster.pids.
[Time T1]: Launcher crashes abnormally (SIGKILL / power loss).
[Time T2]: Child process 45120 crashes or terminates.
[Time T3]: The Linux OS spawns an unrelated process (e.g., sshd, database, nginx) and re-assigns PID 45120.
[Time T4]: Supervisor / Sweeper runs cleanup_orphans():
           - Reads cluster.pids (PID 45120).
           - Checks if PID 45120 is alive (kill -0 returns 0: YES!).
           - Inspects /proc/45120/cmdline.
           - [RACE WINDOW]: If process terminates between inspection and kill,
             or if inspection is naive, supervisor sends kill(45120, 9),
             terminating an unrelated, critical system process!
```

---

## 2. Phase 5.7 Hardened Triple-Layer Architecture

In Phase 5.7, [`src/cluster/launcher.rs`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs) was fortified with a triple-layer defense against PID reuse:

### Layer 1: Process Start-Time (`START_TIME`) Binding
The Linux kernel increments a monotonic tick counter for every newly created task, stored in field 22 of `/proc/{pid}/stat`. When `ClusterLauncher` writes `cluster.pids`, it records both PID and `START_TIME`:
```rust
// src/cluster/launcher.rs:565
pub fn update_pids_file(&self) -> Result<(), String> {
    ...
    let start_time_str = Self::get_process_starttime(pid)
        .map(|st| format!(" START_TIME={}", st))
        .unwrap_or_default();
    content.push_str(&format!("NODE={} PID={}{}\n", node_id.0, pid, start_time_str));
}
```
When `audit_orphans` checks a PID, it parses field 22 of `/proc/{pid}/stat`. If the current process's start-time does not match the recorded start-time, the PID was recycled by the OS and is safely skipped!

### Layer 2: Race-Free Kernel Pinning via Linux `pidfd_open` (Syscall 434)
To eliminate the Time-of-Check to Time-of-Use (TOCTOU) gap between inspection and termination, FSP leverages the Linux `pidfd` API:
```rust
// src/cluster/launcher.rs:677
pub fn kill_process_safely(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    unsafe {
        // SYS_pidfd_open = 434: creates an anonymous file descriptor pinned directly
        // to this exact task_struct in the kernel
        let pidfd = syscall(434, pid as i64, 0i64);
        if pidfd >= 0 {
            let fd = pidfd as i32;
            // Verify that the task pinned by pidfd is an FSP daemon
            if !Self::is_fsp_process(pid) {
                close(fd);
                return false;
            }
            // SYS_pidfd_send_signal = 424: delivers SIGKILL directly through pidfd.
            // If the original process died and PID was reused, this call fails with ESRCH.
            // It is mathematically impossible for pidfd_send_signal to kill a recycled task!
            let ret = syscall(424, pidfd, 9i64, 0i64, 0i64);
            close(fd);
            ret == 0
        } else {
            kill(pid as i32, 9) == 0
        }
    }
}
```

### Layer 3: Kernel `PR_SET_PDEATHSIG` Cleanup
At process spawn time, child daemons execute:
```rust
// src/cluster/launcher.rs:482
cmd.pre_exec(|| {
    prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0);
    Ok(())
});
```
When the parent launcher thread dies, the Linux kernel automatically delivers `SIGKILL` directly to the child processes without requiring external polling or supervision.

---

## 3. Verification & Evidence

This defense was validated across two dedicated tests in `tests/phase5_6_validation.rs`:
- `test_p5_6_14_launcher_sigkill_pdeathsig_clean_termination`: Validates that dead launcher PIDs trigger orphan detection.
- `test_p5_6_15_orphan_pid_reuse_safeguard_prevents_killing_unrelated`: Places the test runner's own PID in a mock `cluster.pids` as an orphan. The safeguard correctly identifies that the command line and start time do not match a launcher orphan, preventing self-termination.

Furthermore, the standalone bash sweeper [`deploy/phase5_7/supervision/orphan_sweeper.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/supervision/orphan_sweeper.sh) mirrors this logic for operations outside of the Rust test harness.
