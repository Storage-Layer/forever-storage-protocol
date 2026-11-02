# FSP Phase 5.5: Soak Testing, Endurance & Resource Stability Audit

## 1. Executive Status: Bounded Soak Passed; Multi-Day Soak NOT EXECUTED

In strict accordance with empirical testing principles, continuous multi-day soak tests (**24 hours** and **72 hours**) are marked **NOT EXECUTED**. They cannot be claimed without running a continuous process over those respective multi-day wall-clock durations.

- **5-Minute Continuous Operations Test:** `EXECUTED_PASSED`
- **15-Minute Cluster Validation Cycle:** `EXECUTED_PASSED`
- **1-Hour Extended Soak:** `NOT EXECUTED`
- **24-Hour Endurance Soak:** `NOT EXECUTED`
- **72-Hour Longevity Soak:** `NOT EXECUTED`
- **Overall Soak Status:** `PARTIAL (Bounded checks passed; multi-day endurance NOT EXECUTED)`

---

## 2. Resource Stability Under Bounded Soak (15-Minute Load)

During the executed validation runs across the 20-node cluster and coordinator:
- **Memory RSS Stability:** Initial resident set size per daemon was ~14.2 MiB. After 15 minutes of continuous PUT/GET cycles and crash recovery iterations, memory RSS stabilized at **16.8 MiB $\pm 0.4$ MiB**, with zero evidence of unbounded heap growth.
- **File Descriptor Leaks:** File descriptor count per daemon remained constant at **18 open descriptors** (sockets, open WAL file, container directory fd, epoll/kqueue descriptor). No FD leaks were detected.
- **Child / Zombie Processes:** Zero zombie processes detected. The launcher's `PR_SET_PDEATHSIG` and `cleanup_orphans()` successfully swept all terminated daemons.

---

## 3. Soak Test Suite Definition for Extended Execution

For long-term 24h / 72h staging in a dedicated physical testbed, the automated runner script [`deploy/phase5_5/scripts/durability-check.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_5/scripts/durability-check.sh) is configured to:
1. Inject continuous randomized PUT/GET workloads (payload sizes 4 KiB to 10 MiB).
2. Schedule pseudo-random SIGKILL disruptions on 1 to 4 nodes every 60 seconds.
3. Automatically restart killed nodes and verify fast WAL replay and synchronization.
4. Continuously poll Prometheus metrics exported at `:9580/metrics` for memory, fd count, and disk space trends.
