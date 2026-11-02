# FSP Phase 1: Mathematical Model, Correlated Failures & Adaptive Repair Priority

## 1. Orthogonal Stochastic Failure Processes & Exogenous Trace Generation

To eliminate common-random-numbers (CRN) coupling and ensure that every comparison strategy is evaluated against **identical exogenous environmental failures**, the Phase 1 failure generator (`src/simulation/failure_generator.rs`) partitions randomness into six independent, domain-separated `SplitMix64` substreams derived from `(master_seed, sample_index, stream_tag)`:

1. **Stream `0x01` — Independent Node Transient Churn (`NodeOutage`):**
   - Per epoch $\Delta E = 86,400\text{ s}$, each node slot $v \in \{0, \dots, N-1\}$ experiences a transient outage with probability $p_{\text{churn}} \in [0, 1)$.
   - Outage duration $D_{\text{outage}}$ is sampled from a configurable `OutageDurationDistribution`:
     - `Exponential { mean_secs }`: $D \sim \text{Exp}(1/\mu)$
     - `LogNormal { mu_ln, sigma_ln }`: $D = \exp(\mu_{\ln} + \sigma_{\ln} Z)$ via Box–Muller transform
     - `Fixed { duration_secs }`
     - `Uniform { min_secs, max_secs }`
   - Upon completion at $t + D_{\text{outage}}$, `NodeRecovery` restores the node to `Online` with all hosted shards intact (unless a permanent failure occurred in the interim).

2. **Stream `0x02` — Independent Node Permanent Failures (`NodePermanentFailure`):**
   - Per epoch $\Delta E$, each node slot $v \in \{0, \dots, N-1\}$ suffers an irreversible storage failure with probability $p_{\text{perm}} \in [0, 1)$.
   - **Slot Re-provisioning Correction:** When node slot $v$ fails permanently at time $t$, its hosted shard replica is permanently destroyed, and slot $v$ is re-provisioned as an empty `Online` replacement node in the same failure domain $\text{dom}(v)$. Crucially, because exogenous events target slot $v$, any shard subsequently repaired onto slot $v$ remains subject to future exogenous outages and permanent failures at the exact same hazard rate.

3. **Stream `0x03` — Failure-Domain-Wide Transient Outages (`DomainOutage`):**
   - Per epoch $\Delta E$, each failure domain $d \in \{0, \dots, M-1\}$ experiences a correlated network/power outage with probability $p_{\text{dom\_outage}} \in [0, 1)$ and duration $D_{\text{dom\_outage}}$, simultaneously taking all nodes in domain $d$ temporarily offline without deleting their shards.

4. **Stream `0x04` — Correlated Permanent Domain Disasters (`DomainPermanentFailure`):**
   - Per epoch $\Delta E$, each failure domain $d \in \{0, \dots, M-1\}$ suffers a correlated permanent failure with probability $p_{\text{dom\_perm}} \in [0, 1)$, simultaneously destroying all shards hosted on all nodes in domain $d$.

5. **Stream `0x05` — Multi-Node Burst Failures (`BurstNodeFailure`):**
   - Per epoch $\Delta E$, a cluster-wide correlated shock occurs with probability $p_{\text{burst}} \in [0, 1)$, affecting $B_{\text{count}} \in [1, N]$ distinct pseudorandomly selected nodes simultaneously (configurable as either permanent node failures or transient outages of duration $D_{\text{burst}}$).

---

## 2. Correlated Analytical Survival Model

When correlated domain failures ($p_{\text{dom\_perm}} > 0$) or burst failures ($p_{\text{burst}} > 0$) are present, the naive binomial formula assuming independent shard failures overestimates survival probability.

In Phase 1 (`src/reliability/survival.rs`), we provide:
1. **Independent Analytical Survival ($S_{\text{indep}}(\Delta t \mid u)$):**
   $$\lambda_{\text{node}} = \frac{-\ln(1 - p_{\text{perm}})}{\Delta E}, \quad p_s(\Delta t) = e^{-\lambda_{\text{node}} \Delta t}$$
   $$S_{\text{indep}}(\Delta t \mid u) = \sum_{j=k}^{u} \binom{u}{j} p_s(\Delta t)^j (1 - p_s(\Delta t))^{u-j}$$
