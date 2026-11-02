# FSP Phase 1: Empirical Monte Carlo Experiment Results & Scientific Analysis

## 1. Execution Provenance & Reproducibility Metadata

- **Exact Command Executed:**
  ```bash
  ./target/release/fsp-sim --phase1
  ```
- **Master PRNG Seed:** `0xF591_2026_0000_0001` (`17694957633540325377`)
  - Per-sample seed derivation: `derive_sample_seed(master_seed, sample_index)` across `sample_index in 0..N`, with six domain-separated `SplitMix64` substreams ensuring **identical exogenous failure traces** across all strategies compared at the same `(churn_scenario, sample_index)`.
- **Total Monte Carlo Batches Executed:** `52`
- **Total Independent Monte Carlo Trajectories Completed:** `232,000` (`0` failed or incomplete samples)
  - Suite 1 (`suite1_baseline_1k`): `20` batches $\times$ `1,000` samples = `20,000` trajectories
  - Suite 2 (`suite2_matched_overhead_2x_1k`): `6` batches $\times$ `1,000` samples = `6,000` trajectories
  - Suite 3 (`suite3_high_hazard_*_1k`): `6` batches $\times$ `1,000` samples = `6,000` trajectories
  - Suite 4 (`suite4_expanded_10k`): `20` batches $\times$ `10,000` samples = `200,000` trajectories
- **Hardware & Runtime Telemetry:**
  - Parallel Worker Threads: `24` (verified bit-identical to `1` and `8` threads in `test_02_worker_count_independent_output`)
  - Total Wall-Clock Duration: `225.11 seconds` (~`1,030.6` full 30-day discrete-event trajectories/sec)
  - Peak Resident Memory (`VmHWM`): `10,636 KB` (~`10.4 MB`)
- **Machine-Readable Artifacts:**
  - `results/phase1/experiment_config.json`
  - `results/phase1/raw_results.jsonl`
  - `results/phase1/summary.json`

---

## 2. Suite 4 — Expanded 10,000-Sample Baseline Comparison (`RS(20,12)` vs. `Replication(2x, 3x)`)

### 2.1 Experimental Configuration
- **Cluster:** $N = 50$ nodes across $M = 10$ failure domains; horizon $T_{\max} = 30\text{ epochs} = 2,592,000\text{ s}$.
- **Failure Process:** Mixed independent + correlated failures:
  - Per-node permanent failure rate: $p_{\text{perm}} = 1.5\% / \text{epoch}$
  - Correlated domain permanent disaster rate: $p_{\text{dom\_perm}} = 0.2\% / \text{epoch}$
  - Correlated domain transient outage rate: $p_{\text{dom\_out}} = 1.0\% / \text{epoch}$ (mean duration $7,200\text{ s}$)
  - Node transient churn sweep: $p_{\text{churn}} \in \{1\%, 5\%, 10\%, 20\%\} / \text{epoch}$ (exponential mean $14,400\text{ s}$), suspect timeout $\tau_{\text{suspect}} = 7,200\text{ s}$.
- **Explicit Architectural & Overhead Disclosure:**
  - `StrategyA_Replication_2x`: Nominal storage overhead $\Omega = 2.00\times$ ($n=2, k=1$, max 1 replica/domain).
  - `StrategyA_Replication_3x`: Nominal storage overhead $\Omega = 3.00\times$ ($n=3, k=1$, max 1 replica/domain).
  - `StrategyB` & `StrategyC` with `RS(20,12)`: Nominal storage overhead $\Omega = 1.6667\times$ ($n=20, k=12$, max 2 shards/domain across 10 domains).

### 2.2 Empirical Measurements ($R = 10,000$ independent runs per row)

