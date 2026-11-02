# FSP Phase 2: Baseline Audit, Hidden-Information Leakage & Regression Verification

## 1. Phase 1 Baseline Reproduction & Audit Status

Prior to writing any Phase 2 code, the complete repository (`src/**/*.rs`, `tests/*.rs`, `docs/*.md`, and `results/phase1/*`) was audited and executed:

- **Existing Test Suite Verification:** All 29 tests across `tests/determinism.rs` (3), `tests/recoverability.rs` (4), `tests/failure_scenarios.rs` (6), and `tests/phase1_validation.rs` (16) passed (`0 failed`).
- **Phase 1 Artifact Reproduction:** `results/phase1/raw_results.jsonl` and `results/phase1/summary.json` were inspected and verified against `src/main.rs` (`master_seed = 0xf591202600000001`, 52 batches, 232,000 trajectories).
  - Specifically, in `suite4_expanded_10k` at `20%` churn (`batch_index` 49, 50, 51):
    - `StrategyB_FixedRepair_T18`: `exogenous_trace_digest = 0xd46df0ca5949e2b4`, `batch_trace_digest = 0x631d149ab22d04d3`, `irrecoverable_loss_count = 4/10000` (`0.04%`), `mean_repair_bandwidth_bps = 416.080247 B/s` (`8.296` ops).
    - `StrategyB_FixedRepairSuspect_T18`: `exogenous_trace_digest = 0xd46df0ca5949e2b4`, `batch_trace_digest = 0xe6cc0eb7e19da37c`, `irrecoverable_loss_count = 5/10000` (`0.05%`), `mean_repair_bandwidth_bps = 1648.517361 B/s` (`32.8689` ops).
    - `StrategyC_FspAdaptiveRepair`: `exogenous_trace_digest = 0xd46df0ca5949e2b4`, `batch_trace_digest = 0xfccecdd2d9a0cd46`, `irrecoverable_loss_count = 4/10000` (`0.04%`), `mean_repair_bandwidth_bps = 359.902392 B/s` (`7.1759` ops, `-13.50%` vs `FixedRepair_T18`).

---

## 2. Critical Modeling Flaws & Hidden-Information Leakage Identified in Phase 1

Our formal audit of Phase 0 and Phase 1 uncovered **five critical realism gaps and hidden-information leaks** that favored oracle-like repair policies:

### 2.1 Flaw #1 — Hidden Ground-Truth Leakage (`missing_unscheduled_shards` vs. `suspected_offline_shards`)
- **What Happened in Phase 1:**
  - When `EventKind::NodePermanentFailure` executed in `src/simulation/engine.rs`, `cluster.fail_node_permanently()` immediately deleted the shard from `cluster.shards` at the exact instant $t$ of the physical disk crash and set `should_evaluate_strategy = true`.
  - When `EventKind::NodeOutage` executed, the shard remained in `cluster.shards` with `NodeState::TemporarilyOffline`.
  - Worse, `StrategyContext` passed `&ClusterState` directly to `RepairStrategy::evaluate()`, and both `FixedRepairStrategy` and `FspAdaptivePolicy` called `cluster.missing_unscheduled_shards()` (which returned confirmed permanent losses at $t + 0\text{ s}$) separately from `cluster.suspected_offline_shards()`!
- **Why This Is Unrealistic:**
  - In a real decentralized storage network, a coordinator or repair monitor **cannot instantaneously distinguish a permanent disk/host crash from a temporary power or network outage** at $t = 0\text{ s}$ unless an out-of-band hardware attestation or explicit decommission message is sent. Both appear initially as missed heartbeats / failed challenge-response probes.
  - Because `FspAdaptivePolicy` in Phase 1 knew with 100% oracle certainty which missing shards were permanent (`missing_shards`) vs. temporary (`suspected_offline_shards`), its `13.5%` bandwidth reduction over `FixedRepair_T18` relied on ground-truth oracle separation rather than blind failure detection.

### 2.2 Flaw #2 — Instantaneous Replacement-Node Provisioning (`provision_empty_replacement: true`)
- **What Happened in Phase 1:**
  - In `ClusterState::fail_node_permanently()`, when a node suffered a permanent failure at time $t$, its slot was re-provisioned as `NodeState::Online` at **zero delay ($t + 0\text{ s}$)**.
- **Why This Is Unrealistic:**
  - Provisioning replacement storage capacity (onboarding a new storage provider or replacing a failed drive) incurs a non-zero physical replacement delay $\Delta t_{\text{replace}} > 0$ (`PhysicalNodeState::Replacing`), and recovering from a transient outage incurs a recovery/warm-up transition (`PhysicalNodeState::Recovering`).