2. **Correlated Domain-Aware Analytical Survival Approximation ($S_{\text{corr}}(\Delta t)$):**
   Let $c_{\max}^* \ge 1$ be the maximum shard concentration per failure domain across $M_{\text{active}}$ populated domains, and let $\lambda_{\text{dom}} = \frac{-\ln(1 - p_{\text{dom\_perm}})}{\Delta E}$. Over window $\Delta t$, each domain survives a correlated permanent disaster with probability $q_{\text{dom}}(\Delta t) = e^{-\lambda_{\text{dom}} \Delta t}$, and each node within a surviving domain survives independent failure with probability $p_s(\Delta t) = e^{-\lambda_{\text{node}} \Delta t}$.
   By conditioning on the number of surviving domains $m \in \{0, \dots, M_{\text{active}}\}$, the remaining capacity is at most $u_m = \max(0, u - (M_{\text{active}} - m) c_{\max}^*)$, yielding the correlated survival upper/structural approximation:
   $$S_{\text{corr}}(\Delta t) = \sum_{m=0}^{M_{\text{active}}} \binom{M_{\text{active}}}{m} q_{\text{dom}}(\Delta t)^m (1 - q_{\text{dom}}(\Delta t))^{M_{\text{active}} - m} \cdot S_{\text{indep}}(\Delta t \mid u_m)$$

---

## 3. FSP Adaptive Repair Priority Function (Heuristic Formulation)

> **Explicit Scientific Disclosure:** The FSP Adaptive Repair policy implemented in Phase 1 (`FspAdaptivePolicy` in `src/strategies/adaptive.rs`) uses a **mathematically normalized multi-factor risk-cost heuristic**, not a closed-form Markov Decision Process (MDP) global optimum.

### 3.1 Decision Problem
At controller evaluation time $t$, given $|\mathcal{A}(t)| \ge k$ online valid shards:
- Let $u = |\mathcal{U}(t)| + N_{\text{inflight}}$ be the effective durable shard count (including shards already scheduled in the repair queue).
- Let $a = |\mathcal{A}(t)|$ be the currently reachable online shard count.
- Let $m_{\text{perm}} = |\mathcal{I}_{\text{missing}}|$ be the count of confirmed permanently lost shards not yet in repair.
- Let $m_{\text{suspect}} = |\mathcal{I}_{\text{suspect}}|$ be the count of shards on nodes that have been `TemporarilyOffline` for $\Delta t_{\text{offline}} \ge \tau_{\text{suspect}}$.

### 3.2 Normalized Risk & Cost Terms (Dimensionless in $[0, 1]$)

1. **Redundancy Depletion Urgency ($U_{\text{margin}} \in [0, 1]$):**
   Measures how much of the $(n - k)$ fault-tolerance buffer has been consumed by permanent losses:
   $$U_{\text{margin}} = \begin{cases} 1.0 & \text{if } u \le k \\ \left(\frac{n - u}{n - k}\right)^{\gamma} & \text{if } k < u \le n \\ 0.0 & \text{if } u > n \end{cases}$$
   where $\gamma = 1.5$ weights steeper urgency as $u \to k$.

2. **Probability of Loss Before Repair Completion ($R_{\text{window}} \in [0, 1]$):**
   Let $\Delta t_{\text{repair\_est}}$ be the expected time to complete repairing the missing shards given the current repair bandwidth queue backlog $Q_{\text{Wait}}(t)$ and per-shard transfer time $\tau_{\text{shard}} = L_{\text{base}} + \frac{(k+1) S_{\text{shard}}}{B_{\max}}$:
   $$\Delta t_{\text{repair\_est}} = Q_{\text{wait}}(t) + \max(1, m_{\text{perm}}) \cdot \tau_{\text{shard}}$$
   Then the probability of suffering irrecoverable data loss during the vulnerability window $\Delta t_{\text{repair\_est}}$ (plus one controller check interval $\Delta t_{\text{check}}$) without repair is:
   $$R_{\text{window}} = 1 - S_{\text{corr}}\!\left(\Delta t_{\text{repair\_est}} + \Delta t_{\text{check}} \;\middle|\; u\right)$$

3. **Failure-Domain Concentration Exposure ($E_{\text{dom}} \in [0, 1]$):**
   $$E_{\text{dom}} = \min\!\left(1.0, \frac{c_{\max}^*(t)}{\max(1, u - k + 1)}\right)$$
   When $E_{\text{dom}} \ge 1.0$, a single correlated failure-domain disaster can immediately cause irrecoverable data loss.

4. **Normalized Repair Cost & Queue Congestion Penalty ($C_{\text{norm}} \in [0, 1]$):**
   Let $B_{\text{budget\_window}} = B_{\max} \cdot \Delta t_{\text{check}}$ be the maximum bytes transferable per controller interval, and let $W_{\text{req}} = (k + 1) S_{\text{shard}}$ be the bytes required to repair one shard:
   $$C_{\text{norm}} = \min\!\left(1.0, \frac{W_{\text{req}} + Q_{\text{bytes}}(t)}{\max(1, B_{\text{budget\_window}})}\right)$$