| Churn | Strategy | Overhead ($\Omega$) | Losses / 10k | $\hat{P}_{\text{loss}}(T_{\max})$ | Wilson 95% CI on $P_{\text{loss}}$ | $\hat{S}_{\text{KM}}(T_{\max})$ | $\widehat{\text{RMST}}(T_{\max})$ (s) | Time-Weighted Availability | Mean Repair BW (B/s) | Mean Repair Ops |
|---|---|---|---|---|---|---|---|---|---|---|
| **1%** | `Replication_2x` | $2.00\times$ | `0` | `0.0000%` | `[0.0000%, 0.0384%]` | `1.00000` | `2,592,000` | `0.99998` | `93.8` | `1.01` |
| **1%** | `Replication_3x` | $3.00\times$ | `0` | `0.0000%` | `[0.0000%, 0.0384%]` | `1.00000` | `2,592,000` | `1.00000` | `141.4` | `1.53` |
| **1%** | `RS(20,12) Fixed_T18` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99993` | `416.0` | `8.29` |
| **1%** | `RS(20,12) FixedSuspect_T18` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99993` | `489.0` | `9.75` |
| **1%** | `RS(20,12) FspAdaptive` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99993` | **`359.7`** | **`7.17`** |
| **5%** | `Replication_2x` | $2.00\times$ | `0` | `0.0000%` | `[0.0000%, 0.0384%]` | `1.00000` | `2,592,000` | `0.99987` | `93.8` | `1.01` |
| **5%** | `Replication_3x` | $3.00\times$ | `0` | `0.0000%` | `[0.0000%, 0.0384%]` | `1.00000` | `2,592,000` | `1.00000` | `141.4` | `1.53` |
| **5%** | `RS(20,12) Fixed_T18` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99992` | `416.0` | `8.29` |
| **5%** | `RS(20,12) FixedSuspect_T18` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99992` | `725.8` | `14.47` |
| **5%** | `RS(20,12) FspAdaptive` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99992` | **`359.7`** | **`7.17`** |
| **10%** | `Replication_2x` | $2.00\times$ | `0` | `0.0000%` | `[0.0000%, 0.0384%]` | `1.00000` | `2,592,000` | `0.99961` | `93.8` | `1.01` |
| **10%** | `Replication_3x` | $3.00\times$ | `0` | `0.0000%` | `[0.0000%, 0.0384%]` | `1.00000` | `2,592,000` | `0.99999` | `141.4` | `1.53` |
| **10%** | `RS(20,12) Fixed_T18` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99991` | `416.0` | `8.29` |
| **10%** | `RS(20,12) FixedSuspect_T18` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99991` | `1,024.8` | `20.43` |
| **10%** | `RS(20,12) FspAdaptive` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99991` | **`359.7`** | **`7.17`** |
| **20%** | `Replication_2x` | $2.00\times$ | `1` | `0.0100%` | `[0.0018%, 0.0566%]` | `0.99990` | `2,591,880` | `0.99866` | `93.8` | `1.01` |
| **20%** | `Replication_3x` | $3.00\times$ | `0` | `0.0000%` | `[0.0000%, 0.0384%]` | `1.00000` | `2,592,000` | `0.99995` | `141.5` | `1.53` |
| **20%** | `RS(20,12) Fixed_T18` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99984` | `416.1` | `8.30` |
| **20%** | `RS(20,12) FixedSuspect_T18` | $1.67\times$ | `5` | `0.0500%` | `[0.0214%, 0.1170%]` | `0.99950` | `2,591,729` | `0.99984` | `1,648.5` | `32.87` |
| **20%** | `RS(20,12) FspAdaptive` | $1.67\times$ | `4` | `0.0400%` | `[0.0156%, 0.1028%]` | `0.99960` | `2,591,829` | `0.99984` | **`359.9`** | **`7.18`** |

