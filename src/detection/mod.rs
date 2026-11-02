//! Phase 2 Blind Failure Detection & Ground-Truth Isolation.
//!
//! Strictly separates hidden simulator physical states (`PhysicalNodeState`) from
//! policy-observable network telemetry (`ObservedNodeState`).
//! No repair strategy may access `PhysicalNodeState` directly.

use crate::config::SECONDS_PER_EPOCH;
use crate::model::failure_domain::FailureDomainId;
use crate::model::node::NodeId;
use crate::simulation::engine::DeterministicRng;

/// Hidden physical ground-truth state of a storage node inside the simulator.
/// Never exposed to blind repair policies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PhysicalNodeState {
    /// Node is online and serving shards.
    Online,
    /// Node is temporarily offline; shards on disk remain intact.
    TemporarilyOffline {
        since_secs: f64,
        recover_at_secs: f64,
    },
    /// Node is rebooting/warming up after a temporary outage before serving traffic.
    Recovering {
        since_secs: f64,
        online_at_secs: f64,
    },
    /// Node has suffered irreversible storage media failure.
    PermanentlyFailed { failed_at_secs: f64 },
    /// Replacement hardware/provider is being provisioned and will become an empty Online host at `ready_at_secs`.
    Replacing {
        failed_at_secs: f64,
        ready_at_secs: f64,
    },
}

impl PhysicalNodeState {
    /// Returns true if the node is currently able to respond to heartbeats/probes over the network.
    pub const fn is_network_reachable(&self) -> bool {
        matches!(self, Self::Online)
    }

    /// Returns true if the node's persistent disk still holds intact shards.
    pub const fn is_disk_intact(&self) -> bool {
        matches!(
            self,
            Self::Online | Self::TemporarilyOffline { .. } | Self::Recovering { .. }
        )
    }
}

/// Snapshot of physical node states passed from the simulation engine to the `FailureDetector`
/// (never to repair strategies).
#[derive(Debug, Clone, PartialEq)]
pub struct PhysicalClusterSnapshot {
    /// Current simulation timestamp in seconds.
    pub now_secs: f64,
    /// Physical states of each storage node.
    pub node_states: Vec<(NodeId, FailureDomainId, PhysicalNodeState)>,
}

/// Information available to a blind repair policy about a storage node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ObservedNodeState {
    /// Initial state before any heartbeat or probe has completed.
    Unknown,
    /// Node responded to the most recent heartbeat or challenge-response probe.
    Healthy { last_response_secs: f64 },
    /// Node has missed `consecutive_misses >= 1` heartbeats, but has not yet reached confirmation threshold.
    SuspectedUnavailable {
        first_missed_secs: f64,
        consecutive_misses: u32,
        failed_probes: u32,
        posterior_permanent_prob: f64,
    },
    /// Node has exceeded `missed_heartbeat_threshold` and failed `challenge_probes_on_miss` active probes.
    /// Note: This is a policy-level timeout confirmation, NOT a guarantee of physical disk destruction!
    ConfirmedUnavailableByPolicy {
        first_missed_secs: f64,
        confirmed_at_secs: f64,
        consecutive_misses: u32,
        failed_probes: u32,
        posterior_permanent_prob: f64,
    },
    /// Node was previously suspected or confirmed unavailable and has resumed responding with valid proofs.
    RecoveryObserved {
        recovered_at_secs: f64,
        previous_outage_duration_secs: f64,
    },
}

impl ObservedNodeState {
    /// Returns true if the node is currently observed as reachable (`Healthy` or `RecoveryObserved`).
    pub const fn is_observed_reachable(&self) -> bool {
        matches!(
            self,
            Self::Healthy { .. } | Self::RecoveryObserved { .. } | Self::Unknown
        )
    }

    /// Returns the Bayesian posterior probability $\hat{p}_{\text{perm}} \in [0, 1]$ that the node
    /// has permanently failed given its observed silence duration.
    pub const fn posterior_permanent_prob(&self) -> f64 {
        match *self {
            Self::Healthy { .. } | Self::RecoveryObserved { .. } | Self::Unknown => 0.0,
            Self::SuspectedUnavailable {
                posterior_permanent_prob,
                ..
            }
            | Self::ConfirmedUnavailableByPolicy {
                posterior_permanent_prob,
                ..
            } => posterior_permanent_prob,
        }
    }

    /// Returns the observed silence duration $t_{\text{now}} - t_{\text{first\_missed}}$ in seconds.
    pub fn silence_duration_secs(&self, now_secs: f64) -> f64 {
        match *self {
            Self::SuspectedUnavailable {
                first_missed_secs, ..
            }
            | Self::ConfirmedUnavailableByPolicy {
                first_missed_secs, ..
            } => (now_secs - first_missed_secs).max(0.0),
            _ => 0.0,
        }
    }
}

