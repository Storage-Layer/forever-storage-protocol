# FSP Phase 1: Verification Audit & Validation Plan

## 1. Independent Verification of Phase 0 Implementation

Prior to writing any Phase 1 code, all Phase 0 source files (`src/**/*.rs`), integration tests (`tests/*.rs`), and documentation (`docs/*.md`) were audited against their mathematical claims, and the test suite was executed independently (`13 passed; 0 failed`).

### 1.1 Verified Phase 0 Strengths
- **Strict Separation of Transient Outage vs. Permanent Loss:** `ClusterState` and `RecoverabilityStatus` accurately distinguish `AvailableAndRecoverable` ($|\mathcal{A}(t)| \ge k$), `TemporarilyUnavailableButDurable` ($|\mathcal{A}(t)| < k \land |\mathcal{U}(t)| \ge k$), and `IrrecoverablyLost` ($|\mathcal{U}(t)| < k$).
- **Deterministic Min-Heap Ordering:** `ScheduledEvent` orders by `(time_secs, sequence)` via `total_cmp`, guaranteeing deterministic tie-breaking.
- **Zero-Dependency Reproducibility:** `DeterministicRng` (`SplitMix64`) eliminates external crate drift and hidden global state.

### 1.2 Mathematical Inconsistencies & Systematic Biases Identified in Phase 0

Our formal methods review uncovered **six specific mathematical and simulation flaws** in Phase 0 that must be corrected in Phase 1:

1. **Systematic Bias #1 — Replacement Nodes Escape Pre-Scheduled Stochastic Failures (`src/simulation/engine.rs`):**
   - *Flaw:* In Phase 0, `populate_seeded_schedule()` pre-generated per-epoch `NodeOutage` and `NodePermanentFailure` events only for initial node IDs `0..num_nodes`. However, when `fail_node_permanently(..., provision_empty_replacement: true)` executed, it allocated a new node with ID `>= num_nodes` (e.g., `NodeId(50)`, `NodeId(51)`). Because events were only pre-scheduled for `0..50`, **any shard repaired onto a replacement node became completely immune to future node-level transient churn and independent permanent failures** (only subject to whole-domain failures)!
   - *Impact:* Severe upward survival bias for long horizons or high repair counts, artificially flattering any strategy that repairs frequently.
   - *Phase 1 Correction:* Instead of allocating unbounded `NodeId(50+)`, replacement hardware must either **re-provision the same slot** (or draw from a pre-seeded per-slot hazard stream / dynamic next-arrival hazard scheduling) so that every active storage node hosting a repaired shard experiences the exact same stochastic transient churn and permanent failure hazard rate $\lambda_{\text{perm}}$ and $\lambda_{\text{outage}}$. Specifically, we will generate exogenous failure traces indexed by **physical node slot** $v \in \{0, \dots, N-1\}$ where a permanent failure on slot $v$ destroys its hosted shard and replaces the disk/host in slot $v$ with a fresh empty node in the same failure domain, continuing to subject slot $v$ to subsequent exogenous failures in the trace!

2. **Systematic Bias #2 — Coupled PRNG Streams in `populate_seeded_schedule()` (`src/simulation/engine.rs`):**
   - *Flaw:* In Phase 0, inside the loop over `node_idx`, `if p_churn > 0.0 && self.rng.next_f64() < p_churn` conditionally consumed **two extra RNG draws** (`offset` and `duration`) before rolling `if p_perm > 0.0 && self.rng.next_f64() < p_perm`.
   - *Impact:* Changing `churn_scenario` (e.g., from `1%` to `20%`) shifted the PRNG state for `permanent_failure_rate_per_epoch`, meaning different churn scenarios did **not** share the same underlying permanent-failure rolls even with the same `seed`.
   - *Phase 1 Correction:* Implement a dedicated `ExogenousFailureTrace` generator with **orthogonal, domain-separated deterministic PRNG substreams** (derived from `(master_seed, stream_tag)`) for:
     - Stream 1: Independent node permanent failures
     - Stream 2: Independent node transient outages
     - Stream 3: Correlated domain outages
     - Stream 4: Correlated permanent domain failures
     - Stream 5: Multi-node burst failures
     - Stream 6: Policy/controller tie-breaking stream
     This guarantees Common Random Numbers (CRN) variance reduction across strategies and across churn sensitivity sweeps.

