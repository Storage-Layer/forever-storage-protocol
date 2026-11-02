# FSP Phase 0: Mathematical Simulation Model & Metrics Specification

## 1. Discrete-Event Simulation (DES) Formalism

The FSP simulator models storage reliability as a deterministic, seeded Discrete-Event Simulation over continuous or discrete simulation time $t \in [0, T_{\max}]$ (measured in seconds, with standard epochs of $\Delta E = 86,400\text{ s} = 1\text{ day}$).

### 1.1 System Entities & State Space

1. **Failure Domains ($\mathcal{D}$):**
   - A finite set of failure domains $\mathcal{D} = \{d_0, d_1, \dots, d_{M-1}\}$.
   - Each domain $d \in \mathcal{D}$ has a correlated hazard profile and a maximum allowed shard concentration $c_{\max}(d) \in \mathbb{N}^+$.

2. **Storage Nodes ($\mathcal{V}$):**
   - A finite set of storage nodes $\mathcal{V} = \{v_0, v_1, \dots, v_{N-1}\}$ (default $N = 50$).
   - Each node $v \in \mathcal{V}$ belongs to exactly one failure domain $\text{dom}(v) \in \mathcal{D}$.
   - At any simulation time $t$, a node has state $s(v, t) \in \{\text{Online}, \text{TemporarilyOffline}, \text{PermanentlyFailed}\}$:
     - $\text{Online}$: Node can serve shard reads, accept shard writes/repairs, and respond to verification challenges.
     - $\text{TemporarilyOffline}$: Node is unreachable due to transient churn or domain outage. Shards stored on $v$ cannot be read at time $t$, **but** remain durably stored on $v$'s persistent media and become accessible again when $v$ transitions back to $\text{Online}$.
     - $\text{PermanentlyFailed}$: Node (or its storage medium) has suffered permanent failure. All shard replicas hosted on $v$ are irreversibly destroyed.

3. **Erasure-Coded Data Shards ($\mathcal{S}$):**
   - A stored object of logical size $S_{\text{obj}}$ bytes is encoded via a maximum distance separable (MDS) erasure code $(n, k)$—by default Reed–Solomon $(20, 12)$—into $n$ distinct logical shards indexed by $i \in \mathcal{I} = \{0, 1, \dots, n-1\}$.
   - Each shard $i \in \mathcal{I}$ has size:
     $$S_{\text{shard}} = \frac{S_{\text{obj}}}{k}$$
   - For an $R$-way replication baseline, $k = 1$, $n = R$, and $S_{\text{shard}} = S_{\text{obj}}$.
   - A **Shard Replica** $r = (i, v, \text{valid})$ represents logical shard index $i \in \mathcal{I}$ stored on node $v \in \mathcal{V}$ with cryptographic validity flag $\text{valid} \in \{\text{true}, \text{false}\}$.
   - **Logical Identity Invariant:** Each logical shard index $i \in \mathcal{I}$ is distinct. Having multiple replicas of the *same* shard index $i$ does not increase the count of distinct shards toward the $k$-of-$n$ recovery threshold. Furthermore, the simulation enforces that at most one active valid replica of shard index $i$ is maintained in the active placement map, and no node $v$ may host more than one shard of the same object unless explicitly allowed by configuration (default: at most 1 shard per node).

---

## 2. Availability vs. Durability (Recoverability) Invariants

At any simulation time $t$, define two subsets of the logical shard index set $\mathcal{I} = \{0, \dots, n-1\}$:

1. **Currently Available Distinct Shards ($\mathcal{A}(t)$):**
   $$\mathcal{A}(t) = \left\{ i \in \mathcal{I} \;\middle|\; \exists \text{ replica } (i, v, \text{true}) \text{ such that } s(v, t) = \text{Online} \right\}$$

2. **Durably Existing Distinct Shards ($\mathcal{U}(t)$):**
   $$\mathcal{U}(t) = \left\{ i \in \mathcal{I} \;\middle|\; \exists \text{ replica } (i, v, \text{true}) \text{ such that } s(v, t) \in \{\text{Online}, \text{TemporarilyOffline}\} \right\}$$