/// Observable telemetry record for a single storage node.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NodeObservation {
    /// Node identifier.
    pub node_id: NodeId,
    /// Failure domain identifier.
    pub domain_id: FailureDomainId,
    /// Observable state (contains zero ground-truth `PhysicalNodeState` information).
    pub state: ObservedNodeState,
}

/// Correlated failure-domain telemetry signal inferred strictly from simultaneous missed heartbeats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DomainCorrelationSignal {
    /// Domain has normal heartbeat response levels.
    Normal {
        domain_id: FailureDomainId,
        affected_fraction: f64,
    },
    /// At least 50% of nodes in `domain_id` are simultaneously unreachable.
    CorrelatedDomainEvent {
        domain_id: FailureDomainId,
        affected_fraction: f64,
    },
}

/// Read-only view of observable cluster state exposed to blind repair policies.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterObservationView {
    /// Timestamp of this observation snapshot.
    pub now_secs: f64,
    /// Per-node observable telemetry.
    pub node_observations: Vec<NodeObservation>,
    /// Per-domain correlation telemetry signals.
    pub domain_signals: Vec<DomainCorrelationSignal>,
}

/// Configuration for the Phase 2 blind failure detector.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FailureDetectionConfig {
    /// Interval between periodic heartbeats in seconds (default: 300s = 5 min).
    pub heartbeat_interval_secs: f64,
    /// Timeout before a heartbeat is considered missed in seconds (default: 30s).
    pub heartbeat_timeout_secs: f64,
    /// Number of consecutive missed heartbeats before triggering active probes / confirmation (default: 3).
    pub missed_heartbeat_threshold: u32,
    /// Number of active challenge-response probes issued when `missed_heartbeat_threshold` is reached (default: 2).
    pub challenge_probes_on_miss: u32,
    /// Base round-trip latency of a challenge-response probe in seconds (default: 2.0s).
    pub probe_latency_secs: f64,
    /// Maximum uniform network jitter added to heartbeat/probe latency in seconds (default: 0.5s).
    pub network_jitter_secs: f64,
    /// Independent packet-loss probability per heartbeat or probe message in $[0.0, 1.0)$ (default: 0.01).
    pub packet_loss_probability: f64,
    /// Physical delay in seconds before a permanently failed node is replaced by an empty host (default: 7200s = 2h).
    pub node_replacement_delay_secs: f64,
    /// Prior mean transient outage duration in seconds used for Bayesian posterior calculation.
    pub assumed_mean_outage_secs: f64,
    /// Prior per-epoch permanent failure probability used for Bayesian posterior calculation.
    pub assumed_perm_prob_per_epoch: f64,
    /// Prior per-epoch transient churn probability used for Bayesian posterior calculation.
    pub assumed_churn_prob_per_epoch: f64,
}

impl Default for FailureDetectionConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval_secs: 300.0,
            heartbeat_timeout_secs: 30.0,
            missed_heartbeat_threshold: 3,
            challenge_probes_on_miss: 2,
            probe_latency_secs: 2.0,
            network_jitter_secs: 0.5,
            packet_loss_probability: 0.01,
            node_replacement_delay_secs: 7_200.0,
            assumed_mean_outage_secs: 14_400.0,
            assumed_perm_prob_per_epoch: 0.015,
            assumed_churn_prob_per_epoch: 0.10,
        }
    }
}

/// Stateful blind failure detector tracking heartbeats, active probes, and detection telemetry.
#[derive(Debug, Clone)]
pub struct FailureDetector {
    config: FailureDetectionConfig,
    num_domains: usize,
    rng: DeterministicRng,
    observations: Vec<NodeObservation>,
    /// Tracks whether a true permanent failure on node `i` has already had its detection delay recorded.
    recorded_perm_fail_timestamp: Vec<Option<f64>>,
    /// Cumulative sum of detection delays ($t_{\text{confirmed}} - t_{\text{failed}}$) for true permanent failures.
    total_perm_detection_delay_secs: f64,
    /// Count of true permanent failures detected.
    detected_perm_failures_count: u64,
    /// Count of false-positive confirmations (confirming a node that was physically Online or TemporarilyOffline).
    false_positive_confirmations: u64,
}

