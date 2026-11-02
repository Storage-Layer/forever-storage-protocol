//! Failure domains, repair bandwidth limits, shard identity invariants, correlated failures,
//! and metric invariants tests (Requirements #5, #6, #7, #9, #10).

use std::collections::BTreeSet;

use fsp_sim::config::{ChurnScenario, SimulationConfig};
use fsp_sim::model::failure_domain::FailureDomainId;
use fsp_sim::reliability::recoverability::{evaluate_recoverability, SystemDataState};
use fsp_sim::reliability::survival::{
    analytical_binomial_survival_probability, estimate_empirical_mttdl, EmpiricalMttdlResult,
};
use fsp_sim::simulation::engine::SimulationEngine;
use fsp_sim::simulation::events::{EventKind, ScheduledEvent};
use fsp_sim::strategies::fixed_repair::FixedRepairStrategy;

#[test]
fn test_shard_placement_respects_failure_domain_constraints() {
    // 50 nodes across 10 failure domains, RS(20,12), max 2 shards per domain.
    let config = SimulationConfig::default_rs_20_12()
        .with_failure_domains(10, 2)
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0);

    let engine =
        SimulationEngine::new(config, Box::new(FixedRepairStrategy::new(16))).expect("engine init");

    let domain_counts = engine.cluster().durable_shards_per_domain();
    assert_eq!(domain_counts.len(), 10);
    for (&_domain_id, &count) in &domain_counts {
        assert!(
            count <= 2,
            "Failure domain exceeded max_shards_per_domain=2: got {}",
            count
        );
    }
    let total_placed: usize = domain_counts.values().sum();
    assert_eq!(total_placed, 20);
}

#[test]
fn test_repair_operations_respect_bandwidth_and_latency_limits() {
    // Configure shard size = 10 MB (object size = 120 MB for k=12).
    // Bandwidth limit = 10 MB/s, base latency = 5.0s.
    // Reconstructing 1 shard in RS(20,12) transfers (12 read + 1 write) * 10 MB = 130 MB.
    // Expected transfer duration for 1 shard = 5.0 + 130 / 10 = 18.0 seconds.
    let shard_bytes: u64 = 10_000_000;
    let config = SimulationConfig::default_rs_20_12()
        .with_object_size_bytes(shard_bytes * 12)
        .with_repair_bandwidth_bytes_per_sec(10_000_000)
        .with_repair_base_latency_secs(5.0)
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    // Strategy repairs immediately when durable shards <= 19.
    let mut engine =
        SimulationEngine::new(config, Box::new(FixedRepairStrategy::new(19))).expect("engine init");

    let first_node = engine.hosted_shard_nodes()[0];
    engine.inject_event(ScheduledEvent {
        time_secs: 100.0,
        sequence: 0,
        kind: EventKind::NodePermanentFailure {
            node_id: first_node,
        },
    });

    // At t = 110.0s (before 100 + 18 = 118.0s), repair is still in flight!
    engine
        .step_until(110.0)
        .expect("step before repair completes");
    let mid_status = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(
        mid_status.durable_distinct_shards, 19,
        "Repair must not complete before bandwidth + latency delay elapses"
    );

    // At t = 120.0s (after 118.0s), repair has completed!
    engine
        .step_until(120.0)
        .expect("step after repair completes");
    let after_status = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(after_status.durable_distinct_shards, 20);

    let report = engine.run().expect("finish run");
    assert_eq!(report.metrics.repair_operation_count, 1);
    assert_eq!(report.metrics.repair_bandwidth_bytes, 13 * shard_bytes);
    assert!((report.metrics.mean_repair_latency_secs - 18.0).abs() < 1e-6);
}