Clearly, $\mathcal{A}(t) \subseteq \mathcal{U}(t) \subseteq \mathcal{I}$ for all $t \ge 0$.

From $|\mathcal{A}(t)|$ and $|\mathcal{U}(t)|$, we define the four canonical system states:

| Condition | Object Status | Operational Meaning |
|---|---|---|
| $\|\mathcal{A}(t)\| \ge k$ | **Available & Recoverable** | Data can be immediately read and reconstructed; repairs can execute. |
| $\|\mathcal{A}(t)\| < k \;\land\; \|\mathcal{U}(t)\| \ge k$ | **Temporarily Unavailable (Durable)** | Data cannot be read at instant $t$ due to transient node outages, but no permanent data loss has occurred. Recovery of offline nodes restores availability. |
| $\|\mathcal{U}(t)\| < k$ | **Irrecoverably Lost (Absorbing)** | Permanent data loss. Even if all temporarily offline nodes immediately recovered, fewer than $k$ distinct valid shards exist. |

---

## 3. Churn & Failure Process Specification

To prevent ambiguity between transient churn and permanent failure, every churn scenario (`1%`, `5%`, `10%`, `20%`) is defined over a standardized epoch $\Delta E = 86,400\text{ s}$ (1 day):

1. **Transient Node Churn Rate ($p_{\text{churn}} \in \{0.01, 0.05, 0.10, 0.20\}$):**
   - Represents the per-node probability per epoch $\Delta E$ of experiencing a temporary outage event (`NodeOutage`), modeled as a Poisson process with rate $\lambda_{\text{outage}} = -\ln(1 - p_{\text{churn}}) / \Delta E$ (or direct per-epoch Bernoulli/exponential arrival).
   - Each outage has a duration $D_{\text{outage}}$ drawn from a seeded distribution (default: Exponential or Log-Normal with configurable mean $\mu_{\text{outage}}$, e.g., $4\text{ hours} = 14,400\text{ s}$), scheduling a `NodeRecovery` event at $t + D_{\text{outage}}$.
   - During $[t, t + D_{\text{outage}})$, all shards on node $v$ remain intact on disk unless a permanent failure event strikes node $v$.

2. **Permanent Node / Shard Failure Rate ($\lambda_{\text{perm}}$):**
   - Represents the per-node hazard rate of irreversible storage failure (`NodePermanentFailure` or `ShardCorruption`), parameterized independently via `permanent_failure_rate_per_epoch`.
   - When `NodePermanentFailure` occurs on node $v$, $s(v, t) \leftarrow \text{PermanentlyFailed}$ and all shards on $v$ are permanently removed from $\mathcal{U}(t)$ and $\mathcal{A}(t)$. Replacement healthy capacity may join the network as new empty nodes (`NodeReplacement`), preserving cluster size without restoring destroyed shards.

3. **Correlated Failure-Domain Events:**
   - `DomainOutage(d, duration)`: Transitions all $v \in \mathcal{V}$ with $\text{dom}(v) = d$ and $s(v, t) = \text{Online}$ into $\text{TemporarilyOffline}$ for `duration`.
   - `DomainPermanentFailure(d)`: Simultaneously transitions all nodes in domain $d$ into $\text{PermanentlyFailed}$, destroying all shards placed in domain $d$.

---

## 4. Repair Mechanics, Bandwidth & Latency

