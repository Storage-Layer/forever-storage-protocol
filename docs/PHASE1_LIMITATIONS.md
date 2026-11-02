# FSP Phase 1: Mathematical, Modeling & Experimental Limitations

To uphold scientific rigor and prevent overclaiming, this document enumerates the verified boundaries, simplifying assumptions, and falsification conditions of the Phase 1 simulation model.

---

## 1. Falsification & Underperformance Regimes Identified for FSP Adaptive Repair

In accordance with the Phase 1 rule (*"Do not assume FSP is superior. Attempt to falsify its reliability and efficiency claims"*), we explicitly identify and test scenarios where `FspAdaptivePolicy` underperforms baseline strategies:

### 1.1 Deferred Repair Vulnerability to Sudden Unmodeled Shocks
- **Mechanism:** `FspAdaptivePolicy` intentionally defers repairing a single permanently lost shard when $u_{\text{eff}} = 19$ out of $n = 20$ (with $k = 12$) and domain concentration exposure is low ($P < \theta_{\text{trigger}} = 0.22$). This batching reduces repair operation invocations and avoids competing with foreground traffic during mild degradation.
- **Falsification Scenario (`test_12_falsification_scenarios_where_adaptive_repair_performs_worse`):**
  - If a cluster loses 1 shard at $t = 100\text{ s}$ (leaving $19$ shards) and an unpredicted, catastrophic $8$-shard simultaneous permanent shock strikes at $t = 200\text{ s}$:
  - An **eager** `FixedRepairStrategy(threshold = 19)` that immediately repaired the first shard at $t = 115\text{ s}$ enters the shock with $20$ shards and survives with $20 - 8 = 12 = k$ shards.
  - The **deferred** `FspAdaptivePolicy` enters the shock with $19$ shards and suffers irrecoverable data loss ($19 - 8 = 11 < 12$).
- **Implication:** Any adaptive policy that trades immediate redundancy restoration for bandwidth/cost deferral inherently sacrifices a small tail-margin against instantaneous, unpredicted multi-node shocks of size $n - k$.

### 1.2 Per-Shard Reconstruction Read Amplification vs. Replication
- **Mechanism:** In Reed–Solomon $(n, k)$ erasure coding without local reconstruction codes (LRCs) or regenerating codes (minimum-storage regenerating / Clay codes), repairing **one** lost shard of size $S_{\text{shard}} = S_{\text{obj}} / k$ requires reading $k$ shards ($k \cdot S_{\text{shard}} = S_{\text{obj}}$) plus writing $1$ shard ($S_{\text{obj}} / k$), totaling $\frac{k + 1}{k} S_{\text{obj}}$ bytes.
- **Comparison with Replication:** Under $R$-way replication, repairing a lost replica requires reading $1 \cdot S_{\text{obj}}$ and writing $1 \cdot S_{\text{obj}}$ ($2.0 S_{\text{obj}}$ bytes), **but** because an RS$(20, 12)$ stripe spans $20$ distinct nodes whereas $2\times$ or $3\times$ replication spans only $2$ or $3$ nodes, the **node-footprint exposure** of a single RS$(20,12)$ stripe to independent node failures is $20 / 3 \approx 6.67\times$ higher per stripe!
- **Experimental Consequence:** When comparing a *single* object stored on $20$ nodes via RS$(20, 12)$ against a *single* object stored on $3$ nodes via $3\times$ replication under uniform per-node failure hazard $\lambda_{\text{node}}$, the RS$(20, 12)$ stripe experiences $6.67\times$ more shard failure events per unit time (though each shard is $1/12$th the size).

---

## 2. Structural & Mathematical Assumptions in Phase 1

1. **Cluster-Wide Serialized Repair Pipe vs. Per-Node Network Topology:**
   - Repair bandwidth is modeled as an aggregate cluster-wide token/pipe limit $B_{\max}$ (bytes/sec) with FIFO queueing (`repair_channel_free_at_secs`).
   - Full bipartite per-node ingress/egress NIC contention, cross-rack switch oversubscription, and multi-object repair queue starvation are not yet modeled.

2. **Known Failure Type vs. Imperfect Timeout Inference:**
   - In `ClusterState`, when a node suffers a `NodePermanentFailure`, its shard is immediately removed from the ground-truth shard table (simulating an explicit disk-loss alert or failed cryptographic proof of storage), whereas a `NodeOutage` leaves the shard in `TemporarilyOffline` until `unreachable_suspect_timeout_secs` ($\tau_{\text{suspect}}$) elapses.
   - In a fully decentralized adversarial network without out-of-band hardware telemetry, a controller cannot distinguish `NodePermanentFailure` from `NodeOutage` until $\tau_{\text{suspect}}$ expires. Modeling purely timeout-blind detection for all permanent failures is recommended for Phase 2.

3. **Stationary Epoch Hazard Rates:**
   - Per-epoch probabilities ($p_{\text{churn}}, p_{\text{perm}}, p_{\text{dom\_out}}, p_{\text{dom\_perm}}, p_{\text{burst}}$) are stationary across epochs $0 \dots H-1$.
   - Real hardware exhibits bathtub-curve (Weibull aging) hazard rates where infant mortality ($\beta < 1$) and wear-out ($\beta > 1$) cause non-stationary failure clustering over multi-year horizons.

4. **Permanent Domain Failure Absorbing Capacity:**
   - When an entire failure domain suffers a `DomainPermanentFailure`, all nodes in that domain transition to `PermanentlyFailed` and are **not** automatically re-provisioned in that destroyed domain. If enough entire domains are permanently destroyed such that remaining healthy domains cannot hold $n$ shards under `max_shards_per_domain`, partial repair continues up to the surviving domain capacity.

5. **Right-Censoring of Unrestricted MTTDL:**
   - As proven in `src/reliability/statistics.rs`, whenever any Monte Carlo trajectories survive past $T_{\max}$ (`right_censored_samples > 0`), unrestricted $\text{MTTDL} = \int_0^\infty S(t)\,dt$ is non-identifiable without parametric extrapolation. Phase 1 strictly reports non-parametric **Restricted Mean Survival Time ($\text{RMST}(T_{\max})$)** and **Wilson 95% Confidence Intervals on $\hat{P}_{\text{loss}}(T_{\max})$**.
