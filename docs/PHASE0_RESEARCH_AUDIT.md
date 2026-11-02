# FSP Phase 0: Research Audit & Protocol Baseline

## 1. Repository Audit Findings

### 1.1 Physical Repository Inspection
Prior to Phase 0 implementation, an exhaustive audit of the repository (`/home/rade/Project/Jobs/fsp`) was conducted:
- **Existing Files:** None. The workspace directory was completely empty (`0` tracked files, no `.git` repository, no `docs/`, no `src/`, and no `Cargo.toml`).
- **Missing Research Artifacts:** The FSP v0.1 research design documents, formal mathematical specifications, architecture descriptions, and prior source code are **absent from the repository**. In strict compliance with Phase 0 rules, we explicitly report that no external or pre-existing FSP v0.1 specification files were present to be reviewed.
- **Source of Truth:** Consequently, the sole authoritative specification available in-tree is the Phase 0 problem statement and architectural brief. All design constraints, initial parameters, and structural boundaries below are extracted directly from that specification, while mathematical gaps are explicitly surfaced and formalized before implementation.

---

## 2. Extracted Protocol Requirements

From the Phase 0 specification, the Forever Storage Protocol (FSP) simulation foundation must satisfy the following requirements:

### 2.1 System & Erasure Coding Parameters
- **Default Storage Nodes ($N_{\text{nodes}}$):** `50` nodes distributed across configurable failure domains.
- **Default Erasure Coding Scheme:** Reed–Solomon $(n = 20, k = 12)$.
  - Total distinct shards per object: $n = 20$.
  - Minimum distinct valid shards required for reconstruction: $k = 12$.
  - Maximum tolerated simultaneous shard unavailability/loss before data unavailability/loss: $n - k = 8$.
- **Churn Scenarios:** `1%`, `5%`, `10%`, `20%` (requiring formal time-basis and event-type disambiguation).

### 2.2 Failure & State Semantics
The simulation must strictly distinguish four orthogonal failure and availability states:
1. **Temporary Unavailability (Node Outage):** A storage node becomes unreachable for a bounded duration $[t_{\text{down}}, t_{\text{up}})$. Shards stored exclusively on this node are temporarily unavailable for reads, repairs, or verifications during the outage, but their durable state remains intact upon node recovery unless a separate permanent failure occurs.
2. **Permanent Shard Loss:** A shard replica on a node is irreversibly destroyed (due to disk failure, silent corruption detected via cryptographic verification, or permanent node departure/decommissioning). Recovery of that shard index requires reconstructing it from at least $k$ other distinct available shards and writing it to a healthy node.
3. **Data Unavailability (Transient):** At simulation time $t$, fewer than $k$ distinct valid shards are *currently reachable* (online and uncorrupted), **but** at least $k$ distinct valid shards still exist durably across the union of online and temporarily offline nodes. Once sufficient offline nodes recover, the object becomes available again without data loss.
4. **Irrecoverable Data Loss (Permanent Absorbing State):** Fewer than $k$ distinct valid shards exist durably across **all** non-permanently-failed nodes (both online and temporarily offline). Even if all temporarily offline nodes immediately recovered, the object could never be reconstructed. Once entered, this state is absorbing; no valid repair can ever occur afterward.

### 2.3 Strategy Interfaces
Three storage preservation strategies must be supported under identical failure traces and seeded pseudo-random streams:
- **Strategy A (`Replication`):** Conventional $R$-way replication (where $k_{\text{rep}} = 1$, $n_{\text{rep}} = R$) or equivalent storage-overhead baseline, repairing lost replicas from any single available replica.
- **Strategy B (`FixedRepair`):** Reed–Solomon $(n, k)$ erasure-coded storage triggering repair whenever the count of known durable (or available) distinct shards drops at or below a fixed threshold $T_{\text{repair}} \in [k, n-1]$, or on a periodic check interval $\Delta t_{\text{check}}$.
- **Strategy C (`AdaptiveRepair` — Interface Only in Phase 0):** Risk-aware repair scheduling interface accepting current shard availability, failure-domain exposure, repair bandwidth cost, and estimated survival probability over horizon $H$. In Phase 0, we define the trait and telemetry inputs without inventing an unvalidated optimization heuristic.

---

## 3. Identified Ambiguities, Underspecified Behavior & Mathematical Resolutions

Because the FSP v0.1 research paper is missing from the repository, several critical behaviors were underspecified in the high-level brief. Below we document each ambiguity and the explicit mathematical convention adopted for Phase 0.

### 3.1 Ambiguity 1: Definition of "Churn Percentage" (1%, 5%, 10%, 20%)
- **Problem:** In distributed storage literature, "10% churn" is frequently conflated between:
  1. The steady-state fraction of nodes temporarily offline at any instant,
  2. The per-epoch probability that a node experiences a transient outage, or
  3. The annual/epoch rate of permanent node turnover (departure + replacement).
- **Phase 0 Resolution:** We explicitly parameterize churn using a **per-epoch (default: 24-hour day) per-node event rate** decomposed into two independent parameters in `SimulationConfig`:
  - `transient_churn_rate_per_epoch` ($p_{\text{outage}}$): Probability per epoch that an online node enters a temporary outage of mean duration $D_{\text{outage}}$. The scenarios `1%`, `5%`, `10%`, `20%` map directly to $p_{\text{outage}} \in \{0.01, 0.05, 0.10, 0.20\}$ per epoch (or equivalent continuous Poisson hazard rate $\lambda_{\text{outage}}$), with explicit configuration of whether a fraction of events represent permanent node replacement (`permanent_failure_ratio`).
  - `permanent_failure_rate_per_epoch` ($\lambda_{\text{perm}}$): Independent hazard rate of permanent node/disk loss.
  - This prevents conflating temporary unavailability with permanent shard destruction while allowing controlled sensitivity sweeps across both dimensions.