3. **Inconsistency #3 — Unconditional strategy evaluation on `NodeOutage` omitted (`src/simulation/engine.rs`):**
   - *Flaw:* In Phase 0 `process_event()`, `EventKind::NodeOutage` did not set `should_evaluate_strategy = true`; strategies were only evaluated on `ControllerCheck`, `NodeRecovery`, `NodePermanentFailure`, or `RepairComplete`. Furthermore, node-level offline time fraction was not explicitly tracked.
   - *Phase 1 Correction:* Track exact node-level online vs. offline time fractions across all node slots, and ensure strategies receive accurate queue/bandwidth telemetry (`repair_channel_free_at_secs`, currently queued repair bytes, and expected repair completion time).

4. **Inconsistency #4 — Single-Run Total-Time-On-Test vs. Restricted Mean Survival Time (RMST) & Kaplan–Meier (`src/reliability/survival.rs`):**
   - *Flaw:* Phase 0 `estimate_empirical_mttdl` computed $\frac{\sum \min(T_{\text{loss}, r}, T_{\max})}{N_{\text{losses}}}$, which is the exponential maximum-likelihood estimator under Type-I censoring, **not** a non-parametric survival estimator, and lacked confidence intervals, Kaplan–Meier product-limit estimation, and Restricted Mean Survival Time ($\text{RMST}(T_{\max}) = \int_0^{T_{\max}} \hat{S}_{\text{KM}}(t)\,dt$).
   - *Phase 1 Correction:* Implement full statistical validation in `src/reliability/statistics.rs` and `src/reliability/survival.rs`:
     - Wilson score confidence intervals for finite-horizon data loss probability $\hat{P}_{\text{loss}}(T_{\max})$.
     - Kaplan–Meier product-limit survival curve $\hat{S}_{\text{KM}}(t)$ with Greenwood's variance / censoring counts.
     - Restricted Mean Survival Time $\text{RMST}(T_{\max})$ over $[0, T_{\max}]$.
     - Explicit refusal to report unrestricted empirical MTTDL when runs are right-censored without sufficient uncensored failures.

5. **Inconsistency #5 — Analytical Independence Assumption Under Correlated Failures (`src/reliability/survival.rs`):**
   - *Flaw:* Phase 0 `analytical_binomial_survival_probability` only accounted for independent per-node permanent failures, silently ignoring correlated domain failures and burst failures.
   - *Phase 1 Correction:* Extend the analytical survival model to explicitly include domain-correlated permanent failure hazard $\lambda_{\text{dom}}$ and burst failure hazard $\lambda_{\text{burst}}$ alongside independent node hazard $\lambda_{\text{node}}$, documenting both the independent and correlated analytical bounds.

6. **Missing Capabilities Required for Phase 1:**
   - Correlated failure generator supporting all 5 failure modes (independent node, domain outage, domain permanent failure, multi-node burst failure, and mixed).
   - Concrete heuristic FSP Adaptive Repair strategy (`PriorityAdaptivePolicy`) with a normalized, dimensionally consistent priority function $P = f(\text{loss\_risk}, \text{repair\_cost}, \text{repair\_latency}, \text{domain\_exposure})$ and explicit deferral of low-risk transient outages.
   - Multi-threaded deterministic Monte Carlo batch runner (`src/simulation/monte_carlo.rs`) producing identical results regardless of thread count (`1` vs. `N` threads) and sharing identical exogenous failure traces across `Replication(2)`, `Replication(3)`, ` RS(20,12) Fixed`, `RS(20,12) Adaptive`, and matched-overhead `RS(24,12)` vs. `Replication(2)`.

---

## 2. Phase 1 Test-Driven Development (TDD) Matrix

Before implementing Phase 1 modules, we will write failing tests in `tests/phase1_validation.rs` covering all 15 mandatory categories plus deterministic randomized property tests:

1. `test_01_monte_carlo_seed_reproducibility`
2. `test_02_worker_count_independent_output`
3. `test_03_identical_exogenous_failure_traces_across_strategies`
4. `test_04_wilson_score_confidence_interval_correctness`
5. `test_05_right_censoring_kaplan_meier_and_rmst_correctness`
6. `test_06_time_weighted_availability_and_node_offline_fraction_correctness`
7. `test_07_correlated_domain_and_burst_failure_behavior`
8. `test_08_adaptive_repair_priority_ordering`
9. `test_09_bandwidth_budget_enforcement_and_queue_accounting`
10. `test_10_repair_queue_deduplication_and_loop_prevention`
11. `test_11_unrecoverable_absorbing_state_invariants`
12. `test_12_falsification_scenarios_where_adaptive_repair_performs_worse`
13. `test_13_zero_failure_analytical_sanity_checks`
14. `test_14_no_repair_vs_active_repair_comparison`
15. `test_15_cross_strategy_storage_overhead_accounting`
16. `test_16_property_based_randomized_invariants_across_100_seeds`
