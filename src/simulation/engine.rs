//! Deterministic Discrete-Event Simulation (DES) engine for FSP Phase 0 & Phase 1.

use std::collections::BinaryHeap;

use crate::config::{ConfigError, ErasureCodeConfig, SimulationConfig};
use crate::model::node::NodeId;
use crate::model::ClusterState;
use crate::reliability::recoverability::{evaluate_recoverability, SystemDataState};
use crate::reliability::survival::{
    analytical_correlated_survival_probability, analytical_markov_mttdl_secs,
    estimate_empirical_mttdl, SurvivalCurvePoint,
};
use crate::simulation::events::{EventKind, ScheduledEvent};
use crate::simulation::failure_generator::{
    generate_exogenous_failure_trace, ExogenousFailureTrace,
};
use crate::simulation::metrics::{SimulationMetrics, SimulationReport};
use crate::strategies::{RepairDecision, RepairStrategy, StrategyContext};

/// Self-contained, deterministic SplitMix64 pseudo-random number generator.
/// Ensures zero global state and bit-exact cross-platform reproducibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    /// Initializes the PRNG from a 64-bit seed.
    pub const fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    /// Generates the next uniform `u64`.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Generates a uniform `f64` in $[0.0, 1.0)$.
    pub fn next_f64(&mut self) -> f64 {
        let bits = self.next_u64() >> 11;
        (bits as f64) * (1.0 / ((1u64 << 53) as f64))
    }

    /// Samples from an exponential distribution with the given mean ($\mu > 0$).
    pub fn next_exponential(&mut self, mean: f64) -> f64 {
        let u = self.next_f64().clamp(1e-15, 1.0 - 1e-15);
        -mean * (1.0 - u).ln()
    }
}

/// Core discrete-event simulation engine.
pub struct SimulationEngine {
    config: SimulationConfig,
    cluster: ClusterState,
    strategy: Box<dyn RepairStrategy>,
    event_queue: BinaryHeap<ScheduledEvent>,
    next_sequence: u64,
    now_secs: f64,
    /// Earliest timestamp at which the cluster's repair bandwidth channel is free.
    repair_channel_free_at_secs: f64,
    /// Cumulative time (seconds) the object was in `AvailableAndRecoverable` state.
    available_time_secs: f64,
    /// Cumulative node-seconds spent offline across all cluster nodes.
    cumulative_offline_node_secs: f64,
    /// Count of offline nodes at the end of the previous event.
    last_offline_node_count: usize,
    /// Whether the object was available at the end of the previous event.
    was_available: bool,
    /// Timestamp when irrecoverable data loss first occurred, if any.
    data_loss_time_secs: Option<f64>,
    /// Total repair network bytes transferred.
    repair_bandwidth_bytes: u64,
    /// Completed shard repair count.
    repair_operation_count: u64,
    /// Aborted shard repair count.
    aborted_repair_count: u64,
    /// Sum of repair latencies across completed repairs.
    total_repair_latency_secs: f64,
    /// Peak observed failure-domain exposure.
    peak_failure_domain_exposure: f64,
    /// Recorded survival curve checkpoints.
    survival_curve: Vec<SurvivalCurvePoint>,
    /// Total processed events.
    events_processed: u64,
    /// Deterministic FNV-1a 64-bit trace digest.
    trace_digest: u64,
}

impl SimulationEngine {
    /// Creates a new simulation engine, validates configuration, performs initial shard
    /// placement across failure domains, and pre-generates the seeded exogenous failure trace.
    pub fn new(
        config: SimulationConfig,
        strategy: Box<dyn RepairStrategy>,
    ) -> Result<Self, ConfigError> {
        let trace = generate_exogenous_failure_trace(&config, 0);
        Self::new_with_exogenous_trace(config, strategy, trace)
    }