impl FailureDetector {
    /// Creates a new blind failure detector for `num_nodes` across `num_domains`.
    pub fn new(
        config: FailureDetectionConfig,
        num_nodes: usize,
        num_domains: usize,
        seed: u64,
    ) -> Self {
        let observations = (0..num_nodes)
            .map(|i| NodeObservation {
                node_id: NodeId(i),
                domain_id: FailureDomainId(i % num_domains.max(1)),
                state: ObservedNodeState::Healthy {
                    last_response_secs: 0.0,
                },
            })
            .collect();

        Self {
            config,
            num_domains: num_domains.max(1),
            rng: DeterministicRng::new(seed ^ 0xDE7E_C700_F592_0001),
            observations,
            recorded_perm_fail_timestamp: vec![None; num_nodes],
            total_perm_detection_delay_secs: 0.0,
            detected_perm_failures_count: 0,
            false_positive_confirmations: 0,
        }
    }

    /// Returns the configuration.
    pub const fn config(&self) -> &FailureDetectionConfig {
        &self.config
    }

    /// Returns the mean detection delay in seconds across all detected permanent failures.
    pub fn mean_permanent_detection_delay_secs(&self) -> f64 {
        if self.detected_perm_failures_count == 0 {
            0.0
        } else {
            self.total_perm_detection_delay_secs / (self.detected_perm_failures_count as f64)
        }
    }

    /// Returns the total number of false-positive policy confirmations (transient or packet-loss).
    pub const fn false_positive_confirmations(&self) -> u64 {
        self.false_positive_confirmations
    }

    /// Computes the Bayesian posterior probability $\hat{p}_{\text{perm}}(\Delta t_{\text{silent}}) \in [0, 1]$
    /// that an outage of silence duration `silence_secs` is permanent rather than transient.
    pub fn compute_posterior_permanent_prob(&self, silence_secs: f64) -> f64 {
        let p_perm = self.config.assumed_perm_prob_per_epoch.clamp(1e-6, 0.999);
        let p_churn = self.config.assumed_churn_prob_per_epoch.clamp(1e-6, 0.999);
        let pi_0 = p_perm / (p_perm + p_churn);

        let mean_outage = self.config.assumed_mean_outage_secs.max(1.0);
        let tail_transient = (-silence_secs.max(0.0) / mean_outage).exp();
        let denom = pi_0 + (1.0 - pi_0) * tail_transient;
        if denom <= 0.0 {
            pi_0
        } else {
            (pi_0 / denom).clamp(0.0, 1.0)
        }
    }

