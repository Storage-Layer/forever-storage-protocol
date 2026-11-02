# FSP Phase 5.6: Security Findings, Threat Modeling & Defensive Hardening

## 1. Executive Summary

Phase 5.6 audited the write pipeline, RPC authentication boundaries, process supervision mechanics, and failure classifications. 4 vulnerabilities were identified and completely resolved with automated tests.

---

## 2. Security Findings Matrix

### SEC-5.6-01: Storage Node PutShardAck Identity Mismatch
- **Severity:** High
- **Vulnerability:** The coordinator previously dispatched `PutShard` RPCs to planned nodes, but accepted any incoming successful `PutShardAck` without explicitly verifying that `ack.node_id` matched `target.node_id` or that `ack.shard_id` matched the planned index. A misconfigured or rogue node could acknowledge shards intended for another node, violating anti-affinity.
- **Resolution:** Enforced strict equality checks in [`src/coordinator/transactions.rs`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs):
  - `ack.node_id == target.node_id`
  - `ack.shard_id == planned_shard_id`
  - `ack.object_id == object_id`
  - `ack.version == shard_version`
  - `ack.durability_level >= DurabilityLevel::LocallyDurable`
- **Verification:** `test_p5_6_09_mismatched_shard_or_node_identity_rejection`.

### SEC-5.6-02: Orphan Process Sweeper PID Reuse Hazard
- **Severity:** Medium
- **Vulnerability:** The launcher sweeper previously read PIDs from `cluster.pids` and issued `SIGKILL` without verifying that the running process was still an FSP daemon. If the OS recycled the PID for an unrelated process, the sweeper could kill arbitrary system programs.
- **Resolution:** Added [`ClusterLauncher::is_fsp_process(pid)`](file:///home/rade/Project/Jobs/fsp/src/cluster/launcher.rs) and shell equivalent in `orphan_sweeper.sh` which checks `/proc/{pid}/cmdline` and `/proc/{pid}/comm` before signaling.
- **Verification:** `test_p5_6_15_orphan_pid_reuse_safeguard_prevents_killing_unrelated`.

### SEC-5.6-03: Distinct Shard Counting vs. Node Count Ambiguity
- **Severity:** Medium
- **Vulnerability:** Tracking acknowledgements by `acknowledged_nodes.len()` alone created an ambiguity if multiple shards were collocated or duplicate ACKs were received.
- **Resolution:** Implemented `acknowledged_shards: BTreeSet<usize>` and introduced [`DurabilityContractReport`](file:///home/rade/Project/Jobs/fsp/src/coordinator/transactions.rs) reporting distinct durable shard counts separately from node count.
- **Verification:** `test_p5_6_08_duplicate_shard_ack_does_not_inflate_durable_count`.

### SEC-5.6-04: Process SIGKILL Conflation with Power Loss
- **Severity:** Medium
- **Vulnerability:** Historical documentation occasionally referred to process `SIGKILL` as a test of power-loss durability, obscuring the role of the OS page cache.
- **Resolution:** Established formal 3-tier failure taxonomy: Class A (Process), Class B (OS/VM), and Class C (Power Loss). Explicitly marked Class B and Class C as `NOT EXECUTED`.
- **Verification:** Documented in `docs/PHASE5_6_POWER_LOSS_FAULT_MODEL.md`.