    /// Creates a simulation engine injected with a pre-generated `ExogenousFailureTrace`,
    /// guaranteeing that multiple strategies can be evaluated on the exact same failure sequence.
    pub fn new_with_exogenous_trace(
        mut config: SimulationConfig,
        strategy: Box<dyn RepairStrategy>,
        trace: ExogenousFailureTrace,
    ) -> Result<Self, ConfigError> {
        config.validate()?;
        config.seed = trace.sample_seed;

        let shard_size = config.shard_size_bytes();
        let cluster = ClusterState::new_with_initial_placement(
            config.num_nodes,
            config.num_failure_domains,
            config.max_shards_per_domain,
            config.erasure_code.total_shards,
            shard_size,
        );

        let initial_rec = evaluate_recoverability(&cluster, config.erasure_code);

        let mut engine = Self {
            config,
            cluster,
            strategy,
            event_queue: BinaryHeap::new(),
            next_sequence: 0,
            now_secs: 0.0,
            repair_channel_free_at_secs: 0.0,
            available_time_secs: 0.0,
            cumulative_offline_node_secs: 0.0,
            last_offline_node_count: 0,
            was_available: initial_rec.is_currently_available(),
            data_loss_time_secs: None,
            repair_bandwidth_bytes: 0,
            repair_operation_count: 0,
            aborted_repair_count: 0,
            total_repair_latency_secs: 0.0,
            peak_failure_domain_exposure: initial_rec.failure_domain_exposure,
            survival_curve: vec![SurvivalCurvePoint {
                time_secs: 0.0,
                empirical_survival: 1.0,
                analytical_no_repair_survival: 1.0,
            }],
            events_processed: 0,
            trace_digest: 0xCBF2_9CE4_8422_2325,
        };

        engine.populate_schedule_from_trace(trace);
        Ok(engine)
    }

    /// Returns a reference to the current cluster state.
    pub fn cluster(&self) -> &ClusterState {
        &self.cluster
    }

    /// Returns the configured erasure-coding scheme.
    pub fn erasure_code(&self) -> ErasureCodeConfig {
        self.config.erasure_code
    }

    /// Returns the list of `NodeId`s currently hosting durable shards.
    pub fn hosted_shard_nodes(&self) -> Vec<NodeId> {
        self.cluster.hosted_shard_nodes()
    }

    /// Injects a custom event into the priority queue (assigning the next deterministic sequence number).
    pub fn inject_event(&mut self, mut event: ScheduledEvent) {
        event.sequence = self.next_sequence;
        self.next_sequence += 1;
        self.event_queue.push(event);
    }

    /// Schedules periodic controller ticks and inserts all events from `trace`.
    fn populate_schedule_from_trace(&mut self, trace: ExogenousFailureTrace) {
        let horizon_secs = self.config.horizon_secs();
        let check_interval = self.config.controller_check_interval_secs.max(60.0);

        let mut t = check_interval;
        while t <= horizon_secs + 1e-9 {
            self.inject_event(ScheduledEvent {
                time_secs: t,
                sequence: 0,
                kind: EventKind::ControllerCheck,
            });
            t += check_interval;
        }

        for ev in trace.events {
            self.inject_event(ev);
        }
    }

    /// Steps the simulation forward until `target_time_secs`, processing all events with
    /// `time_secs <= target_time_secs`.
    pub fn step_until(&mut self, target_time_secs: f64) -> Result<(), ConfigError> {
        let clamped_target = target_time_secs.min(self.config.horizon_secs());

        while let Some(next_ev) = self.event_queue.peek() {
            if next_ev.time_secs > clamped_target + 1e-9 {
                break;
            }
            let ev = self.event_queue.pop().expect("peeked some");
            self.process_event(ev);
        }

        if clamped_target > self.now_secs {
            self.advance_clock_to(clamped_target);
        }

        Ok(())
    }