    /// Advances the failure detector to `now_secs` by executing a heartbeat/probe round against
    /// `physical` and returning the sanitized `ClusterObservationView` (free of ground truth).
    pub fn step_to(
        &mut self,
        now_secs: f64,
        physical: &PhysicalClusterSnapshot,
    ) -> ClusterObservationView {
        let p_loss = self.config.packet_loss_probability.clamp(0.0, 0.999);

        for &(node_id, domain_id, phys_state) in &physical.node_states {
            let idx = node_id.0;
            if idx >= self.observations.len() {
                continue;
            }
            self.observations[idx].domain_id = domain_id;

            // Track ground-truth permanent failure start time solely for observability metric accounting
            // (never exposed in NodeObservation).
            match phys_state {
                PhysicalNodeState::PermanentlyFailed { failed_at_secs }
                | PhysicalNodeState::Replacing { failed_at_secs, .. } => {
                    if self.recorded_perm_fail_timestamp[idx].is_none() {
                        self.recorded_perm_fail_timestamp[idx] = Some(failed_at_secs);
                    }
                }
                PhysicalNodeState::Online => {
                    self.recorded_perm_fail_timestamp[idx] = None;
                }
                _ => {}
            }

            let physically_reachable = phys_state.is_network_reachable();
            // Roll whether the heartbeat succeeds over the network
            let hb_dropped = self.rng.next_f64() < p_loss;
            let hb_succeeded = physically_reachable && !hb_dropped;

            if hb_succeeded {
                let prev_state = self.observations[idx].state;
                self.observations[idx].state = match prev_state {
                    ObservedNodeState::SuspectedUnavailable {
                        first_missed_secs, ..
                    }
                    | ObservedNodeState::ConfirmedUnavailableByPolicy {
                        first_missed_secs, ..
                    } => ObservedNodeState::RecoveryObserved {
                        recovered_at_secs: now_secs,
                        previous_outage_duration_secs: (now_secs - first_missed_secs).max(0.0),
                    },
                    _ => ObservedNodeState::Healthy {
                        last_response_secs: now_secs,
                    },
                };
            } else {
                // Heartbeat missed!
                let (first_missed_secs, prev_misses, prev_probes, was_confirmed_at) =
                    match self.observations[idx].state {
                        ObservedNodeState::SuspectedUnavailable {
                            first_missed_secs,
                            consecutive_misses,
                            failed_probes,
                            ..
                        } => (first_missed_secs, consecutive_misses, failed_probes, None),
                        ObservedNodeState::ConfirmedUnavailableByPolicy {
                            first_missed_secs,
                            confirmed_at_secs,
                            consecutive_misses,
                            failed_probes,
                            ..
                        } => (
                            first_missed_secs,
                            consecutive_misses,
                            failed_probes,
                            Some(confirmed_at_secs),
                        ),
                        _ => (now_secs, 0, 0, None),
                    };

                let new_misses = prev_misses.saturating_add(1);
                let mut new_failed_probes = prev_probes;
                let mut rescued_by_probe = false;

                // If consecutive misses reached threshold, issue active challenge-response probes:
                if new_misses >= self.config.missed_heartbeat_threshold
                    && was_confirmed_at.is_none()
                {
                    for _ in 0..self.config.challenge_probes_on_miss {
                        let probe_dropped = self.rng.next_f64() < p_loss;
                        if physically_reachable && !probe_dropped {
                            rescued_by_probe = true;
                            break;
                        }
                        new_failed_probes = new_failed_probes.saturating_add(1);
                    }
                }

                if rescued_by_probe {
                    self.observations[idx].state = ObservedNodeState::Healthy {
                        last_response_secs: now_secs,
                    };
                } else {
                    let silence_secs = (now_secs - first_missed_secs)
                        + self.config.heartbeat_interval_secs.min(now_secs);
                    let posterior = self.compute_posterior_permanent_prob(silence_secs);

                    if new_misses >= self.config.missed_heartbeat_threshold
                        && new_failed_probes >= self.config.challenge_probes_on_miss
                    {
                        let confirmed_at = was_confirmed_at.unwrap_or_else(|| {
                            // Record detection delay if this is a true permanent failure, or false positive otherwise
                            if let Some(fail_t) = self.recorded_perm_fail_timestamp[idx] {
                                let probe_overhead = (self.config.challenge_probes_on_miss as f64)
                                    * self.config.probe_latency_secs;
                                let delay = (now_secs + probe_overhead - fail_t).max(0.0);
                                self.total_perm_detection_secs_add(delay);
                            } else {
                                self.false_positive_confirmations += 1;
                            }
                            now_secs
                        });
                        self.observations[idx].state =
                            ObservedNodeState::ConfirmedUnavailableByPolicy {
                                first_missed_secs,
                                confirmed_at_secs: confirmed_at,
                                consecutive_misses: new_misses,
                                failed_probes: new_failed_probes,
                                posterior_permanent_prob: posterior,
                            };
                    } else {
                        self.observations[idx].state = ObservedNodeState::SuspectedUnavailable {
                            first_missed_secs,
                            consecutive_misses: new_misses,
                            failed_probes: new_failed_probes,
                            posterior_permanent_prob: posterior,
                        };
                    }
                }
            }
        }

        // Compute domain correlation signals
        let mut domain_totals = vec![0usize; self.num_domains];
        let mut domain_down = vec![0usize; self.num_domains];
        for obs in &self.observations {
            let d = obs.domain_id.0.min(self.num_domains - 1);
            domain_totals[d] += 1;
            if !obs.state.is_observed_reachable() {
                domain_down[d] += 1;
            }
        }

        let domain_signals = (0..self.num_domains)
            .map(|d| {
                let tot = domain_totals[d].max(1) as f64;
                let frac = (domain_down[d] as f64) / tot;
                if frac >= 0.50 && domain_down[d] >= 2 {
                    DomainCorrelationSignal::CorrelatedDomainEvent {
                        domain_id: FailureDomainId(d),
                        affected_fraction: frac,
                    }
                } else {
                    DomainCorrelationSignal::Normal {
                        domain_id: FailureDomainId(d),
                        affected_fraction: frac,
                    }
                }
            })
            .collect();

        ClusterObservationView {
            now_secs,
            node_observations: self.observations.clone(),
            domain_signals,
        }
    }

    fn total_perm_detection_secs_add(&mut self, delay: f64) {
        self.total_perm_detection_delay_secs += delay;
        self.detected_perm_failures_count += 1;
    }
}

/// Helper to convert per-epoch hazard probability to continuous per-second rate $\lambda$.
pub fn epoch_prob_to_rate_per_sec(p_epoch: f64) -> f64 {
    let clamped = p_epoch.clamp(0.0, 1.0 - 1e-12);
    if clamped <= 0.0 {
        0.0
    } else {
        -clamped.ln_1p_neg() / SECONDS_PER_EPOCH
    }
}

trait Ln1pNeg {
    fn ln_1p_neg(self) -> f64;
}

impl Ln1pNeg for f64 {
    fn ln_1p_neg(self) -> f64 {
        (1.0 - self).ln()
    }
}
