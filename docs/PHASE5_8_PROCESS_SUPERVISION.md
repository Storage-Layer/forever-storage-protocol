# FSP Phase 5.8: Process Supervision, Pidfd Signaling & Lifecycle Architecture

## 1. Operating System Process Supervision

In a distributed multi-node deployment, storage node daemons run as independent operating system processes (`fsp-node`). Preventing dangling child processes, runaway resource utilization, and PID recycling hazards is critical to operational stability.

### Supervision Mechanisms:
1. **Parent-Death Signal (`PR_SET_PDEATHSIG`):**
   - Subprocesses launched by the cluster manager invoke `libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL)` upon fork.
   - If the parent launcher or test harness terminates abruptly (e.g. `SIGKILL`, OOM panic), the Linux kernel automatically delivers `SIGKILL` to all child daemons.
2. **Linux Pidfd Management (`pidfd_open` / `pidfd_send_signal`):**
   - On Linux 5.3+, process monitoring uses file descriptors bound to specific process instances via `pidfd_open(pid, 0)`.
   - Signaling via `pidfd_send_signal` eliminates the classic Unix PID race condition where a signal intended for a terminated process is mistakenly delivered to a new process that reused the PID.
3. **Systemd Template Units (`deploy/phase5_8/systemd/`):**
   - In production staging environments, systemd provides process supervision via `fsp-node@.service` and `fsp-coordinator.service`.
   - Enforces `LimitNOFILE=65536`, `ProtectSystem=strict`, and `KillMode=mixed`.
4. **Orphan Sweeper Utility (`deploy/phase5_8/supervision/orphan_sweeper.sh`):**
   - Background utility auditing all running `fsp-node` and `fsp-coordinator` processes. Any instance whose PPID is re-parented to `1` (init) without active cluster ownership is safely terminated.

---

## 2. Test Verification

Verified by automated integration test:
- `test_p5_8_20_process_supervision_and_clean_termination` (Passed)
