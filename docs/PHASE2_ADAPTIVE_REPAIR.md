# FSP Phase 2: Adaptive Repair v2 (`FspAdaptiveV2`) Design & Falsification Resolution

## 1. Resolving the Phase 1 Catastrophic Shock Vulnerability (`19/20` Shards)

In Phase 1 (`test_12_falsification_scenarios_where_adaptive_repair_performs_worse`), when a single shard was lost at $t = 100\text{ s}$ (leaving $19/20$ durable shards for $\text{RS}(20, 12)$), `FspAdaptivePolicy` v1 deferred repairing that shard indefinitely because its static priority score $P \approx 0.05 < \theta_{\text{trigger}} = 0.22$. When an 8-node permanent burst struck later, `FspAdaptivePolicy` v1 suffered irrecoverable data loss ($19 - 8 = 11 < 12$), whereas eager `FixedRepair(T=19)` survived ($20 - 8 = 12$).

### 1.1 Why Indefinite Deferral at $19/20$ Failed
Deferring a single-shard repair briefly during a transient churn spike is beneficial because:
1. If the missing shard was merely a transient outage, it recovers on its own without spending repair bandwidth.
2. If a second shard fails shortly thereafter, both can be repaired together.

However, **indefinite deferral** leaves the object permanently degraded at $u = 19$, permanently reducing its worst-case simultaneous burst tolerance from $n - k = 8$ nodes down to $7$ nodes!

### 1.2 Six Candidate Protections Evaluated & Integrated in `FspAdaptiveV2`

`FspAdaptiveV2` (`src/strategies/adaptive_v2.rs`) implements a configurable `ShockProtectionMode` supporting all six candidate mechanisms:

1. **Conservative Minimum Shard Margin (`min_safety_margin_shards`):**
   - Enforces a hard floor $u_{\text{floor}} = k + M_{\text{safety}}$. Whenever conservative durable count $u_{\text{low}} \le u_{\text{floor}}$, emergency repair is triggered immediately.
2. **Worst-Case Burst Tolerance Constraint (`worst_case_burst_tolerance` $B_{\text{burst}}$):**
   - If the protocol is configured to survive an instantaneous $B_{\text{burst}}$-node shock (e.g., $B_{\text{burst}} = 8$ for $\text{RS}(20, 12)$), any confirmed permanent shard loss that drops $u_{\text{low}} < k + B_{\text{burst}}$ must be repaired as soon as the repair channel is idle and the failure is confirmed (or after a bounded confirmation window $\tau_{\text{max\_defer}}$).
3. **Bounded Deferral Age / Hybrid Eager-Idle Repair (`max_deferral_secs`):**
   - Instead of deferring a $19/20$ confirmed loss forever, `FspAdaptiveV2` defers a non-critical single-shard loss for at most $\tau_{\text{max\_defer}}$ seconds (or repairs it opportunistically as soon as the repair bandwidth queue is idle and posterior permanent failure probability $\hat{p}_{\text{perm}}(i) \ge p_{\text{confirm}}$).
   - This restores full $20/20$ redundancy before later shocks arrive while still filtering out short transient outages!
4. **Predicted Short-Horizon Loss Risk Escalation (`risk_threshold`):**
   - Uses the Phase 2 `RiskEstimate` upper bound $P_{\max}(\Delta t_{\text{eval}})$; if $P_{\max} \ge \theta_{\text{risk}}$, all confirmed and high-posterior suspected shards are scheduled immediately.
5. **Domain-Level Correlated Hazard Escalation (`domain_correlation_escalation`):**
   - When `FailureDetector` reports a `CorrelatedDomainEvent` (or when `domain_concentration_score` $E_{\text{dom}}$ increases), the trigger threshold is lowered by factor $(1 - 0.5 E_{\text{dom}})$.
6. **Proactive Cancellation of Redundant In-Flight Repairs (`cancel_on_recovery`):**
   - When a node hosting a shard with an in-flight repair transitions to `ObservedNodeState::RecoveryObserved`, `FspAdaptiveV2` emits a `CancelRepairs(Vec<ShardId>)` directive (or the engine aborts the queued/in-flight transfer immediately), reclaiming the unspent repair bandwidth for genuinely lost shards.

---

## 2. Documented Heuristic Objective Function in `FspAdaptiveV2`

> **Explicit Heuristic Disclosure:** `FspAdaptiveV2` optimizes a multi-criteria risk-cost heuristic objective at each decision epoch; it is not claimed to be a globally optimal solution to a partially observable Markov decision process (POMDP).

For each candidate unreachable shard $i \in \{0, \dots, n-1\}$ with observable silence duration $\Delta t_{\text{silent}}(i)$ and Bayesian permanent-failure posterior $\hat{p}_{\text{perm}}(i) \in [0, 1]$:

$$\text{Score}_2(i) = \hat{p}_{\text{perm}}(i) \cdot \left( w_{\text{margin}} U_{\text{margin}}(u_{\text{low}}) + w_{\text{risk}} \hat{P}_{\text{loss\_upper}} + w_{\text{dom}} E_{\text{dom}} + w_{\text{age}} \min\!\left(1, \frac{\Delta t_{\text{silent}}(i)}{\tau_{\text{max\_defer}}}\right) \right) - w_{\text{cost}} C_{\text{norm}}$$

where:
- Multiplicative gating by $\hat{p}_{\text{perm}}(i)$ ensures that freshly offline nodes ($\Delta t_{\text{silent}} \ll \mu_{\text{outage}}$, where $\hat{p}_{\text{perm}} \approx \pi_0 \ll 1$) have near-zero repair score unless online availability $|\mathcal{A}(t)|$ is critically close to $k$.
- Conversely, once a shard is confirmed lost ($\hat{p}_{\text{perm}}(i) \to 1$) and silence age approaches $\tau_{\text{max\_defer}}$ (or when `worst_case_burst_tolerance` requires $n$ intact shards and the bandwidth pipe is free), $\text{Score}_2(i)$ crosses $\theta_{\text{trigger}}$, restoring full $n$-shard protection.

---

## 3. Separation of Development/Tuning Seeds vs. Held-Out Evaluation Seeds

To prevent overfitting `FspAdaptiveV2` thresholds to the test or evaluation trajectories:
- **Development / Unit-Test Seeds:** `0x0000_0000_0000_0001` .. `0x0000_0000_00FF_FFFF` (used in `tests/`).
- **Phase 1 Regression Seed:** `0xF591_2026_0000_0001`.
- **Phase 2 Held-Out Evaluation Master Seed:** `0xF592_2026_9999_0001` (never used for parameter tuning).