### 3.2 Ambiguity 2: Repair Triggering During Temporary Outages (Lazy vs. Eager Repair)
- **Problem:** When a node goes offline temporarily, a naive repair controller cannot immediately distinguish a transient network/power outage from a permanent disk crash. Eagerly repairing every transient outage wastes massive repair bandwidth; waiting forever risks permanent data loss if permanent failures accumulate.
- **Phase 0 Resolution:**
  - The simulation tracks both the **ground-truth node state** (`Online`, `TemporarilyOffline`, `PermanentlyFailed`) and the **controller-observed shard status** (including how long a shard's host node has been unreachable, $\Delta t_{\text{unreachable}}$).
  - Strategies can trigger repair either on confirmed permanent shard loss or when a shard remains unreachable beyond a configurable suspect timeout $\tau_{\text{suspect}}$, or when available shards drop to a critical threshold.
  - Crucially, if a repair is scheduled for shard index $i$ while another copy of shard $i$ is on a temporarily offline node, the simulation enforces **shard identity deduplication**: at most one valid active assignment per shard index $i \in \{0, \dots, n-1\}$ is counted toward the $k$-of-$n$ distinct shard threshold, and repair operations never fabricate duplicate logical shard IDs on the same node or inflate distinct shard counts above $n$.

### 3.3 Ambiguity 3: Failure-Domain Placement & Correlated Failures
- **Problem:** Placing $n = 20$ shards across $M$ failure domains (e.g., racks, datacenters, geographic regions, or autonomous systems) requires well-defined placement constraints when $M < n$ vs. $M \ge n$.
- **Phase 0 Resolution:**
  - Let $M$ be the number of failure domains. To minimize correlated failure exposure, a valid placement assigns at most $c_{\max}$ shards of the same object to any single failure domain, where by default $c_{\max} = \lceil n / M \rceil$ (or a stricter user-configured cap).
  - Importantly, if a single failure domain can hold at most $c_{\max}$ shards, then a single correlated domain outage/destruction can knock out at most $c_{\max}$ shards. So long as $c_{\max} \le n - k$, no single failure-domain disaster can immediately cause data unavailability or loss.
  - Correlated failure events (`DomainOutage` and `DomainPermanentFailure`) simultaneously transition all nodes assigned to that `FailureDomainId`.

### 3.4 Ambiguity 4: Repair Bandwidth, Concurrency & Latency Modeling
- **Problem:** Instantaneous repair artificially drives data loss probability toward zero in discrete-event simulations, masking the vulnerability window during which additional failures cause data loss.
- **Phase 0 Resolution:**
  - Every repair of a missing shard of size $S_{\text{shard}}$ (bytes) requires reading $k$ distinct currently-online valid shards (reconstruction read traffic $= k \cdot S_{\text{shard}}$ for erasure coding, or $1 \cdot S_{\text{shard}}$ for replication) and writing $1 \cdot S_{\text{shard}}$ to a target online node respecting failure-domain constraints.
  - Repair completion is scheduled at $t_{\text{complete}} = t_{\text{start}} + \Delta t_{\text{repair}} > t_{\text{start}}$, where $\Delta t_{\text{repair}}$ is governed by the available repair bandwidth cap $B_{\max}$ (bytes/sec) and base network/computation latency $L_{\text{base}}$.
  - If at $t_{\text{complete}}$ fewer than $k$ distinct valid shards are online (or the target node went offline/failed during transfer), the in-flight repair fails and must be retried.

### 3.5 Ambiguity 5: Fair Storage Overhead Comparison Across Strategies
- **Problem:** Comparing 3-way replication ($3\times$ overhead) directly against RS(20, 12) ($20/12 \approx 1.67\times$ overhead) without accounting for storage overhead conflates redundancy budget with coding efficiency.
- **Phase 0 Resolution:**
  - Storage overhead is explicitly measured as $\Omega = \frac{\sum \text{stored shard bytes}}{\text{logical object bytes}}$.
  - For RS$(n, k)$ where each shard has size $S_{\text{object}} / k$, nominal overhead is $n / k$.
  - For $R$-way replication where each replica has size $S_{\text{object}}$ (represented logically as $k=1, n=R$ shards of size $S_{\text{object}}$), nominal overhead is $R$.
  - Both strategies expose identical simulation telemetry so experiments can compare equal-overhead configurations (e.g., $R=2$ vs. RS(20, 10)) or standard configurations ($R=3$ vs. RS(20, 12)).

---

## 4. Unproven Claims Requiring Phase 1+ Experimental Validation

In accordance with scientific rigor, Phase 0 makes **no claim** that FSP adaptive repair outperforms fixed-threshold repair or replication. The following hypotheses require controlled Monte Carlo validation in subsequent phases:
1. **Hypothesis H1 (Correlated Survival Advantage):** Failure-domain-aware placement combined with domain-risk-weighted repair reduces empirical data loss probability under correlated domain failures compared to domain-oblivious fixed repair at equal repair bandwidth.
2. **Hypothesis H2 (Bandwidth Suppression Under Transient Churn):** Distinguishing transient churn from permanent loss via survival-probability gating reduces total repair bandwidth consumption by $>50\%$ at $10\%\text{--}20\%$ transient churn without degrading durability.
3. **Hypothesis H3 (MTTDL Estimator Convergence):** Analytical Markov / Weibull survival estimators accurately predict empirical Monte Carlo time-to-data-loss under non-stationary correlated failure traces.