### 2.3 What the Suite 4 Measurements Demonstrate
1. **Why `RS(20,12)` across 10 domains had 4 losses/10k (`0.04%`) while `Replication_3x` had 0:**
   - With $M = 10$ failure domains and `max_shards_per_domain = 2`, all $10$ domains hold $2$ shards of the `RS(20,12)` object ($10 \times 2 = 20$).
   - When a correlated permanent domain disaster (`DomainPermanentFailure`) destroys a failure domain, those domain capacity slots are **permanently lost** (`fail_node_permanently(..., provision_empty_replacement: false)`).
   - Once $2$ domains are permanently destroyed, the remaining $8$ domains can hold at most $8 \times 2 = 16$ shards; once $5$ domains are destroyed across 30 epochs, only $5 \times 2 = 10 < k=12$ shards can survive!
   - By contrast, `Replication_3x` only occupies $3$ of the $10$ domains at any time; when one of its domains is destroyed, it still has $7$ other intact failure domains to re-replicate into, plus it pays a **$3.00\times$ storage overhead** ($80\%$ higher storage cost than `RS(20,12)`'s $1.67\times$).
2. **Availability Advantage of `RS(20,12)` over `Replication_2x` at High Churn (`20%`):**
   - At `20%` transient churn, `Replication_2x` availability drops to `0.99866` (~`57.9 minutes` of object unavailability per 30 days) and suffers `1` permanent loss, whereas `RS(20,12)` maintains `0.99984` availability (~`6.9 minutes` of unavailability — **$8.4\times$ less downtime than 2x replication** while using **$16.7\%$ less storage**).
3. **Bandwidth & Durability Comparison Between `RS(20,12)` Strategies:**
   - Naive timeout-based repair (`StrategyB_FixedRepairSuspect_T18`) explodes in repair bandwidth as transient churn increases: from `489.0 B/s` (`9.75` ops) at `1%` churn to **`1,648.5 B/s` (`32.87` ops) at `20%` churn**, and actually suffers **more data losses (`5` vs `4`)** at `20%` churn due to repair queue saturation during outages.
   - `StrategyC_FspAdaptiveRepair` achieves the **exact same durability (`4` losses/10k) and availability (`0.99984`)** as `FixedRepair_T18` while consuming **`13.5%` less repair bandwidth (`359.9 B/s` vs `416.1 B/s`)** than permanent-only `FixedRepair_T18` and **`78.2%` less repair bandwidth (`359.9 B/s` vs `1,648.5 B/s`)** than suspect-timeout fixed repair at `20%` churn.

---

## 3. Suite 2 — Matched Storage Overhead Experiment (`RS(24,12)` vs. `Replication_2x`, Both $\Omega = 2.00\times$)

To eliminate unequal storage overhead as a confounding variable, Suite 2 evaluates `RS(24,12)` ($\Omega = 24/12 = 2.00\times$) against `Replication_2x` ($\Omega = 2/1 = 2.00\times$) across $M = 12$ failure domains ($1,000$ samples per strategy):

| Churn | Strategy | Overhead ($\Omega$) | Losses / 1k | $\hat{P}_{\text{loss}}(T_{\max})$ | Time-Weighted Availability | Mean Repair BW (B/s) | Mean Repair Ops |
|---|---|---|---|---|---|---|---|
| **5%** | `Replication_2x` | $2.00\times$ | `0` | `0.00%` | `0.99985` | `94.5` | `1.02` |
| **5%** | `RS(24,12) Fixed_T20` | $2.00\times$ | `0` | `0.00%` | **`1.00000`** | `455.3` | `9.08` |
| **5%** | `RS(24,12) FspAdaptive` | $2.00\times$ | `0` | `0.00%` | **`1.00000`** | **`349.1`** (`-23.3%` vs Fixed) | **`6.96`** |
| **20%** | `Replication_2x` | $2.00\times$ | `0` | `0.00%` | `0.99879` | `94.5` | `1.02` |
| **20%** | `RS(24,12) Fixed_T20` | $2.00\times$ | `0` | `0.00%` | **`1.00000`** | `454.5` | `9.06` |
| **20%** | `RS(24,12) FspAdaptive` | $2.00\times$ | `0` | `0.00%` | **`1.00000`** | **`349.2`** (`-23.2%` vs Fixed) | **`6.96`** |

- **Finding:** At equal $2.00\times$ storage overhead, `RS(24,12)` achieves **`1.00000` (five-nines+) availability** under `20%` transient churn compared to `0.99879` for `Replication_2x`, with `0` data losses. Within `RS(24,12)`, `FspAdaptive` reduces repair bandwidth by **`23.2%`** (`349.2 B/s` vs `454.5 B/s`) compared to `FixedRepair_T20`.
- **Trade-off Falsification:** Even with `FspAdaptive`, `RS(24,12)` consumes $3.7\times$ more total repair bandwidth than `Replication_2x` (`349.2 B/s` vs `94.5 B/s`) because $24$ nodes suffer $12\times$ more independent node failure events per epoch than $2$ nodes, and each RS repair reads $12$ shards ($130\text{ MB}$) whereas each replication repair transfers $2$ full objects ($240\text{ MB}$).

---

## 4. Suite 3 — High Permanent Hazard (`3%/epoch`), Multi-Node Bursts & Bandwidth Contention (`10 MB/s` vs. `1 MB/s`)

In Suite 3 ($R = 1,000$ samples, `10%` churn, $p_{\text{perm}} = 3.0\%/\text{epoch}$, $p_{\text{dom\_perm}} = 0.4\%/\text{epoch}$, plus $5\%/\text{epoch}$ probability of a 4-node simultaneous permanent burst failure):

| Bandwidth Cap | Strategy | Losses / 1k | $\hat{P}_{\text{loss}}(T_{\max})$ | Wilson 95% CI | $\widehat{\text{RMST}}(T_{\max})$ (s) | Availability | Mean Repair BW (B/s) | Mean Repair Ops |
|---|---|---|---|---|---|---|---|---|
| **10 MB/s** | `RS(20,12) Fixed_T19` | `14` | `1.40%` | `[0.84%, 2.34%]` | `2,575,902` | `0.99721` | `959.9` | `19.14` |
| **10 MB/s** | `RS(20,12) FixedSuspect_T18` | `13` | `1.30%` | `[0.76%, 2.21%]` | `2,576,974` | `0.99725` | `1,427.9` | `28.47` |
| **10 MB/s** | `RS(20,12) FspAdaptive` | `14` | `1.40%` | `[0.84%, 2.34%]` | `2,575,902` | `0.99721` | **`876.7`** | **`17.48`** |
| **1 MB/s** | `RS(20,12) Fixed_T19` | `14` | `1.40%` | `[0.84%, 2.34%]` | `2,575,901` | `0.99721` | `959.8` | `19.14` |
| **1 MB/s** | `RS(20,12) FixedSuspect_T18` | `13` | `1.30%` | `[0.76%, 2.21%]` | `2,576,974` | `0.99725` | `1,424.5` | `28.40` |
| **1 MB/s** | `RS(20,12) FspAdaptive` | `14` | `1.40%` | `[0.84%, 2.34%]` | `2,575,901` | `0.99721` | **`876.9`** | **`17.48`** |

- **Important Falsification Observation in Suite 3:**
  - Under extreme permanent hazard ($3\%/\text{epoch}$) + 4-node permanent bursts, `StrategyB_FixedRepairSuspect_T18` (which proactively repairs offline nodes after 2 hours at $62.9\%$ higher bandwidth cost: `1,427.9 B/s` vs `876.7 B/s`) saved `1` additional trajectory out of `1,000` (`13` losses vs `14` losses for `FspAdaptive` and `Fixed_T19`, within overlapping 95% Wilson CIs `[0.76%, 2.21%]` vs `[0.84%, 2.34%]`).
  - Why? Because when a node is temporarily offline during a burst-heavy regime, replacing it onto an online node increases the immediate count of *online* shards $|\mathcal{A}(t)|$ available to serve a subsequent repair if a permanent burst strikes while other nodes are still offline.
  - This confirms that `FspAdaptivePolicy`'s conservative suspect gating trades a tiny fraction of extreme-shock resilience (`0.1%` in Suite 3, or deterministic shock failure in `test_12`) to save `38.6%` repair bandwidth against suspect-triggered fixed repair.