    /// Runs the simulation to completion at `config.horizon_secs()` and returns the full report.
    pub fn run(&mut self) -> Result<SimulationReport, ConfigError> {
        let horizon = self.config.horizon_secs();
        self.step_until(horizon)?;

        let final_rec = evaluate_recoverability(&self.cluster, self.config.erasure_code);
        let irrecoverable_data_loss = self.data_loss_time_secs.is_some()
            || final_rec.state == SystemDataState::IrrecoverablyLost;

        let availability_empirical = if horizon > 0.0 {
            (self.available_time_secs / horizon).clamp(0.0, 1.0)
        } else {
            1.0
        };

        let total_node_horizon_secs = (self.config.num_nodes as f64) * horizon;
        let measured_node_offline_fraction = if total_node_horizon_secs > 0.0 {
            (self.cumulative_offline_node_secs / total_node_horizon_secs).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let data_loss_probability_empirical = if irrecoverable_data_loss { 1.0 } else { 0.0 };
        let empirical_mttdl = estimate_empirical_mttdl(&[self.data_loss_time_secs], horizon);

        let single_shard_repair_bytes = ((self.config.erasure_code.recovery_threshold as u64) + 1)
            * self.config.shard_size_bytes();
        let mean_single_shard_repair_secs = self.config.repair_base_latency_secs
            + (single_shard_repair_bytes as f64)
                / (self.config.repair_bandwidth_bytes_per_sec as f64);

        let analytical_markov_mttdl = analytical_markov_mttdl_secs(
            self.config.erasure_code.total_shards,
            self.config.erasure_code.recovery_threshold,
            self.config.permanent_failure_rate_per_epoch,
            mean_single_shard_repair_secs,
        );

        let analytical_horizon_survival_no_repair = analytical_correlated_survival_probability(
            self.config.erasure_code.total_shards,
            self.config.erasure_code.recovery_threshold,
            self.config.num_failure_domains,
            self.config.max_shards_per_domain,
            self.config.permanent_failure_rate_per_epoch,
            self.config.correlated_domain_failure_rate_per_epoch,
            horizon,
        );

        let mean_repair_latency_secs = if self.repair_operation_count > 0 {
            self.total_repair_latency_secs / (self.repair_operation_count as f64)
        } else {
            0.0
        };

        let mean_repair_bandwidth_bytes_per_sec = if horizon > 0.0 {
            (self.repair_bandwidth_bytes as f64) / horizon
        } else {
            0.0
        };

        Ok(SimulationReport {
            strategy_name: self.strategy.name().to_string(),
            churn_scenario_label: self.config.churn_scenario.label(),
            seed: self.config.seed,
            horizon_secs: horizon,
            events_processed: self.events_processed,
            event_trace_digest: self.trace_digest,
            metrics: SimulationMetrics {
                irrecoverable_data_loss,
                data_loss_time_secs: self.data_loss_time_secs,
                data_loss_probability_empirical,
                availability_empirical,
                measured_node_offline_fraction,
                empirical_mttdl,
                analytical_markov_mttdl_secs: analytical_markov_mttdl,
                analytical_horizon_survival_no_repair,
                repair_bandwidth_bytes: self.repair_bandwidth_bytes,
                mean_repair_bandwidth_bytes_per_sec,
                repair_operation_count: self.repair_operation_count,
                aborted_repair_count: self.aborted_repair_count,
                mean_repair_latency_secs,
                nominal_storage_overhead: self.config.erasure_code.nominal_storage_overhead(),
                failure_domain_exposure: final_rec.failure_domain_exposure,
                peak_failure_domain_exposure: self.peak_failure_domain_exposure,
                survival_curve: self.survival_curve.clone(),
            },
        })
    }

    fn advance_clock_to(&mut self, new_time_secs: f64) {
        let dt = (new_time_secs - self.now_secs).max(0.0);
        if self.was_available {
            self.available_time_secs += dt;
        }
        self.cumulative_offline_node_secs += (self.last_offline_node_count as f64) * dt;
        self.now_secs = new_time_secs;
    }

    fn hash_event(&mut self, ev: &ScheduledEvent) {
        let mut mix = |val: u64| {
            self.trace_digest ^= val;
            self.trace_digest = self.trace_digest.wrapping_mul(0x0000_0100_0000_01B3);
        };
        mix(ev.time_secs.to_bits());
        mix(ev.sequence);
        match &ev.kind {
            EventKind::NodeOutage {
                node_id,
                duration_secs,
            } => {
                mix(1);
                mix(node_id.0 as u64);
                mix(duration_secs.to_bits());
            }
            EventKind::NodeRecovery { node_id } => {
                mix(2);
                mix(node_id.0 as u64);
            }
            EventKind::NodePermanentFailure { node_id } => {
                mix(3);
                mix(node_id.0 as u64);
            }
            EventKind::ShardPermanentLoss { shard_id } => {
                mix(4);
                mix(shard_id.0 as u64);
            }
            EventKind::DomainOutage {
                domain_id,
                duration_secs,
            } => {
                mix(5);
                mix(domain_id.0 as u64);
                mix(duration_secs.to_bits());
            }
            EventKind::DomainPermanentFailure { domain_id } => {
                mix(6);
                mix(domain_id.0 as u64);
            }
            EventKind::BurstNodeFailure {
                node_ids,
                is_permanent,
                outage_duration_secs,
            } => {
                mix(9);
                mix(u64::from(*is_permanent));
                mix(outage_duration_secs.to_bits());
                for n in node_ids {
                    mix(n.0 as u64);
                }
            }
            EventKind::RepairComplete {
                shard_id,
                bytes_transferred,
            } => {
                mix(7);
                mix(shard_id.0 as u64);
                mix(*bytes_transferred);
            }
            EventKind::ControllerCheck => {
                mix(8);
            }
        }
    }

    fn process_event(&mut self, ev: ScheduledEvent) {
        self.advance_clock_to(ev.time_secs);
        self.events_processed += 1;
        self.hash_event(&ev);

        let mut should_evaluate_strategy = false;

        match ev.kind {
            EventKind::NodeOutage {
                node_id,
                duration_secs,
            } => {
                let recover_at = self.now_secs + duration_secs.max(0.0);
                if self
                    .cluster
                    .set_node_temporarily_offline(node_id, self.now_secs, recover_at)
                {
                    self.inject_event(ScheduledEvent {
                        time_secs: recover_at,
                        sequence: 0,
                        kind: EventKind::NodeRecovery { node_id },
                    });
                }
            }
            EventKind::NodeRecovery { node_id } => {
                self.cluster.recover_node(node_id, self.now_secs);
                should_evaluate_strategy = true;
            }
            EventKind::NodePermanentFailure { node_id } => {
                if self
                    .cluster
                    .fail_node_permanently(node_id, self.now_secs, true)
                    .is_some()
                {
                    // In Phase 2 blind detection mode, a permanent failure is NOT immediately
                    // notified to the repair controller at t + 0s; the controller only discovers
                    // it on subsequent ControllerCheck / heartbeat timeout ticks!
                    if self.config.blind_detection.is_none() {
                        should_evaluate_strategy = true;
                    }
                }
            }
            EventKind::ShardPermanentLoss { shard_id } => {
                if self.cluster.fail_shard_permanently(shard_id, self.now_secs) {
                    should_evaluate_strategy = true;
                }
            }
            EventKind::DomainOutage {
                domain_id,
                duration_secs,
            } => {
                let recover_at = self.now_secs + duration_secs.max(0.0);
                let domain_nodes: Vec<NodeId> = self
                    .cluster
                    .nodes()
                    .iter()
                    .filter(|n| n.domain_id == domain_id && n.is_online())
                    .map(|n| n.id)
                    .collect();
                for node_id in domain_nodes {
                    if self
                        .cluster
                        .set_node_temporarily_offline(node_id, self.now_secs, recover_at)
                    {
                        self.inject_event(ScheduledEvent {
                            time_secs: recover_at,
                            sequence: 0,
                            kind: EventKind::NodeRecovery { node_id },
                        });
                    }
                }
            }
            EventKind::DomainPermanentFailure { domain_id } => {
                let domain_nodes: Vec<NodeId> = self
                    .cluster
                    .nodes()
                    .iter()
                    .filter(|n| n.domain_id == domain_id && !n.is_permanently_failed())
                    .map(|n| n.id)
                    .collect();
                for node_id in domain_nodes {
                    self.cluster
                        .fail_node_permanently(node_id, self.now_secs, false);
                }
                should_evaluate_strategy = true;
            }
            EventKind::BurstNodeFailure {
                node_ids,
                is_permanent,
                outage_duration_secs,
            } => {
                if is_permanent {
                    for node_id in node_ids {
                        self.cluster
                            .fail_node_permanently(node_id, self.now_secs, true);
                    }
                    should_evaluate_strategy = true;
                } else {
                    let recover_at = self.now_secs + outage_duration_secs.max(0.0);
                    for node_id in node_ids {
                        if self.cluster.set_node_temporarily_offline(
                            node_id,
                            self.now_secs,
                            recover_at,
                        ) {
                            self.inject_event(ScheduledEvent {
                                time_secs: recover_at,
                                sequence: 0,
                                kind: EventKind::NodeRecovery { node_id },
                            });
                        }
                    }
                }
            }
            EventKind::RepairComplete {
                shard_id,
                bytes_transferred,
            } => {
                let rec = evaluate_recoverability(&self.cluster, self.config.erasure_code);
                if self.data_loss_time_secs.is_none() && rec.is_currently_available() {
                    if let Some(latency) = self.cluster.complete_shard_repair(
                        shard_id,
                        self.config.shard_size_bytes(),
                        self.now_secs,
                    ) {
                        self.repair_bandwidth_bytes += bytes_transferred;
                        self.repair_operation_count += 1;
                        self.total_repair_latency_secs += latency;
                        should_evaluate_strategy = true;
                    } else {
                        self.aborted_repair_count += 1;
                    }
                } else {
                    self.cluster.clear_repair_in_flight(shard_id);
                    self.aborted_repair_count += 1;
                }
            }
            EventKind::ControllerCheck => {
                should_evaluate_strategy = true;
                let emp_surv = if self.data_loss_time_secs.is_some() {
                    0.0
                } else {
                    1.0
                };
                let ana_surv = analytical_correlated_survival_probability(
                    self.config.erasure_code.total_shards,
                    self.config.erasure_code.recovery_threshold,
                    self.config.num_failure_domains,
                    self.config.max_shards_per_domain,
                    self.config.permanent_failure_rate_per_epoch,
                    self.config.correlated_domain_failure_rate_per_epoch,
                    self.now_secs,
                );
                self.survival_curve.push(SurvivalCurvePoint {
                    time_secs: self.now_secs,
                    empirical_survival: emp_surv,
                    analytical_no_repair_survival: ana_surv,
                });
            }
        }

        // Update offline node count for exact time-weighted node offline fraction integration.
        self.last_offline_node_count = self.cluster.count_offline_nodes();

        // Re-evaluate recoverability after state mutation.
        let mut rec_after = evaluate_recoverability(&self.cluster, self.config.erasure_code);
        if let Some(lrc) = self.config.lrc_config {
            let durable_ids: std::collections::BTreeSet<_> = self
                .cluster
                .durable_shard_replicas()
                .into_iter()
                .map(|r| r.shard_id)
                .collect();
            let online_ids: std::collections::BTreeSet<_> = self
                .cluster
                .available_shard_replicas()
                .into_iter()
                .map(|r| r.shard_id)
                .collect();
            if !lrc.is_pattern_decodable(&durable_ids) {
                rec_after.state = SystemDataState::IrrecoverablyLost;
            } else if !lrc.is_pattern_decodable(&online_ids) {
                rec_after.state = SystemDataState::TemporarilyUnavailableButDurable;
            }
        }
        if rec_after.failure_domain_exposure > self.peak_failure_domain_exposure {
            self.peak_failure_domain_exposure = rec_after.failure_domain_exposure;
        }

        // If durable shards dropped below k, enter absorbing IrrecoverablyLost state.
        if rec_after.state == SystemDataState::IrrecoverablyLost
            && self.data_loss_time_secs.is_none()
        {
            self.data_loss_time_secs = Some(self.now_secs);
        }

        // Update current availability flag for piecewise integration.
        self.was_available =
            self.data_loss_time_secs.is_none() && rec_after.is_currently_available();

        // Invoke repair strategy if eligible and data is not irrecoverably lost.
        if should_evaluate_strategy && self.data_loss_time_secs.is_none() {
            let shard_bytes = self.config.shard_size_bytes();
            let k = self.config.erasure_code.recovery_threshold as u64;
            let bytes_per_shard_repair = (k + 1) * shard_bytes;
            let bps = self.config.repair_bandwidth_bytes_per_sec.max(1) as f64;
            let single_repair_secs =
                self.config.repair_base_latency_secs + (bytes_per_shard_repair as f64) / bps;

            let queue_wait_secs = (self.repair_channel_free_at_secs - self.now_secs).max(0.0);
            let queued_repair_bytes = (queue_wait_secs * bps).round().max(0.0) as u64;
            let missing_count = self
                .cluster
                .missing_unscheduled_shards(self.config.erasure_code.total_shards)
                .len()
                .max(1);
            let expected_repair_duration_secs =
                queue_wait_secs + (missing_count as f64) * single_repair_secs;

            let bandwidth_budget_bytes_per_interval =
                ((self.config.repair_bandwidth_bytes_per_sec as f64)
                    * self.config.controller_check_interval_secs)
                    .round()
                    .max(bytes_per_shard_repair as f64) as u64;

            let u_eff = (rec_after.durable_distinct_shards + self.cluster.in_flight_repair_count())
                .min(self.config.erasure_code.total_shards);

            let step_surv = analytical_correlated_survival_probability(
                u_eff,
                self.config.erasure_code.recovery_threshold,
                self.config.num_failure_domains,
                self.config.max_shards_per_domain,
                self.config.permanent_failure_rate_per_epoch,
                self.config.correlated_domain_failure_rate_per_epoch,
                self.config.controller_check_interval_secs,
            );

            let vuln_window =
                expected_repair_duration_secs + self.config.controller_check_interval_secs;
            let window_surv = analytical_correlated_survival_probability(
                u_eff,
                self.config.erasure_code.recovery_threshold,
                self.config.num_failure_domains,
                self.config.max_shards_per_domain,
                self.config.permanent_failure_rate_per_epoch,
                self.config.correlated_domain_failure_rate_per_epoch,
                vuln_window,
            );
            let window_loss_risk = (1.0 - window_surv).clamp(0.0, 1.0);

            let ctx = StrategyContext {
                now_secs: self.now_secs,
                cluster: &self.cluster,
                erasure_code: self.config.erasure_code,
                recoverability: &rec_after,
                shard_size_bytes: shard_bytes,
                unreachable_suspect_timeout_secs: self.config.unreachable_suspect_timeout_secs,
                analytical_step_survival_probability: step_surv,
                window_loss_risk,
                queued_repair_bytes,
                bandwidth_budget_bytes_per_interval,
                expected_repair_duration_secs,
            };

            if let RepairDecision::RepairShards(shards_to_repair) = self.strategy.evaluate(&ctx) {
                self.schedule_repairs(shards_to_repair);
            }
        }
    }

    fn schedule_repairs(&mut self, shards: Vec<crate::model::shard::ShardId>) {
        let shard_bytes = self.config.shard_size_bytes();
        let k = self.config.erasure_code.recovery_threshold as u64;
        let bps = self.config.repair_bandwidth_bytes_per_sec as f64;
        let online_ids: std::collections::BTreeSet<_> = self
            .cluster
            .available_shard_replicas()
            .into_iter()
            .map(|r| r.shard_id)
            .collect();

        for shard_id in shards {
            if shard_id.0 >= self.config.erasure_code.total_shards
                || self.cluster.is_repair_in_flight(shard_id)
            {
                continue;
            }
            let bytes_per_shard_repair = if let Some(lrc) = self.config.lrc_config {
                match lrc.plan_shard_repair(shard_id, &online_ids) {
                    Some(method) => method.total_transfer_bytes(shard_bytes),
                    None => continue,
                }
            } else {
                (k + 1) * shard_bytes
            };

            let start_time = self.now_secs.max(self.repair_channel_free_at_secs);
            let transfer_duration =
                self.config.repair_base_latency_secs + (bytes_per_shard_repair as f64) / bps;
            let finish_time = start_time + transfer_duration;
            // Do not schedule repair completions beyond the simulation horizon.
            if finish_time > self.config.horizon_secs() {
                continue;
            }
            self.cluster.mark_repair_in_flight(shard_id, self.now_secs);
            self.repair_channel_free_at_secs = finish_time;

            self.inject_event(ScheduledEvent {
                time_secs: finish_time,
                sequence: 0,
                kind: EventKind::RepairComplete {
                    shard_id,
                    bytes_transferred: bytes_per_shard_repair,
                },
            });
        }
    }
}
