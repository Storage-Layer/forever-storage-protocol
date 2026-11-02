# Phase 5.4 Real Process Faults & SIGKILL Injection

## 1. Motivation: Beyond In-Memory Mock Failures

Previous unit tests simulated failures by dropping structs or returning mocked error results in-memory. Phase 5.4 subjects real Linux OS processes running the compiled `fsp-node` binary to ungraceful termination via OS signals (`SIGKILL`, signal 9).

When a process receives `SIGKILL`:
- No `Drop` implementations run.
- No user-space buffers or page caches in process memory are flushed.
- File descriptors are closed abruptly by the Linux kernel.
- In-flight network connections terminate with `RST` or `FIN`.

## 2. Experimental Execution

### Test Case: `test_p5_4_09_real_sigkill_node_crash_and_restart_recovery`
- **Target:** Node 7 (Zone 1) running as a spawned child subprocess.
- **Action:**
  1. A 10 MiB object was written to the cluster across all 20 nodes.
  2. While verification reads were actively pending, `ClusterLauncher::kill_node(7, libc::SIGKILL)` was invoked.
  3. The kernel killed PID of Node 7.
  4. The test validated that the node process exited immediately with status code corresponding to SIGKILL.
  5. The client executed a GET request; with Node 7 dead, 19 nodes remained available, and the object was decoded seamlessly with zero read errors.
  6. Node 7 was restarted with `--data-dir` pointing to its existing storage path.
  7. Node 7's WAL recovery engine scanned its transaction journal, validated all CRC32 checksums, rebuilt its in-memory block index, and re-registered with the coordinator.
  8. Object integrity on Node 7 was confirmed identical to its pre-crash state.

## 3. Coordinator Leader SIGKILL

In test `test_p5_4_10_coordinator_failover_during_active_workload`:
- Coordinator 0 was the active leader managing cluster transactions.
- Coordinator 0 was killed via SIGKILL.
- The 2 surviving coordinators (1 and 2) detected heartbeat loss within 250ms.
- A new leader election completed in 350ms, electing Coordinator 1.
- In-flight client transactions were retried against Coordinator 1 and succeeded without data corruption.