### 3.3 Composite Repair Priority Function
For each candidate shard repair, the dimensionless priority score $P \in \mathbb{R}$ is defined as:
$$P(\text{loss\_risk}, \text{repair\_cost}, \text{repair\_latency}, \text{domain\_exposure}) = w_{\text{margin}} U_{\text{margin}} + w_{\text{risk}} R_{\text{window}} + w_{\text{dom}} E_{\text{dom}} - w_{\text{cost}} C_{\text{norm}}$$
with default normalized weights $w_{\text{margin}} = 0.45$, $w_{\text{risk}} = 0.35$, $w_{\text{dom}} = 0.20$, $w_{\text{cost}} = 0.15$.

### 3.4 Scheduling & Transient-Outage Gating Rules
1. **Critical Safety Override:** If $u \le k + c_{\max}^*(t)$ (meaning a single domain failure or $\le c_{\max}^*$ node failures would destroy the object), priority is boosted to $P = 1.0$, immediately scheduling all confirmed missing shards $\mathcal{I}_{\text{missing}}$.
2. **Batch Deferred Repair of Confirmed Permanent Losses:** When $u > k + c_{\max}^*(t)$ and $P < \theta_{\text{trigger}}$ (default $\theta_{\text{trigger}} = 0.22$), confirmed permanent losses are **deferred** until additional losses or rising risk push $P \ge \theta_{\text{trigger}}$, amortizing repair operations and avoiding bandwidth contention during transient churn spikes.
3. **Transient Outage Suppression:** Suspected offline shards ($\mathcal{I}_{\text{suspect}}$, whose host nodes are in `TemporarilyOffline` rather than `PermanentlyFailed`) are **never** repaired unless:
   - $\Delta t_{\text{offline}} \ge \tau_{\text{suspect}}$, **AND**
   - Currently available online shards $a = |\mathcal{A}(t)| \le k + 2$ (availability is critically threatened) and $P \ge \theta_{\text{suspect}}$ (default $0.55$).
   This prevents short transient outages ($1\%\text{--}20\%$ churn) from triggering wasteful speculative repairs.
4. **Candidate Ordering:** Eligible shards are sorted deterministically by `(is_confirmed_permanent_loss DESC, priority_score DESC, shard_id ASC)` and capped so that newly scheduled bytes do not exceed the available bandwidth queue cap.

---

## 4. Statistical Estimators & Censoring Mathematics

### 4.1 Wilson Score Confidence Interval for $\hat{P}_{\text{loss}}(T_{\max})$
Given $R$ independent Monte Carlo trajectories with $X$ observed irrecoverable data loss events ($\hat{p} = X / R$) and two-sided confidence level $1 - \alpha = 0.95$ ($z = 1.959963984540054$):
$$\text{CI}_{\text{Wilson}} = \frac{\hat{p} + \frac{z^2}{2R} \pm z \sqrt{\frac{\hat{p}(1 - \hat{p})}{R} + \frac{z^2}{4R^2}}}{1 + \frac{z^2}{R}}$$
Unlike the Wald interval $\hat{p} \pm z\sqrt{\hat{p}(1-\hat{p})/R}$, the Wilson interval is strictly bounded in $[0, 1]$ and provides valid non-zero upper bounds even when $X = 0$ losses are observed (e.g., for $X = 0, R = 1000$, upper bound $\approx 0.003827$; for $X = 0, R = 10000$, upper bound $\approx 0.000384$).

### 4.2 Kaplan–Meier Product-Limit Survival Estimator ($\hat{S}_{\text{KM}}(t)$)
Let $t_1 < t_2 < \dots < t_m \le T_{\max}$ be the distinct ordered failure times across $R$ runs, where $d_j$ is the number of data loss events at $t_j$ and $n_j$ is the number of runs still at risk (durable and uncensored) just prior to $t_j$:
$$\hat{S}_{\text{KM}}(t) = \prod_{j : t_j \le t} \left(1 - \frac{d_j}{n_j}\right)$$
Runs that survive to $T_{\max}$ without data loss are right-censored at $T_{\max}$ ($N_{\text{censored}} = R - \sum d_j$).

### 4.3 Restricted Mean Survival Time ($\text{RMST}(T_{\max})$)
Because unrestricted $\text{MTTDL} = \int_0^\infty S(t)\,dt$ is non-identifiable when trajectories are right-censored at $T_{\max}$ (especially when $X = 0$), we compute the exact non-parametric **Restricted Mean Survival Time** over $[0, T_{\max}]$:
$$\widehat{\text{RMST}}(T_{\max}) = \int_0^{T_{\max}} \hat{S}_{\text{KM}}(t)\,dt = \frac{1}{R} \sum_{r=1}^{R} \min(T_{\text{loss}, r}, T_{\max})$$
along with its empirical standard error and $95\%$ confidence interval.