#[test]
fn test_repair_events_never_produce_duplicate_shard_identities_or_invalid_state() {
    let config = SimulationConfig::default_rs_20_12()
        .with_failure_domains(10, 2)
        .with_churn_scenario(ChurnScenario::Percent10)
        .with_permanent_failure_rate_per_epoch(0.01)
        .with_horizon_epochs(10)
        .with_seed(999);

    let mut engine =
        SimulationEngine::new(config, Box::new(FixedRepairStrategy::new(18))).expect("engine init");

    let _report = engine.run().expect("run");

    // Inspect active shard assignments in the cluster:
    let active_replicas = engine.cluster().durable_shard_replicas();
    assert!(active_replicas.len() <= 20);

    let mut seen_logical_ids = BTreeSet::new();
    let mut seen_nodes = BTreeSet::new();
    for replica in &active_replicas {
        assert!(replica.shard_id.0 < 20, "Invalid shard index");
        assert!(
            seen_logical_ids.insert(replica.shard_id),
            "Duplicate logical shard identity detected: {:?}",
            replica.shard_id
        );
        assert!(
            seen_nodes.insert(replica.node_id),
            "Multiple shards of same object placed on single node: {:?}",
            replica.node_id
        );
    }

    // Domain constraints must also remain satisfied after repairs:
    for (&_dom, &count) in &engine.cluster().durable_shards_per_domain() {
        assert!(
            count <= 2,
            "Repair violated failure domain concentration cap"
        );
    }
}

#[test]
fn test_adaptive_strategy_interface_receives_accurate_risk_telemetry() {
    use fsp_sim::strategies::adaptive::{
        AdaptiveRepairPolicy, AdaptiveRiskTelemetry, FspAdaptiveStrategy,
    };
    use fsp_sim::strategies::RepairDecision;

    struct MockAdaptivePolicy {
        calls: usize,
    }

    impl AdaptiveRepairPolicy for MockAdaptivePolicy {
        fn policy_name(&self) -> &'static str {
            "StrategyC_MockAdaptivePolicy"
        }

        fn select_repairs(&mut self, telemetry: &AdaptiveRiskTelemetry) -> RepairDecision {
            self.calls += 1;
            assert_eq!(telemetry.total_shards, 20);
            assert_eq!(telemetry.recovery_threshold, 12);
            assert!((0.0..=1.0).contains(&telemetry.failure_domain_exposure));
            assert!((0.0..=1.0).contains(&telemetry.estimated_survival_probability));
            if !telemetry.missing_shards.is_empty() {
                RepairDecision::RepairShards(telemetry.missing_shards.clone())
            } else {
                RepairDecision::NoOp
            }
        }
    }

    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Percent5)
        .with_permanent_failure_rate_per_epoch(0.01)
        .with_horizon_epochs(5)
        .with_seed(42);

    let strategy = FspAdaptiveStrategy::new(MockAdaptivePolicy { calls: 0 });
    let mut engine = SimulationEngine::new(config, Box::new(strategy)).expect("valid engine");
    let report = engine.run().expect("run");
    assert_eq!(report.strategy_name, "StrategyC_MockAdaptivePolicy");
    assert!(!report.metrics.irrecoverable_data_loss);
}

