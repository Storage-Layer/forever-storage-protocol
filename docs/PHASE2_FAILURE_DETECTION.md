# FSP Phase 2: Blind Failure Detection Architecture & Observable State Isolation

## 1. Epistemic Boundary: `PhysicalNodeState` vs. `ObservedNodeState`

In a decentralized storage protocol, a repair scheduler has **zero direct access** to whether a silent storage node suffered an irreversible disk failure or a transient network/power outage. To prevent hidden simulator information leakage, Phase 2 enforces a strict compile-time and runtime boundary (`src/detection/mod.rs`).

### 1.1 Simulator Ground Truth (`PhysicalNodeState` — Hidden from Policies)
- `Online`: Node is powered on, network-reachable, and serving valid shards.
- `TemporarilyOffline { since_secs, recover_at_secs }`: Node is unreachable due to transient churn or domain outage; shards on disk remain intact.
- `Recovering { since_secs, online_at_secs }`: Node is rebooting / catching up after a transient outage before serving reads/probes.
- `PermanentlyFailed { failed_at_secs }`: Node's storage media has suffered irreversible destruction; all hosted shards are permanently lost.
- `Replacing { failed_at_secs, ready_at_secs }`: Replacement hardware/provider is being provisioned and will become an empty `Online` node only after `node_replacement_delay_secs` elapses at `ready_at_secs`.

### 1.2 Policy-Observable State (`ObservedNodeState` — Exposed via `ClusterObservationView`)
Repair policies operating in blind-detection mode receive only `ObservedNodeTelemetry` per node:
- `Healthy { last_response_secs }`: Node responded within the expected heartbeat/probe window.
- `SuspectedUnavailable { first_missed_secs, consecutive_misses, failed_probes }`: Node has missed $\ge m_{\text{suspect}}$ consecutive heartbeats or challenge-response probes, but has not yet crossed the policy confirmation threshold.
- `ConfirmedUnavailableByPolicy { first_missed_secs, confirmed_at_secs, consecutive_misses, failed_probes, posterior_permanent_prob }`: Node has exceeded the missed-heartbeat + probe confirmation criteria (`consecutive_misses >= missed_heartbeat_threshold` and `failed_probes >= required_failed_probes`, or silence duration $\Delta t_{\text{silent}} \ge \tau_{\text{confirm}}$).
  - **Crucial Distinction:** Entering `ConfirmedUnavailableByPolicy` does **not** mean the physical disk is destroyed; if the underlying physical state was a long `TemporarilyOffline` outage, the node can still recover later!
- `RecoveryObserved { recovered_at_secs, previous_outage_duration_secs }`: Node has resumed responding to heartbeats/probes with valid cryptographic shard proofs after a suspected or confirmed outage.
- `Unknown`: Initial state prior to first heartbeat/probe exchange.

---

## 2. Heartbeat, Challenge-Response Probe & Network Imperfection Model

The `FailureDetector` (`src/detection/mod.rs`) models realistic network telemetry parameterized by `FailureDetectionConfig`:

1. **Periodic Heartbeats (`heartbeat_interval_secs`, `heartbeat_timeout_secs`):**
   - Every $\Delta t_{\text{hb}}$ seconds, each node attempts to emit a heartbeat.
   - If the node's `PhysicalNodeState` is `Online`, the heartbeat arrives after `probe_latency_secs + jitter` unless dropped by independent network packet loss (`packet_loss_probability` $p_{\text{drop}} \in [0, 1)$).
   - If dropped by packet loss or if the node is physically offline/failed/replacing, the monitor records a missed heartbeat after `heartbeat_timeout_secs`.

2. **Active Challenge-Response Probes (`challenge_probes_on_miss`):**
   - To suppress false positives caused by transient packet loss $p_{\text{drop}}$, when a node reaches `missed_heartbeat_threshold` consecutive missed heartbeats, the detector issues active cryptographic storage challenge-response probes separated by `probe_latency_secs + jitter`.
   - For a physically `Online` node, the probability that $K_{\text{hb}}$ heartbeats **and** $K_{\text{probe}}$ active probes all fail solely due to uncorrelated packet loss is $p_{\text{drop}}^{K_{\text{hb}} + K_{\text{probe}}}$. For $p_{\text{drop}} = 0.05$, $K_{\text{hb}} = 3$, $K_{\text{probe}} = 2$, the packet-loss false-positive rate is $0.05^5 = 3.125 \times 10^{-7}$.

3. **Bayesian Posterior Probability of Permanent Failure ($\hat{p}_{\text{perm}}(\Delta t_{\text{silent}})$):**
   - Given that a node has been continuously silent for duration $\Delta t_{\text{silent}} = t_{\text{now}} - t_{\text{first\_missed}}$ (after ruling out packet loss), the detector computes the observable Bayesian posterior probability that the outage is a permanent failure rather than a transient outage of mean duration $\hat{\mu}_{\text{outage}}$:
     - Prior probability that an outage initiation is permanent:
       $$\pi_0 = \frac{\hat{\lambda}_{\text{perm}}}{\hat{\lambda}_{\text{perm}} + \hat{\lambda}_{\text{outage}}}$$
     - Under an exponential transient outage survival tail $P(D_{\text{outage}} > \Delta t_{\text{silent}}) = e^{-\Delta t_{\text{silent}} / \hat{\mu}_{\text{outage}}}$, while a permanent failure remains silent with probability $1$:
       $$\hat{p}_{\text{perm}}(\Delta t_{\text{silent}}) = \frac{\pi_0}{\pi_0 + (1 - \pi_0) e^{-\Delta t_{\text{silent}} / \hat{\mu}_{\text{outage}}}}$$
   - Notice that at $\Delta t_{\text{silent}} = 0$, $\hat{p}_{\text{perm}}(0) = \pi_0$ (small when transient churn dominates). As $\Delta t_{\text{silent}} \gg \hat{\mu}_{\text{outage}}$, $e^{-\Delta t_{\text{silent}} / \hat{\mu}_{\text{outage}}} \to 0$ and $\hat{p}_{\text{perm}}(\Delta t_{\text{silent}}) \to 1.0$.

4. **Correlated Domain Telemetry (`DomainCorrelationSignal`):**
   - The detector tracks the fraction of nodes within each `FailureDomainId` that entered `SuspectedUnavailable` or `ConfirmedUnavailableByPolicy` within a sliding coincidence window $\Delta t_{\text{coincidence}}$.
   - When $\ge 50\%$ of nodes in domain $d$ become unreachable simultaneously, the detector emits a `DomainCorrelationSignal::CorrelatedDomainEvent { domain_id, affected_fraction }`, alerting the risk estimator and `FspAdaptiveV2` that a domain-wide event is underway.

5. **Observability Metrics Tracked by `FailureDetector`:**
   - **Detection Delay ($\Delta t_{\text{detect}}$):** For true permanent failures, $t_{\text{confirmed}} - t_{\text{physical\_fail}}$.
   - **False-Positive Repair Count:** Repairs initiated for shards on nodes that were physically `TemporarilyOffline` (or `Online` with packet loss) and subsequently recovered.
   - **Cancelled Redundant Repairs:** In-flight repairs aborted early when `RecoveryObserved` occurs before transfer completion, saving the remaining unspent transfer bandwidth.
   - **Missed Critical Repair Count:** Instances where a true permanent failure remained unconfirmed at the moment irrecoverable data loss occurred.