When a repair strategy schedules the repair of $m$ missing/lost shard indices $I_{\text{repair}} \subseteq \mathcal{I} \setminus \mathcal{U}(t)$ (or unreachable shards exceeding the strategy's timeout):

1. **Reconstruction Precondition:**
   - Repair can only initiate and complete if $|\mathcal{A}(t)| \ge k$ (at least $k$ distinct valid shards are currently `Online`) and the object has not entered the absorbing `IrrecoverablyLost` state.

2. **Bandwidth Accounting:**
   - Reconstructing $m$ shards under Reed–Solomon $(n, k)$ requires reading $k$ online shards ($k \cdot S_{\text{shard}}$ bytes read) and writing $m$ repaired shards ($m \cdot S_{\text{shard}}$ bytes written).
   - For replication ($k = 1$), repairing $m$ replicas reads $1 \cdot S_{\text{shard}}$ bytes and writes $m \cdot S_{\text{shard}}$ bytes.
   - Total repair network transfer for a batch of $m$ shards initiated together:
     $$W_{\text{batch}}(m) = (k + m) \cdot S_{\text{shard}} \quad \text{(or } (k + 1) \cdot S_{\text{shard}} \text{ per individual shard repair)}$$

3. **Bandwidth Throttling & Repair Latency:**
   - Given a maximum cluster repair bandwidth budget $B_{\max}$ (bytes/sec) and base coordination latency $L_{\text{base}}$ (sec), the transfer duration for a repair job transferring $W$ bytes is:
     $$\Delta t_{\text{transfer}} = L_{\text{base}} + \frac{W}{B_{\max}}$$
   - Concurrent repair requests queue behind the active repair bandwidth channel (or share bandwidth up to $B_{\max}$), ensuring that repair bursts cannot complete faster than physical bandwidth allows.
   - A `RepairComplete` event is scheduled at $t_{\text{finish}} = \max(t_{\text{now}}, t_{\text{channel\_free}}) + \Delta t_{\text{transfer}}$.
   - **Repair Latency ($L_{\text{repair}}$):** Defined for each repaired shard $i$ as $t_{\text{finish}} - t_{\text{lost}}(i)$, which includes detection delay, queueing delay due to bandwidth limits, and transfer time.

---

## 5. Mathematical Definitions of Observability Metrics

We strictly distinguish **Monte Carlo Empirical Estimates** (observed over $R_{\text{runs}}$ simulation trajectories of horizon $T_{\max}$) from **Analytical Predictions** (closed-form or Markov approximations).

### 5.1 Data Loss Probability — Empirical ($\hat{P}_{\text{loss}}(T_{\max})$)
Across $R_{\text{runs}}$ independent seeded simulation runs over horizon $[0, T_{\max}]$:
$$\hat{P}_{\text{loss}}(T_{\max}) = \frac{1}{R_{\text{runs}}} \sum_{r=1}^{R_{\text{runs}}} \mathbb{I}\!\left(\min_{t \in [0, T_{\max}]} |\mathcal{U}_r(t)| < k\right)$$
For a single trajectory ($R_{\text{runs}} = 1$), $\hat{P}_{\text{loss}}(T_{\max}) \in \{0.0, 1.0\}$.

### 5.2 Time-Average Data Availability ($\hat{A}(T_{\max})$)
For a single trajectory over horizon $T_{\max} > 0$, let $\mathcal{T}_{\text{avail}} = \{t \in [0, T_{\max}] : |\mathcal{A}(t)| \ge k\}$. Using the exact piecewise-constant event intervals $[t_j, t_{j+1})$:
$$\hat{A}(T_{\max}) = \frac{1}{T_{\max}} \sum_{j} (t_{j+1} - t_j) \cdot \mathbb{I}\!\left(|\mathcal{A}(t_j)| \ge k\right)$$

### 5.3 Empirical Survival Curve ($\hat{S}(t)$)
Across $R_{\text{runs}}$ trajectories, the Kaplan–Meier / empirical survival probability at time $t \in [0, T_{\max}]$ is:
$$\hat{S}(t) = \frac{1}{R_{\text{runs}}} \sum_{r=1}^{R_{\text{runs}}} \mathbb{I}\!\left(T_{\text{loss}, r} > t\right)$$
where $T_{\text{loss}, r} = \inf\{t \ge 0 : |\mathcal{U}_r(t)| < k\}$ (with $T_{\text{loss}, r} = +\infty$ if no loss occurs in $[0, T_{\max}]$).

### 5.4 Analytical Instantaneous & Horizon Survival Probability ($S_{\text{analytical}}(\Delta t)$)
Given independent per-node permanent failure hazard rate $\lambda_{\text{perm}}$ and $u = |\mathcal{U}(t)| \ge k$ currently durable shards without repair over window $\Delta t$, each shard survives window $\Delta t$ with probability $p_s(\Delta t) = e^{-\lambda_{\text{perm}} \Delta t}$. The binomial analytical survival probability without repair over $\Delta t$ is:
$$S_{\text{analytical}}(\Delta t \mid u) = \sum_{j=k}^{u} \binom{u}{j} p_s(\Delta t)^j \left(1 - p_s(\Delta t)\right)^{u - j}$$
*(Note: This is explicitly labeled as an analytical no-repair lower-bound / step estimator in `src/reliability/survival.rs`, distinct from the empirical Monte Carlo survival curve).*

### 5.5 Mean Time To Data Loss (MTTDL)
- **Empirical MTTDL Guardrail:** When $R_{\text{runs}}$ trajectories are executed over finite horizon $T_{\max}$ and fewer than a statistically meaningful number of data loss events occur (specifically, when $N_{\text{losses}} = 0$), **empirical MTTDL is mathematically undefined / right-censored** and MUST return `None` (`RightCensored`) rather than a fabricated number or $\infty$. When $N_{\text{losses}} > 0$ with all runs uncensored (or via total-time-on-test estimator $\frac{\sum_{r=1}^{R_{\text{runs}}} \min(T_{\text{loss}, r}, T_{\max})}{N_{\text{losses}}}$), it is reported with an explicit censoring status and confidence warning if $N_{\text{losses}}$ is small.
- **Analytical Markov MTTDL Approximation:** Classic birth-death CTMC approximation for $(n, k)$ code with per-shard failure rate $\lambda$ and repair rate $\mu$ is provided separately in `src/reliability/survival.rs` and clearly marked as an analytical independent-failure approximation that overestimates reliability under correlated domain failures.

### 5.6 Repair Bandwidth Consumption & Operation Count
- **Total Repair Bytes ($B_{\text{total}}$):** Sum of all bytes read and written by initiated/completed repair operations in $[0, T_{\max}]$.
- **Average Repair Bandwidth ($\bar{B}_{\text{repair}}$):** $B_{\text{total}} / T_{\max}$ (bytes/sec).
- **Repair Operation Count ($C_{\text{repair}}$):** Total number of shard repair operations completed (and failed/aborted tracked separately).

### 5.7 Storage Overhead ($\Omega$)
$$\Omega(t) = \frac{|\mathcal{U}(t)| \cdot S_{\text{shard}}}{S_{\text{obj}}} = \frac{|\mathcal{U}(t)|}{k}$$
At full health ($|\mathcal{U}(t)| = n$), nominal storage overhead is $\Omega_{\text{nominal}} = \frac{n}{k}$ (for RS(20,12), $\frac{20}{12} \approx 1.6667\times$; for 3-way replication, $\frac{3}{1} = 3.0\times$).

### 5.8 Failure-Domain Exposure Metric ($E_{\text{domain}}(t)$)
Let $c(d, t)$ be the number of distinct durable shards in $\mathcal{U}(t)$ hosted on nodes in failure domain $d \in \mathcal{D}$.
- **Maximum Domain Concentration:** $c_{\max}^*(t) = \max_{d \in \mathcal{D}} c(d, t)$.
- **Single-Domain Vulnerability Margin:** $M_{\text{domain}}(t) = (|\mathcal{U}(t)| - k) - c_{\max}^*(t)$.
- **Normalized Domain Exposure Ratio:**
  $$E_{\text{domain}}(t) = \begin{cases} 0.0 & \text{if } |\mathcal{U}(t)| = 0 \\ \min\!\left(1.0, \frac{c_{\max}^*(t)}{\max(1, |\mathcal{U}(t)| - k + 1)}\right) & \text{if } |\mathcal{U}(t)| \ge k \\ 1.0 & \text{if } |\mathcal{U}(t)| < k \end{cases}$$
  Notice that $E_{\text{domain}}(t) \ge 1.0$ iff the permanent loss of the single most-populated failure domain would immediately reduce durable shards below $k$ (causing irrecoverable data loss in a single correlated domain disaster).
