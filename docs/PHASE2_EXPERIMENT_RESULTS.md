# FSP Phase 2 — Monte Carlo Experiment Results

## 1. Execution Summary & Methodology

All Phase 2 experiments were executed via `/usr/bin/time -v ./target/release/fsp-sim --phase2` ([`run_phase2_experiments`](file:///home/rade/Project/Jobs/fsp/src/main.rs#L653-L824)) on a 24-core Linux workstation (`Rust 1.99.1 --release`).

- **Total Batch Experiments:** 53 batches across Stages A through G (`107,000` trajectories)
- **Paired Trajectory Comparisons:** 3 paired evaluations $\times 10,000$ trajectory pairs (`30,000` additional trajectories)
- **Multi-Object Contention Benchmarks:** 2 workloads (`10` and `50` concurrent objects sharing 50 storage nodes)
- **Total Trajectories Executed:** `137,000`
- **Wall-Clock Runtime:** `993.63 s` (`16m 33.63s`, CPU utilization `1038%` across 24 worker threads)
- **Peak Resident Set Size (RSS):** `15,108 KB` (`14.75 MB`)
- **Seeds:**
  - Phase 1 Regression Seed (Stage A): `0xF591_2026_0000_0001`
  - Phase 2 Held-Out Evaluation Seed (Stages B–G): `0xF592_2026_9999_0001`
- **Machine-Readable Artifacts:**
  - [results/phase2/experiment_config.json](file:///home/rade/Project/Jobs/fsp/results/phase2/experiment_config.json)
  - [results/phase2/raw_results.jsonl](file:///home/rade/Project/Jobs/fsp/results/phase2/raw_results.jsonl)
  - [results/phase2/summary.json](file:///home/rade/Project/Jobs/fsp/results/phase2/summary.json)
  - [results/phase2/comparison.json](file:///home/rade/Project/Jobs/fsp/results/phase2/comparison.json)

---

## 2. Stage A — Phase 1 Regression Verification ($N = 1,000$, Seed `0xF591_2026_0000_0001`)

Stage A reproduces the Phase 1 20% churn mixed-correlation scenario (`blind_detection: None`) on the exact Phase 1 master seed `0xF591_2026_0000_0001`, alongside `StrategyC_FspAdaptiveV2`.

| Batch ID | Strategy | Churn | $N$ | Losses | Loss Prob (95% Wilson CI) | Mean Availability | Mean Repair BW ($\text{B/s}$) | Mean Repairs |
|---|---|---|---|---|---|---|---|---|
| `00` | `StrategyB_FixedRepair_T18` | 20% | 1,000 | 1 | `0.1000%` (`[0.0177%, 0.5643%]`) | `0.99974` | `416.9` | `8.31` |
| `01` | `StrategyC_FspAdaptiveRepair` (v1) | 20% | 1,000 | 1 | `0.1000%` (`[0.0177%, 0.5643%]`) | `0.99973` | `360.5` | `7.19` |
| `02` | `StrategyC_FspAdaptiveV2` | 20% | 1,000 | 1 | `0.1000%` (`[0.0177%, 0.5643%]`) | `0.99974` | `438.8` | `8.75` |

**Verification:** `StrategyB_FixedRepair_T18` (`416.9 B/s`, `8.31` ops, `1` loss) and `StrategyC_FspAdaptiveRepair` (`360.5 B/s`, `7.19` ops, `1` loss) match the Phase 1 [results/phase1/summary.json](file:///home/rade/Project/Jobs/fsp/results/phase1/summary.json) baseline bit-for-bit.

---

## 3. Stage B — Blind Failure Detection Validation ($N = 1,000$, Held-Out Seed `0xF592_2026_9999_0001`)

Stage B enables [`FailureDetectionConfig`](file:///home/rade/Project/Jobs/fsp/src/detection/mod.rs#L65-L90) (`blind_detection: Some(...)`), removing the Phase 1 $t + 0\text{ s}$ ground-truth permanent-failure notification and requiring all policies to infer node status from missed heartbeats. We compare `StrategyB_FixedRepair_T18` (oracle permanent-loss knowledge), `StrategyB_FixedRepairSuspect_T18` (blind fixed-threshold baseline that triggers repair when suspect + confirmed lost shards drop to $\le 18$), `StrategyC_FspAdaptiveRepair` (v1), and `StrategyC_FspAdaptiveV2`.

| Batch ID | Strategy | Churn | $N$ | Losses | Mean Availability | Mean Repair BW ($\text{B/s}$) | Mean Repairs | BW vs `FixedSuspect_T18` |
|---|---|---|---|---|---|---|---|---|
| `03` | `StrategyB_FixedRepair_T18` (oracle) | 5% | 1,000 | 0 | `1.00000` | `406.1` | `8.10` | `-42.9%` |
| `04` | `StrategyB_FixedRepairSuspect_T18` (blind) | 5% | 1,000 | 0 | `1.00000` | `711.1` | `14.18` | baseline |
| `05` | `StrategyC_FspAdaptiveRepair` (v1) | 5% | 1,000 | 0 | `1.00000` | `348.7` | `6.95` | `-51.0%` |
| `06` | `StrategyC_FspAdaptiveV2` (blind) | 5% | 1,000 | 0 | `1.00000` | `426.7` | `8.51` | **`-40.0%`** |
| `07` | `StrategyB_FixedRepair_T18` (oracle) | 20% | 1,000 | 0 | `0.99994` | `406.2` | `8.10` | `-75.3%` |
| `08` | `StrategyB_FixedRepairSuspect_T18` (blind) | 20% | 1,000 | 0 | `0.99994` | `1643.5` | `32.77` | baseline |
| `09` | `StrategyC_FspAdaptiveRepair` (v1) | 20% | 1,000 | 0 | `0.99993` | `347.8` | `6.93` | `-78.8%` |
| `10` | `StrategyC_FspAdaptiveV2` (blind) | 20% | 1,000 | 0 | `0.99995` | `426.3` | `8.50` | **`-74.1%`** |

**Finding:** Under blind failure detection at 20% transient churn, a naive fixed-threshold policy that reacts to suspect nodes (`FixedRepairSuspect_T18`) suffers **`4.05x` repair amplification** (`32.77` repairs vs `8.10` actual permanent-failure repairs, `1643.5 B/s` vs `406.2 B/s`). `FspAdaptiveV2` filters out transient outages using the Bayesian permanent-failure posterior $\hat{p}_{\text{perm}}(\Delta t_{\text{silent}})$ and repairs every true permanent failure (including single-shard `19/20` losses after bounded deferral), achieving **`426.3 B/s`** (`8.50` repairs — within `4.9%` of the oracle `FixedRepair_T18` bandwidth and **`74.1%` lower** than `FixedRepairSuspect_T18`).

---

## 4. Stage C — Strategy Comparison & Non-Stationary Weibull Aging ($N = 1,000$)

### 4.1 30-Day Mixed-Correlation Comparison (10% and 20% Churn)

| Batch ID | Strategy | Churn | Storage Overhead | Losses | Loss Prob | Mean Availability | Mean Repair BW ($\text{B/s}$) | Mean Repairs |
|---|---|---|---|---|---|---|---|---|
| `11` | `StrategyA_Replication_2x` | 10% | $2.0000\times$ | 1 | `0.1000%` | `0.99918` | `90.4` | `0.98` |
| `12` | `StrategyA_Replication_3x` | 10% | $3.0000\times$ | 0 | `0.0000%` | `0.99999` | `135.5` | `1.46` |
| `13` | `StrategyB_FixedRepair_T18` | 10% | $1.6667\times$ | 0 | `0.0000%` | `0.99999` | `406.1` | `8.10` |
| `14` | `StrategyB_FixedRepairSuspect_T18` | 10% | $1.6667\times$ | 0 | `0.0000%` | `0.99999` | `1016.2` | `20.26` |
| `15` | `StrategyC_FspAdaptiveRepair` (v1) | 10% | $1.6667\times$ | 0 | `0.0000%` | `0.99998` | `348.7` | `6.95` |
| `16` | `StrategyC_FspAdaptiveV2` | 10% | $1.6667\times$ | 0 | `0.0000%` | `0.99999` | `426.5` | `8.50` |
| `17` | `StrategyA_Replication_2x` | 20% | $2.0000\times$ | 1 | `0.1000%` | `0.99828` | `90.6` | `0.98` |
| `18` | `StrategyA_Replication_3x` | 20% | $3.0000\times$ | 0 | `0.0000%` | `0.99996` | `135.9` | `1.47` |
| `19` | `StrategyB_FixedRepair_T18` | 20% | $1.6667\times$ | 0 | `0.0000%` | `0.99994` | `406.2` | `8.10` |
| `20` | `StrategyB_FixedRepairSuspect_T18` | 20% | $1.6667\times$ | 0 | `0.0000%` | `0.99994` | `1643.5` | `32.77` |
| `21` | `StrategyC_FspAdaptiveRepair` (v1) | 20% | $1.6667\times$ | 0 | `0.0000%` | `0.99993` | `347.8` | `6.93` |
| `22` | `StrategyC_FspAdaptiveV2` | 20% | 1,000 | 0 | `0.0000%` | `0.99995` | `426.3` | `8.50` |

### 4.2 90-Day Non-Stationary Weibull Wear-Out ($\beta = 1.8, \eta = 180\text{ days}$, $N = 1,000$)

| Batch ID | Strategy | Horizon | Losses | Loss Prob (95% CI) | Mean Availability | Mean Repair BW ($\text{B/s}$) | Mean Repairs |
|---|---|---|---|---|---|---|---|
| `23` | `StrategyB_FixedRepair_T18` | 90 days | 1 | `0.1000%` (`[0.0177%, 0.5643%]`) | `0.99957` | `28.0` | `1.67` |
| `24` | `StrategyC_FspAdaptiveRepair` (v1) | 90 days | **2** | **`0.2000%`** (`[0.0549%, 0.7263%]`) | `0.99939` | `17.4` | `1.04` |
| `25` | `StrategyC_FspAdaptiveV2` | 90 days | **1** | **`0.1000%`** (`[0.0177%, 0.5643%]`) | `0.99957` | `32.2` | `1.93` |

**Finding:** Under accelerating Weibull hardware wear-out ($\beta = 1.8$) over 90 days, Phase 1 `FspAdaptiveRepair` (v1) suffers `2` losses (`0.20%`) because leaving `19/20` shards unrepaired exposes the object to late-stage wear-out + correlated shocks. `FspAdaptiveV2` eliminates the extra loss (`1` loss, matching `FixedRepair_T18`).

---

## 5. Stage D — Correlated Stress & Catastrophic Burst Suite ($N = 1,000$)

Stage D tests 2-node, 4-node, and 8-node correlated permanent burst shocks at rate $\lambda_{\text{burst}} = 1.5 \times 10^{-6}\text{ s}^{-1}$.

| Batch ID | Burst Size | Strategy | Losses | Loss Prob (95% CI) | Mean Availability | Mean Repair BW ($\text{B/s}$) | Mean Repairs |
|---|---|---|---|---|---|---|---|
| `26` | 2 nodes | `StrategyB_FixedRepair_T19` | 0 | `0.0000%` | `0.99999` | `521.2` | `10.39` |
| `27` | 2 nodes | `StrategyB_FixedRepair_T18` | 0 | `0.0000%` | `0.99999` | `498.9` | `9.95` |
| `28` | 2 nodes | `StrategyC_FspAdaptiveRepair` (v1) | 0 | `0.0000%` | `0.99998` | `438.0` | `8.73` |
| `29` | 2 nodes | `StrategyC_FspAdaptiveV2` | 0 | `0.0000%` | `0.99999` | `520.9` | `10.38` |
| `30` | 4 nodes | `StrategyB_FixedRepair_T19` | 1 | `0.1000%` | `0.99981` | `550.0` | `10.97` |
| `31` | 4 nodes | `StrategyB_FixedRepair_T18` | 1 | `0.1000%` | `0.99981` | `528.1` | `10.53` |
| `32` | 4 nodes | `StrategyC_FspAdaptiveRepair` (v1) | 1 | `0.1000%` | `0.99980` | `466.0` | `9.29` |
| `33` | 4 nodes | `StrategyC_FspAdaptiveV2` | 1 | `0.1000%` | `0.99981` | `549.9` | `10.96` |
| `34` | **8 nodes** | `StrategyB_FixedRepair_T19` | **2** | **`0.2000%`** (`[0.0549%, 0.7263%]`) | `0.99963` | `524.5` | `10.46` |
| `35` | **8 nodes** | `StrategyB_FixedRepair_T18` | **2** | **`0.2000%`** (`[0.0549%, 0.7263%]`) | `0.99963` | `502.8` | `10.03` |
| `36` | **8 nodes** | `StrategyC_FspAdaptiveRepair` (v1) | **4** | **`0.4000%`** (`[0.1557%, 1.0240%]`) | `0.99877` | `442.4` | `8.82` |
| `37` | **8 nodes** | `StrategyC_FspAdaptiveV2` | **2** | **`0.2000%`** (`[0.0549%, 0.7263%]`) | `0.99963` | `524.4` | `10.46` |

**Finding:** Under 8-node catastrophic bursts, Phase 1 `FspAdaptiveRepair` (v1) doubles the data-loss rate (`4` losses vs `2`, `0.40%` vs `0.20%`) due to the `19/20` single-shard permanent-loss deferral vulnerability identified in Phase 1. `FspAdaptiveV2` with `BoundedDeferralAndBurstGuard` (`worst_case_burst_shards: 7, max_deferral_secs: 1800.0`) completely eliminates those extra losses (`2` losses, matching `FixedRepair_T19` and `FixedRepair_T18`).

---

## 6. Stage E — Constrained Bandwidth Regimes ($N = 1,000$)

Stage E evaluates blind failure detection under abundant (`10 MB/s`) and constrained (`500 KB/s`) cluster repair ceilings, plus multi-object per-node NIC contention (`10` and `50` objects sharing 50 storage nodes).

### 6.1 Single-Object Cluster Ceiling (`10 MB/s` vs `500 KB/s`)

| Batch ID | Ceiling | Strategy | Losses | Mean Availability | Mean Repair BW ($\text{B/s}$) | Mean Repairs |
|---|---|---|---|---|---|---|
| `38` | `10 MB/s` | `StrategyB_FixedRepairSuspect_T18` | 0 | `0.99999` | `1169.7` | `23.32` |
| `39` | `10 MB/s` | `StrategyC_FspAdaptiveRepair` (v1) | 0 | `0.99999` | `490.5` | `9.78` |
| `40` | `10 MB/s` | `StrategyC_FspAdaptiveV2` | 0 | `0.99999` | `576.3` | `11.49` |
| `41` | `500 KB/s` | `StrategyB_FixedRepairSuspect_T18` | 0 | `0.99999` | `1157.7` | `23.08` |
| `42` | `500 KB/s` | `StrategyC_FspAdaptiveRepair` (v1) | 0 | `0.99999` | `490.7` | `9.78` |
| `43` | `500 KB/s` | `StrategyC_FspAdaptiveV2` | 0 | `0.99999` | `576.4` | `11.49` |

### 6.2 Multi-Object Per-Node NIC Contention (`10` vs `50` Concurrent Objects)

Evaluated via [`MultiObjectSimulationEngine`](file:///home/rade/Project/Jobs/fsp/src/simulation/workloads.rs#L65-L185) (`50%` $\text{RS}(20, 12)$ objects + `50%` $\text{LRC}(20, 12, 4)$ objects sharing 50 nodes with per-node NIC limits):

| Concurrent Objects | Aggregate Repair Bytes | Aggregate Repair BW ($\text{B/s}$) | Object Loss Fraction | Mean $\text{RS}(20,12)$ Repair Latency | Mean $\text{LRC}(20,12,4)$ Repair Latency |
|---|---|---|---|---|---|
| `10` objects | `8,000,000,000 B` (`8.00 GB`) | `3,086.4 B/s` | `0.0000` | `32.75 s` | `29.02 s` (`-11.4%`) |
| `50` objects | `36,170,000,000 B` (`36.17 GB`) | `13,954.5 B/s` | `0.0000` | `121.39 s` | `127.20 s` (`3.88x` contention queuing) |

---

## 7. Stage F — Coding Scheme Comparison (`RS(20,12)` vs `LRC(20,12,4)` vs `LRC(22,12,4)`, $N = 1,000$)

Stage F evaluates `StrategyC_FspAdaptiveV2` across the three coding architectures under normal 10% churn (`blind_detection: Some(...)`).

| Batch ID | Coding Scheme | $(n, k, l)$ | Storage Overhead | $d_{\min}$ | Losses | Mean Availability | Mean Repair BW ($\text{B/s}$) | Mean Repairs | BW vs `RS(20,12)` |
|---|---|---|---|---|---|---|---|---|---|
| `44` | $\text{RS}(20, 12)$ | $(20, 12, -)$ | $1.6667\times$ | $9$ | 0 | `0.99999` | `426.5` | `8.50` | baseline |
| `45` | $\text{LRC}(20, 12, 4)$ | $(20, 12, 4)$ | $1.6667\times$ | $7$ | 0 | `0.99996` | **`257.2`** | `8.50` | **`-39.7%`** |
| `46` | $\text{LRC}(22, 12, 4)$ | $(22, 12, 4)$ | $1.8333\times$ | $9$ | 0 | `1.00000` | **`297.2`** | `9.33` | **`-30.3%`** |

---

## 8. Stage G — Expanded 10,000-Run Evaluation & Paired Statistical Significance

Stage G runs $N = 10,000$ independent trajectories per configuration under 20% churn + 8-node burst shocks (`burst_failure_rate: 1.0e-6`, `burst_size: 8`) with blind failure detection (`blind_detection: Some(...)`) on held-out seed `0xF592_2026_9999_0001`.

### 8.1 Stage G Summary Table ($N = 10,000$ per row)

| Batch ID | Strategy & Coding Scheme | Storage Overhead | Losses / $10,000$ | Loss Prob (95% Wilson CI) | Mean Availability | Mean Repair BW ($\text{B/s}$) | Mean Repairs |
|---|---|---|---|---|---|---|---|
| `47` | `StrategyB_FixedRepair_T18` ($\text{RS}(20,12)$, oracle) | $1.6667\times$ | `37` | `0.3700%` (`[0.2686%, 0.5096%]`) | `0.99906` | `503.9` | `10.05` |
| `48` | `StrategyB_FixedRepairSuspect_T18` ($\text{RS}(20,12)$, blind) | $1.6667\times$ | `38` | `0.3800%` (`[0.2770%, 0.5211%]`) | `0.99900` | `1695.0` | `33.80` |
| `49` | `StrategyC_FspAdaptiveRepair` v1 ($\text{RS}(20,12)$, blind) | $1.6667\times$ | `55` | `0.5500%` (`[0.4228%, 0.7152%]`) | `0.99828` | `444.4` | `8.86` |
| `50` | `StrategyC_FspAdaptiveV2` ($\text{RS}(20,12)$, blind) | $1.6667\times$ | **`36`** | **`0.3600%`** (`[0.2602%, 0.4980%]`) | **`0.99910`** | `526.7` | `10.50` |
| `51` | `StrategyC_FspAdaptiveV2` ($\text{LRC}(20,12,4)$, blind) | $1.6667\times$ | `51` | `0.5100%` (`[0.3881%, 0.6699%]`) | `0.99862` | **`346.4`** | `10.50` |
| `52` | `StrategyC_FspAdaptiveV2` ($\text{LRC}(22,12,4)$, blind) | $1.8333\times$ | **`6`** | **`0.0600%`** (`[0.0275%, 0.1309%]`) | **`0.99988`** | **`399.3`** | `11.58` |

### 8.2 Paired Trajectory Difference Analysis ($N = 10,000$ Pairs, Shared Exogenous Traces)

Computed via [`run_paired_monte_carlo_comparison`](file:///home/rade/Project/Jobs/fsp/src/simulation/monte_carlo.rs#L171-L268) and recorded in [results/phase2/comparison.json](file:///home/rade/Project/Jobs/fsp/results/phase2/comparison.json):

| Comparison ($A$ vs $B$) | Discordant Survivals ($A\text{ survived, } B\text{ lost}$ vs $B\text{ survived, } A\text{ lost}$) | $\Delta P(\text{loss})$ (95% Paired CI) | Stat. Sig. (Loss)? | $\Delta \text{BW}$ ($\text{B/s}$, 95% Paired CI) | Stat. Sig. (BW)? | $\Delta \text{Avail}$ (95% Paired CI) |
|---|---|---|---|---|---|---|
| `FspAdaptiveV2` vs `FspAdaptiveRepair` v1 ($\text{RS}(20,12)$) | **`21` vs `2`** | **`-0.1900%`** (`[-0.2839%, -0.0961%]`) | **YES ($p < 0.001$)** | `+82.36` (`[+80.88, +83.84]`) | **YES** | **`+0.000825`** (`[+0.000413, +0.001236]`) |
| `FspAdaptiveV2` vs `FixedRepair_T18` ($\text{RS}(20,12)$) | `1` vs `0` | `-0.0100%` (`[-0.0296%, +0.0096%]`) | No (Matched) | `+22.86` (`[+22.18, +23.53]`) | YES | `+0.000038` (`[-0.000033, +0.000109]`) |
| `FspAdaptiveV2` vs `FixedRepairSuspect_T18` ($\text{RS}(20,12)$) | `3` vs `1` | `-0.0200%` (`[-0.0592%, +0.0192%]`) | No (Matched) | **`-1168.27`** (`[-1182.77, -1153.77]`) | **YES** | `+0.000104` (`[-0.000033, +0.000240]`) |

### 8.3 Key Experimental Conclusions

1. **`FspAdaptiveV2` Statistically Significantly Outperforms Phase 1 `FspAdaptiveRepair` in Durability ($p < 0.001$):**
   On 10,000 paired trajectories under 20% churn + 8-node burst shocks, `FspAdaptiveV2` reduces data loss from `55` (`0.5500%`) to `36` (`0.3600%`), surviving `21` trajectories where Phase 1 `FspAdaptiveRepair` suffered irrecoverable data loss (`95% paired CI [-0.2839%, -0.0961%]`).
2. **`FspAdaptiveV2` Matches Oracle `FixedRepair_T18` Durability Without Ground-Truth Leakage, While Cutting Blind Fixed-Threshold Bandwidth by `68.9%`:**
   Under blind failure detection, `FspAdaptiveV2` achieves `36` losses (`0.3600%`) vs `37` losses (`0.3700%`) for oracle `FixedRepair_T18` and `38` losses (`0.3800%`) for blind `FixedRepairSuspect_T18`, while reducing repair bandwidth by **`-1,168.3 B/s` (`-68.9%`)** compared to `FixedRepairSuspect_T18` (`526.7 B/s` vs `1695.0 B/s`).
3. **Equal-Overhead $\text{LRC}(20, 12, 4)$ Reduces Bandwidth by `34.2%` at the Cost of Higher Burst Vulnerability:**
   Keeping storage overhead fixed at $1.6667\times$, $\text{LRC}(20, 12, 4)$ cuts `FspAdaptiveV2` repair bandwidth from `526.7 B/s` to **`346.4 B/s` (`-34.2%`)**, but its smaller minimum distance ($d_{\min} = 7$ vs $9$) increases data losses under 8-node bursts from `36` (`0.36%`) to `51` (`0.51%`).
4. **Equal-Distance $\text{LRC}(22, 12, 4)$ Breaks the Reliability-Cost Pareto Frontier:**
   With $+10\%$ storage overhead ($1.8333\times$ vs $1.6667\times$), `FspAdaptiveV2` + $\text{LRC}(22, 12, 4)$ simultaneously achieves:
   - **`6.17x` lower data-loss probability** (`0.0600%`, `6` losses vs `37` losses for `FixedRepair_T18` $\text{RS}(20,12)$ — non-overlapping 95% Wilson CIs `[0.0275%, 0.1309%]` vs `[0.2686%, 0.5096%]`), and
   - **`20.8%` lower repair bandwidth** (`399.3 B/s` vs `503.9 B/s` for oracle `FixedRepair_T18`, and `-76.4%` vs blind `FixedRepairSuspect_T18`).
