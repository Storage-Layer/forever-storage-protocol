# FSP Phase 2 — Limitations, Negative Findings & Falsification Report

## 1. Epistemic Classification

| Claim / Result | Epistemic Status | Evidence Source |
|---|---|---|
| Phase 1 `ClusterState::fail_node_permanently` leaked $t + 0\text{ s}$ ground-truth permanent failure notifications to repair controllers | **Verified Fact** | [`src/simulation/engine.rs`](file:///home/rade/Project/Jobs/fsp/src/simulation/engine.rs#L352-L370), [`docs/PHASE2_BASELINE_AUDIT.md`](file:///home/rade/Project/Jobs/fsp/docs/PHASE2_BASELINE_AUDIT.md) |
| Under blind failure detection (`blind_detection: Some(...)`), a naive fixed-threshold policy that reacts to suspect nodes (`FixedRepairSuspect_T18`) incurs $3.36\times$–$4.05\times$ repair bandwidth amplification at 20% churn | **Simulation Measurement** | Stage B (`[08]`: `1643.5 B/s` vs `406.2 B/s`) and Stage G (`[48]`: `1695.0 B/s` vs `503.9 B/s`) |
| `FspAdaptiveV2` achieves a statistically significant durability improvement over Phase 1 `FspAdaptiveRepair` v1 under 20% churn + 8-node burst shocks ($\Delta P(\text{loss}) = -0.1900\%$, 95% paired CI `[-0.2839%, -0.0961%]`, $21$ vs $2$ discordant survivals) | **Simulation Measurement** | Stage G paired evaluation (`N = 10,000` pairs, [results/phase2/comparison.json](file:///home/rade/Project/Jobs/fsp/results/phase2/comparison.json)) |
| Eliminating the `19/20` single-shard permanent-loss deferral vulnerability increases `FspAdaptiveV2` repair bandwidth by `+18.5%` (`+82.4 B/s`) relative to Phase 1 `FspAdaptiveRepair` v1 (`526.7 B/s` vs `444.4 B/s`) | **Simulation Measurement (Negative Trade-off)** | Stage G paired evaluation (`[49]` vs `[50]`) |
| Equal-overhead $\text{LRC}(20, 12, 4)$ ($1.6667\times$) reduces repair bandwidth by `-34.2%` (`346.4 B/s` vs `526.7 B/s`), but increases data loss under 8-node bursts (`0.5100%` vs `0.3600%`) due to $d_{\min} = 7 < 9$ | **Analytical Proof + Simulation Measurement (Negative Result)** | Gopalan bound ([`docs/PHASE2_CODING_RESEARCH.md`](file:///home/rade/Project/Jobs/fsp/docs/PHASE2_CODING_RESEARCH.md)) & Stage G (`[51]` vs `[50]`) |
| Equal-distance $\text{LRC}(22, 12, 4)$ ($1.8333\times$, $d_{\min} = 9$) reduces data loss by `6.17x` (`0.0600%` vs `0.3700%`) while reducing repair bandwidth by `-20.8%` (`399.3 B/s` vs `503.9 B/s`) | **Simulation Measurement** | Stage G (`[52]` vs `[47]`) |
| Single-step first-order risk heuristic $\hat{R}_{\text{point}}$ underestimates multi-burst trajectories when $m - k \ge 2$; conservative interval bound $[R_{\text{lower}}, R_{\text{upper}}]$ bounds single-burst tail risk | **Analytical Estimate** | [`src/reliability/risk.rs`](file:///home/rade/Project/Jobs/fsp/src/reliability/risk.rs#L65-L215), [`docs/PHASE2_RISK_MODEL.md`](file:///home/rade/Project/Jobs/fsp/docs/PHASE2_RISK_MODEL.md) |

---

## 2. Documented Negative Results & Falsifications

### 2.1 No "Free Lunch" Between Single-Shard Deferral Savings and Catastrophic Shock Margin under Pure $\text{RS}(20, 12)$
In Phase 1, `FspAdaptiveRepair` v1 achieved `13.5%` lower repair bandwidth than `FixedRepair_T18` solely by leaving single permanently lost shards ($19/20$) unrepaired until a second failure occurred.
- **Phase 2 Finding:** Once `FspAdaptiveV2` enforces `BoundedDeferralAndBurstGuard { worst_case_burst_shards: 7, max_deferral_secs: 1800.0 }` to survive 8-node shocks, it must eventually repair single-shard permanent losses after $1800\text{ s}$ of silence.
- **Consequence:** Under pure $\text{RS}(20, 12)$, `FspAdaptiveV2` consumes `526.7 B/s` (`10.50` repairs) vs `503.9 B/s` (`10.05` repairs) for oracle `FixedRepair_T18` (`+4.5%` bandwidth) because `FixedRepair_T18` leaves $19/20$ states unrepaired at the end of the 30-day horizon, whereas `FspAdaptiveV2` proactively restores the cluster to $20/20$ to maintain the 7-shard shock buffer.
- **Where `FspAdaptiveV2` Wins on Bandwidth under $\text{RS}(20, 12)$:** Against any *realistic blind* fixed-threshold policy (`FixedRepairSuspect_T18`, `1695.0 B/s`), `FspAdaptiveV2` saves **`68.9%` repair bandwidth** (`-1,168.3 B/s`) by filtering out transient outages via Bayesian silence-duration posteriors.

### 2.2 Equal-Overhead $\text{LRC}(20, 12, 4)$ Degrades Tail Durability Under Heavy Correlated Bursts
By information-theoretic necessity (the Gopalan-Huang-Simitci-Yekhanin bound), converting 3 global parities in $\text{RS}(20, 12)$ into 3 local group parities in $\text{LRC}(20, 12, 4)$ reduces minimum distance from $d_{\min} = 9$ to $d_{\min} = 7$.
- In Stage G (`N = 10,000`, 8-node bursts), $\text{LRC}(20, 12, 4)$ reduced repair bandwidth by **`34.2%`** (`346.4 B/s` vs `526.7 B/s`), but suffered **`51` losses (`0.5100%`)** compared to **`36` losses (`0.3600%`)** for $\text{RS}(20, 12)$.
- **Conclusion:** Equal-overhead $\text{LRC}(20, 12, 4)$ should **not** be deployed in environments subject to large ($\ge 7$-node) correlated burst shocks unless paired with at least 2 additional global parities ($\text{LRC}(22, 12, 4)$, $1.8333\times$ overhead), which restores $d_{\min} = 9$ and lowers losses to `6 / 10,000` (`0.0600%`).

### 2.3 Multi-Object Contention Reverses LRC Latency Advantage Under Extreme Saturation
In Stage E multi-object contention benchmarks ([results/phase2/comparison.json](file:///home/rade/Project/Jobs/fsp/results/phase2/comparison.json)):
- At `10` concurrent objects, $\text{LRC}(20, 12, 4)$ achieved `11.4%` lower mean repair latency than $\text{RS}(20, 12)$ (`29.02 s` vs `32.75 s`).
- At `50` concurrent objects (`50%` RS + `50%` LRC sharing the same 50 storage nodes), mean $\text{LRC}(20, 12, 4)$ repair latency (`127.20 s`) slightly exceeded $\text{RS}(20, 12)$ (`121.39 s`) due to head-of-line queuing behind in-flight $\text{RS}(20, 12)$ transfers on shared target nodes (`PerNodeBandwidthScheduler` FIFO admission). Priority preemption or weighted fair queuing (WFQ) for small local-group repairs is required to preserve LRC latency advantages under heavy mixed-workload saturation.

---

## 3. Remaining Architectural Assumptions & Future Work (Phase 3 Candidates)

1. **Instantaneous Replacement Hardware Provisioning Upon Permanent Confirmation:**
   While Phase 2 eliminates immediate permanent-failure detection (requiring $\tau_{\text{confirm}} = 1200\text{ s}$ or Bayesian inference) and ages replacement nodes using the same non-stationary `FailureDistribution` as initial nodes, replacement node hardware is assumed to be available in the spare pool once a node is decommissioned. Modeling finite spare-pool depletion and procurement lead times (days to weeks) remains future work.
2. **FIFO Per-Node Bandwidth Scheduling Without Preemption:**
   [`PerNodeBandwidthScheduler`](file:///home/rade/Project/Jobs/fsp/src/simulation/resource_scheduler.rs#L37-L132) schedules transfers in arrival order and reserves full bottleneck capacity per transfer rather than dynamically time-slicing TCP flows (progressive water-filling / max-min fair sharing).
3. **Homogeneous Shard Sizes Within Each Object:**
   All shards of an object have identical size `shard_size_bytes`. Heterogeneous object size distributions (e.g., Pareto/log-normal blob sizes from $64\text{ KB}$ to $64\text{ GB}$) should be evaluated in Phase 3.
4. **Absence of Byzantine / Silent Data Corruption (Bit-Rot Scrubbing):**
   Phase 2 models crash-recovery (transient outages), network packet loss, and fail-stop permanent disk/node destruction, but assumes cryptographic verification catches corruption immediately upon read. Background scrubbing schedules for latent sector errors (LSEs) remain a Phase 3 extension.