### 2.3 Flaw #3 — Unprotected Vulnerability to Catastrophic Shocks at High Redundancy (`19/20` Shards)
- **What Happened in Phase 1:**
  - As proven in `test_12_falsification_scenarios_where_adaptive_repair_performs_worse`, when 1 shard was permanently lost (`19/20` shards remaining), `FspAdaptivePolicy` v1 computed a priority score $P \approx 0.05 < \theta_{\text{trigger}} = 0.22$ and deferred repair indefinitely until a second shard failed.
  - When an 8-node permanent burst struck before a second shard failed, `FspAdaptivePolicy` v1 suffered irrecoverable data loss (`19 - 8 = 11 < 12`), whereas eager `FixedRepair(T=19)` survived (`20 - 8 = 12`).
  - Furthermore, once a repair was scheduled on a suspected offline node, if that node recovered *before* the repair completed, the in-flight repair was not cancelled until completion time, wasting repair bandwidth.

### 2.4 Flaw #4 — Single-Pipe Bandwidth Model & Single-Object Isolation
- **What Happened in Phase 1:**
  - Only a single object was simulated per `ClusterState`, and repair bandwidth was modeled as a single cluster-wide pipe $B_{\max}$ rather than enforcing per-node NIC ingress/egress caps alongside multi-object repair queue contention.
  - Additionally, only Reed–Solomon $(n, k)$ codes were modeled, requiring $k = 12$ shard reads for every single-shard repair ($12\times$ read amplification).

### 2.5 Flaw #5 — Stationary Exponential Inter-Arrival Times Only
- **What Happened in Phase 1:**
  - Permanent node failures were rolled using a stationary per-epoch probability $p_{\text{perm}}$, equivalent to a memoryless Exponential lifetime ($\beta = 1$). Non-stationary Weibull wear-out ($\beta > 1$), infant mortality ($\beta < 1$), and piecewise-constant hazard schedules were not supported.

---

## 3. Phase 2 Architectural & Verification Plan

To address every finding while preserving 100% backward compatibility with all 29 Phase 0/1 tests and Phase 1 regression modes:
1. **Strict Ground-Truth Isolation (`src/detection/`):**
   - Separate `PhysicalNodeState` (`Online`, `TemporarilyOffline`, `Recovering`, `PermanentlyFailed`, `Replacing`) from `ObservedNodeState` (`Healthy`, `SuspectedUnavailable`, `ConfirmedUnavailableByPolicy`, `RecoveryObserved`, `Unknown`).
   - Implement heartbeat monitoring (`heartbeat_interval_secs`, `heartbeat_timeout_secs`, `missed_heartbeat_threshold`), active challenge-response probing (`probe_latency_secs`, `network_jitter_secs`, `packet_loss_probability`), correlated domain telemetry (detecting when multiple nodes in the same `FailureDomainId` fail heartbeats simultaneously), and explicit `node_replacement_delay_secs`.
   - Ensure Phase 2 blind strategies receive **only** `ClusterObservationView` (which contains zero `PhysicalNodeState` fields).
2. **Calibrated & Interval Risk Estimation Engine (`src/reliability/risk.rs`):**
   - Compute `RiskEstimate` with explicit lower/upper probability bounds (`uncertainty_interval: (f64, f64)`) and `ConfidenceCalibrationStatus` distinguishing analytical bounds under known hazards from uncalibrated empirical hazard estimates.
3. **FSP Adaptive Repair v2 (`src/strategies/adaptive_v2.rs`):**
   - Combine blind failure detection posterior probability, domain-correlated shock escalation, worst-case burst-tolerance floor ($u_{\text{eff}} - B_{\text{burst\_guard}} \ge k$), max-deferral age bound (so a single lost shard at $19/20$ is repaired after a bounded quiet window rather than left permanently unhealed before a shock!), and **in-flight repair cancellation** when a suspected node recovers (`RecoveryObserved`).
4. **Local Reconstruction Codes (`src/coding/`):**
   - Formalize and implement explicit parity-check structure and decodability conditions for `LRC(20, 12, l=4)` (3 local groups of 4 data shards + 3 local parities + 5 global parities = 20 total shards) and `LRC(21, 12, l=4)` vs `RS(20, 12)`, proving exact information-theoretic minimum distance $d_{\min}$ and arbitrary vs. non-recoverable erasure patterns.
5. **Non-Stationary Hardware Failure Distributions (`src/reliability/distributions.rs`):**
   - Implement seeded `Exponential`, `Weibull { shape_beta, scale_eta_secs }`, `PiecewiseConstantHazard`, and `CorrelatedDomainShock` generators.
6. **Multi-Object Contention & Per-Node Bandwidth Scheduler (`src/simulation/workloads.rs`, `src/simulation/resource_scheduler.rs`):**
   - Support multi-object workloads with heterogeneous sizes and coding schemes competing for shared per-node ingress/egress bandwidth and cluster repair queues.
