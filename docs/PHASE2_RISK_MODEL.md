# FSP Phase 2: Probabilistic Risk Estimation Engine (`RiskEstimate`)

## 1. Mathematical Purpose & Scope

The Phase 2 Risk Estimation Engine (`src/reliability/risk.rs`) computes a deterministic, observable-only estimate of the probability that a stored object will lose recoverability (fewer than $k$ surviving distinct shards) within a configurable lookahead horizon $\Delta t_{\text{eval}}$ (default: expected repair completion duration plus one controller interval).

Crucially, the risk estimator **never** accesses hidden simulator ground truth (`PhysicalNodeState`). Instead, it takes as input `ObservableObjectState` derived strictly from `ObservedNodeState` and historical observed failure signals.

---

## 2. Formal Definition of `RiskEstimate` Fields

```rust
pub struct RiskEstimate {
    pub loss_probability: f64,
    pub uncertainty_interval: (f64, f64),
    pub confidence: f64,
    pub calibration_status: ConfidenceCalibrationStatus,
    pub expected_repair_duration_secs: f64,
    pub domain_concentration_score: f64,
    pub effective_durable_shards_expected: f64,
}
```

### 2.1 `effective_durable_shards_expected` ($\hat{u}_{\text{eff}} \in [0, n]$)
Under blind failure detection, a shard hosted on a node in `SuspectedUnavailable` or `ConfirmedUnavailableByPolicy` has an observable Bayesian posterior probability $\hat{p}_{\text{perm}}(i) \in [0, 1]$ of being permanently destroyed rather than temporarily offline:
- For a `Healthy` or `RecoveryObserved` shard $i$: $\hat{p}_{\text{perm}}(i) = 0$, so survival weight is $1.0$.
- For an unreachable shard $i$ silent for $\Delta t_{\text{silent}}(i)$: survival weight is $1 - \hat{p}_{\text{perm}}(i)$.
- Therefore, the expected number of currently intact durable shards is:
  $$\hat{u}_{\text{eff}} = \sum_{i \in \mathcal{I}_{\text{placed}}} \left(1 - \hat{p}_{\text{perm}}(i)\right) + N_{\text{inflight}}$$
- We also define the **conservative lower-bound durable shard count** $u_{\text{low}}$ (treating all `ConfirmedUnavailableByPolicy` and high-posterior suspected shards as permanently lost) and the **optimistic upper-bound durable shard count** $u_{\text{high}}$ (treating all unconfirmed suspected shards as temporarily offline).

### 2.2 `expected_repair_duration_secs` ($\Delta t_{\text{repair\_est}} \ge 0$)
Given $m_{\text{need}}$ shards requiring repair, per-shard repair transfer bytes $W_{\text{shard}}$ (which equals $(k+1) S_{\text{shard}}$ for Reed–Solomon or $(l+1) S_{\text{shard}}$ for local LRC repair), current repair bandwidth queue backlog $Q_{\text{bytes}}$, and effective cluster/node repair bandwidth $B_{\text{eff}}$:
$$\Delta t_{\text{repair\_est}} = \frac{Q_{\text{bytes}}}{B_{\text{eff}}} + m_{\text{need}} \cdot \left(L_{\text{base}} + \frac{W_{\text{shard}}}{B_{\text{eff}}}\right)$$

### 2.3 `loss_probability` ($\hat{P}_{\text{loss}}(\Delta t_{\text{eval}}) \in [0, 1]$) & `uncertainty_interval`
Over the lookahead window $\Delta t_{\text{eval}} = \Delta t_{\text{repair\_est}} + \Delta t_{\text{check}}$:
- Using the domain-correlated analytical survival function $S_{\text{corr}}(\Delta t_{\text{eval}} \mid u, \hat{\lambda}_{\text{node}}, \hat{\lambda}_{\text{dom}})$:
  - **Lower Risk Bound ($P_{\min}$):** Evaluated at $u = u_{\text{high}}$ and the lower 95% Poisson rate bound $\hat{\lambda}_{\text{low}}$.
  - **Upper Risk Bound ($P_{\max}$):** Evaluated at $u = u_{\text{low}}$ and the upper 95% Poisson rate bound $\hat{\lambda}_{\text{high}}$ (escalated if a `DomainCorrelationSignal` is active).
  - **Point Estimate (`loss_probability`):** Probability-weighted mixture across $\lfloor \hat{u}_{\text{eff}} \rfloor$ and $\lceil \hat{u}_{\text{eff}} \rceil$, bounded within $[P_{\min}, P_{\max}] \subseteq [0, 1]$.

### 2.4 `confidence` & `calibration_status`
In strict compliance with Phase 2 requirements (*"Confidence must not be an arbitrary number. If uncertainty cannot be statistically calibrated, report a clearly defined uncertainty range or mark confidence as unavailable"*):
- `ConfidenceCalibrationStatus` is an explicit enum:
  - `AnalyticalUnderConfiguredHazards { interval_width }`: Hazards are supplied from configured/known model parameters; `uncertainty_interval` reflects state-observability uncertainty $[u_{\text{low}}, u_{\text{high}}]$, and `confidence = 1.0 - (P_max - P_min)` is the exact interval tightness in $[0, 1]$.
  - `EmpiricalPoissonCalibrated { observed_events, observation_node_epochs }`: Hazard rates $\hat{\lambda}$ are estimated online from `observed_events >= 10` historical failures with a 95% Poisson confidence interval.
  - `UncalibratedInsufficientHistory { observed_events }`: Fewer than 10 failure events have been observed online; statistical calibration is marked **unavailable** (`confidence = f64::NAN` or `0.0` with `UncalibratedInsufficientHistory` status), and policies rely on `uncertainty_interval.1` ($P_{\max}$ worst-case bound).

### 2.5 `domain_concentration_score` ($E_{\text{dom}} \in [0, 1]$)
Let $c_{\max}^*$ be the maximum number of believed-durable shards in any single failure domain:
$$E_{\text{dom}} = \begin{cases} 1.0 & \text{if } u_{\text{low}} < k \\ \min\!\left(1.0, \frac{c_{\max}^*}{\max(1, u_{\text{low}} - k + 1)}\right) & \text{if } u_{\text{low}} \ge k \end{cases}$$

---

## 3. Approximations & Limitations of the Risk Estimator

1. **Conditional Independence Within Domains:** Within a surviving failure domain, individual node permanent failures over the short window $\Delta t_{\text{eval}}$ are modeled as conditionally independent given the domain survival indicator.
2. **Bayesian Outage Prior Sensitivity:** The posterior $\hat{p}_{\text{perm}}(\Delta t_{\text{silent}})$ assumes exponential transient outage durations; under heavy-tailed Pareto outages, a fixed exponential tail may overestimate $\hat{p}_{\text{perm}}$ for very long transient outages unless the log-normal/heavy-tail survival function is selected.
