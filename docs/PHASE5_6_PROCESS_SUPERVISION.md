# FSP Phase 5.6: Process Supervision, `PR_SET_PDEATHSIG` & PID Reuse Protection

## 1. Problem Statement: Orphan Daemon Leaks & PID Reuse Risks

When integration tests or cluster orchestration scripts are abruptly terminated via `SIGKILL`, the kernel reparents surviving child processes to `init`/`systemd` (PID 1). In distributed storage testing, these lingering daemons hold open TCP ports (`9500..9520`), causing subsequent cluster runs to fail with `EADDRINUSE`.

However, naive cleanup scripts that terminate processes based solely on recorded PIDs introduce a severe hazard: **PID Reuse**. If the Linux kernel recycles an old PID and allocates it to an unrelated system process (such as a shell or system daemon), blindly issuing `kill -9 $pid` causes silent system instability.

---

## 2. Two-Layer Defensive Architecture

### Layer 1: Kernel-Enforced Parent-Death Signal (`PR_SET_PDEATHSIG`)
In [`src/cluster/launcher.rs`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs), every spawned child storage daemon is configured with:
```rust
#[cfg(target_os = "linux")]
unsafe {
    cmd.pre_exec(|| {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
        Ok(())
    });
}
```
When the parent launcher thread or test runner exits—whether gracefully, by panic, or via `SIGKILL`—the Linux kernel automatically and atomically delivers `SIGKILL` to the child daemon.

### Layer 2: Verified Process Sweeping with PID Reuse Protection
Before terminating any process discovered in `.fsp_cluster/cluster.pids`, [`ClusterLauncher::cleanup_orphans`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs) and [`orphan_sweeper.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_6/supervisor/orphan_sweeper.sh) verify the target process identity:

```rust
pub fn is_fsp_process(pid: u32) -> bool {
    let cmdline_path = format!("/proc/{}/cmdline", pid);
    if let Ok(cmdline) = fs::read_to_string(&cmdline_path) {
        cmdline.contains("fsp-node") || cmdline.contains("fsp") || cmdline.contains("target/debug")
    } else {
        let comm_path = format!("/proc/{}/comm", pid);
        if let Ok(comm) = fs::read_to_string(&comm_path) {
            comm.contains("fsp-node") || comm.contains("fsp")
        } else {
            false
        }
    }
}
```

If the PID was recycled by the OS for an unrelated command line, the sweeper **skips the process**, preventing unintended process termination.

Tested in `test_p5_6_14_launcher_sigkill_pdeathsig_clean_termination` and `test_p5_6_15_orphan_pid_reuse_safeguard_prevents_killing_unrelated`.