#[test]
fn test_correlated_failure_scenarios_are_represented_correctly() {
    // 5 failure domains with 4 shards per domain (5 * 4 = 20 shards).
    // With RS(20,12), losing 2 entire domains permanently destroys 8 shards (leaving 12 = k, still recoverable).
    // Losing a 3rd domain permanently destroys 12 shards (leaving 8 < k=12, irrecoverable data loss).
    let config = SimulationConfig::default_rs_20_12()
        .with_failure_domains(5, 4)
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    let mut engine = SimulationEngine::new(
        config,
        Box::new(FixedRepairStrategy::only_permanent_below(0)), // disable repair to test raw domain blast radius
    )
    .expect("engine init");

    // Correlated permanent failure of Domain 0 and Domain 1 at t=10s -> 8 shards lost, 12 remain.
    engine.inject_event(ScheduledEvent {
        time_secs: 10.0,
        sequence: 0,
        kind: EventKind::DomainPermanentFailure {
            domain_id: FailureDomainId(0),
        },
    });
    engine.inject_event(ScheduledEvent {
        time_secs: 10.0,
        sequence: 0,
        kind: EventKind::DomainPermanentFailure {
            domain_id: FailureDomainId(1),
        },
    });

    engine
        .step_until(20.0)
        .expect("step after 2 domain failures");
    let status_after_2 = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(status_after_2.durable_distinct_shards, 12);
    assert_eq!(
        status_after_2.state,
        SystemDataState::AvailableAndRecoverable
    );

    // Correlated temporary outage of Domain 2 at t=30s for 100s -> 4 more shards temporarily offline.
    // Available = 8 (< 12), Durable = 12 (>= 12).
    engine.inject_event(ScheduledEvent {
        time_secs: 30.0,
        sequence: 0,
        kind: EventKind::DomainOutage {
            domain_id: FailureDomainId(2),
            duration_secs: 100.0,
        },
    });

    engine
        .step_until(50.0)
        .expect("step during domain 2 outage");
    let status_during_outage = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(status_during_outage.available_distinct_shards, 8);
    assert_eq!(status_during_outage.durable_distinct_shards, 12);
    assert_eq!(
        status_during_outage.state,
        SystemDataState::TemporarilyUnavailableButDurable
    );

    // Domain 2 recovers at t=130s -> Available back to 12.
    engine
        .step_until(150.0)
        .expect("step after domain 2 recovery");
    let status_recovered = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(status_recovered.available_distinct_shards, 12);
    assert_eq!(
        status_recovered.state,
        SystemDataState::AvailableAndRecoverable
    );

    // Correlated permanent failure of Domain 3 at t=200s -> Durable drops to 8 (< 12) -> Irrecoverable!
    engine.inject_event(ScheduledEvent {
        time_secs: 200.0,
        sequence: 0,
        kind: EventKind::DomainPermanentFailure {
            domain_id: FailureDomainId(3),
        },
    });
    let report = engine.run().expect("finish run");
    assert!(report.metrics.irrecoverable_data_loss);
    assert_eq!(
        evaluate_recoverability(engine.cluster(), engine.erasure_code()).state,
        SystemDataState::IrrecoverablyLost
    );
}

#[test]
fn test_metric_calculations_maintain_documented_invariants() {
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Percent5)
        .with_permanent_failure_rate_per_epoch(0.005)
        .with_horizon_epochs(10)
        .with_seed(12345);

    let mut engine =
        SimulationEngine::new(config, Box::new(FixedRepairStrategy::new(16))).expect("engine init");

    let report = engine.run().expect("run");
    let m = &report.metrics;

    // 1. Probabilities and availability must lie strictly in [0.0, 1.0].
    assert!((0.0..=1.0).contains(&m.availability_empirical));
    assert!((0.0..=1.0).contains(&m.data_loss_probability_empirical));
    assert!((0.0..=1.0).contains(&m.failure_domain_exposure));
    assert!((0.0..=1.0).contains(&m.analytical_horizon_survival_no_repair));

    // 2. Nominal storage overhead for RS(20,12) is 20 / 12 = 1.666666...
    assert!((m.nominal_storage_overhead - (20.0 / 12.0)).abs() < 1e-9);

    // 3. Survival curve points must be non-increasing and bounded in [0.0, 1.0].
    let mut prev_s = 1.0;
    for pt in &m.survival_curve {
        assert!((0.0..=1.0).contains(&pt.empirical_survival));
        assert!(pt.empirical_survival <= prev_s + 1e-12);
        prev_s = pt.empirical_survival;
    }

    // 4. Right-censored empirical MTTDL guardrail: with 0 data losses, MTTDL must be RightCensored.
    let mttdl_zero_losses = estimate_empirical_mttdl(&[None, None, None], 864_000.0);
    assert_eq!(
        mttdl_zero_losses,
        EmpiricalMttdlResult::RightCensoredZeroLosses
    );

    // 5. Analytical binomial survival probability invariants:
    let s_zero_hazard = analytical_binomial_survival_probability(20, 12, 0.0, 86_400.0);
    assert!((s_zero_hazard - 1.0).abs() < 1e-12);
    let s_below_k = analytical_binomial_survival_probability(11, 12, 0.001, 86_400.0);
    assert_eq!(s_below_k, 0.0);
}
