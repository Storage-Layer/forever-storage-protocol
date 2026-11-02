# Phase 5.5 Process Supervision, Launcher SIGKILL & Orphan Protection

## 1. Problem Statement: The Rust RAII Limitation

In Phase 5.4, cluster processes were managed by `ClusterLauncher` with an `impl Drop` block that called `kill_sigkill()` on child processes. However, when a launcher process receives an unhandled `SIGKILL` (signal 9) or abruptly crashes:
- The Linux kernel terminates the parent process immediately without running user-space code.
- Rust destructors (`Drop::drop`) **never execute**.
- Child daemons (`fsp-node`) are reparented to `init` (PID 1) or `systemd --user`, continuing to hold TCP ports (`9000..9119`) and disk locks as unmanaged orphans.

## 2. Engineering Corrections Implemented in Phase 5.5

In [`src/cluster/launcher.rs`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs), two complementary supervision mechanisms were introduced:

### 2.1 Linux Kernel `PR_SET_PDEATHSIG`
When spawning child processes on Linux, `CommandExt::pre_exec` configures parent-death signaling:
```rust
#[cfg(target_os = "linux")]
unsafe {
    use std::os::unix::process::CommandExt;
    cmd.pre_exec(|| {
        extern "C" {
            fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
        }
        const PR_SET_PDEATHSIG: i32 = 1;
        const SIGKILL: u64 = 9;
        let ret = prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0);
        if ret != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    });
}
```
**Effect:** When the launcher process terminates for ANY reason (including uncatchable `SIGKILL`), the Linux kernel automatically delivers `SIGKILL` to all child storage nodes.

### 2.2 On-Disk PID Manifest & Orphan Sweeping
- A durable manifest (`cluster.pids`) is written at `<cluster_root>/cluster.pids` recording the parent launcher PID and each child PID.
- [`ClusterLauncher::audit_orphans`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs#L560) inspects `cluster.pids` on startup; if the recorded launcher is dead, any surviving child processes are identified.
- [`ClusterLauncher::cleanup_orphans`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs#L605) safely terminates identified orphans with `SIGKILL` and cleans the PID file before restarting the cluster, preventing port collision bugs.

## 3. Test Verification

Tests [`test_p5_5_10`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs#L440) and [`test_p5_5_11`](file:///home/rade/Project/Jobs/fsp/tests/phase5_5_validation.rs#L455) verified that orphan detection and cleanup execute deterministically without leaking background daemons or affecting unrelated processes.
