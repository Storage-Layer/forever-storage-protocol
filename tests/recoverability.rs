//! Recoverability & availability invariant tests for FSP Phase 0 (Requirements #1, #2, #3, #8).

use fsp_sim::config::{ChurnScenario, SimulationConfig};
use fsp_sim::model::node::NodeId;
use fsp_sim::reliability::recoverability::{evaluate_recoverability, SystemDataState};
use fsp_sim::simulation::engine::SimulationEngine;
use fsp_sim::simulation::events::{EventKind, ScheduledEvent};
use fsp_sim::strategies::fixed_repair::FixedRepairStrategy;

#[test]
fn test_recoverable_when_at_least_k_distinct_valid_shards_available() {
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    let mut engine =
        SimulationEngine::new(config, Box::new(FixedRepairStrategy::new(12))).expect("engine init");

    // Initially all 20 shards are online and durable.
    let initial_status = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(initial_status.available_distinct_shards, 20);
    assert_eq!(initial_status.durable_distinct_shards, 20);
    assert_eq!(
        initial_status.state,
        SystemDataState::AvailableAndRecoverable
    );
    assert!(initial_status.is_currently_available());
    assert!(initial_status.is_durably_recoverable());

    // Permanently fail exactly 8 shard-hosting nodes (leaving 20 - 8 = 12 = k shards).
    let hosted_nodes: Vec<NodeId> = engine.hosted_shard_nodes();
    assert_eq!(hosted_nodes.len(), 20);
    for &node_id in hosted_nodes.iter().take(8) {
        engine.inject_event(ScheduledEvent {
            time_secs: 10.0,
            sequence: 0,
            kind: EventKind::NodePermanentFailure { node_id },
        });
    }

    engine.step_until(15.0).expect("step");
    let status_at_k = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(status_at_k.available_distinct_shards, 12);
    assert_eq!(status_at_k.durable_distinct_shards, 12);
    assert_eq!(status_at_k.state, SystemDataState::AvailableAndRecoverable);
    assert!(status_at_k.is_currently_available());
    assert!(status_at_k.is_durably_recoverable());
}

#[test]
fn test_unrecoverable_when_fewer_than_k_valid_shards_remain() {
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    let mut engine = SimulationEngine::new(
        config,
        Box::new(FixedRepairStrategy::new(10)), // won't repair before k=12
    )
    .expect("engine init");

    // Permanently fail 9 shard-hosting nodes (leaving 20 - 9 = 11 < k=12 shards).
    let hosted_nodes: Vec<NodeId> = engine.hosted_shard_nodes();
    for &node_id in hosted_nodes.iter().take(9) {
        engine.inject_event(ScheduledEvent {
            time_secs: 5.0,
            sequence: 0,
            kind: EventKind::NodePermanentFailure { node_id },
        });
    }

    let report = engine.run().expect("run");
    let final_status = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(final_status.durable_distinct_shards, 11);
    assert_eq!(final_status.state, SystemDataState::IrrecoverablyLost);
    assert!(!final_status.is_currently_available());
    assert!(!final_status.is_durably_recoverable());
    assert!(report.metrics.irrecoverable_data_loss);
    assert_eq!(report.metrics.data_loss_probability_empirical, 1.0);
}

#[test]
fn test_temporary_outages_do_not_imply_permanent_shard_deletion() {
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    // Use a strategy with repair disabled (threshold 0) so only node outage/recovery acts.
    let mut engine = SimulationEngine::new(
        config,
        Box::new(FixedRepairStrategy::only_permanent_below(0)),
    )
    .expect("engine init");

    let hosted_nodes: Vec<NodeId> = engine.hosted_shard_nodes();
    // Put 10 shard-hosting nodes into temporary outage from t=100s to t=500s.
    // Available shards during [100, 500) will be 20 - 10 = 10 (< k=12).
    // Durable shards during [100, 500) must remain 20 (>= k=12)!
    for &node_id in hosted_nodes.iter().take(10) {
        engine.inject_event(ScheduledEvent {
            time_secs: 100.0,
            sequence: 0,
            kind: EventKind::NodeOutage {
                node_id,
                duration_secs: 400.0,
            },
        });
    }

    engine.step_until(250.0).expect("step into outage window");
    let mid_status = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(mid_status.available_distinct_shards, 10);
    assert_eq!(mid_status.durable_distinct_shards, 20);
    assert_eq!(
        mid_status.state,
        SystemDataState::TemporarilyUnavailableButDurable
    );
    assert!(!mid_status.is_currently_available());
    assert!(mid_status.is_durably_recoverable());

    // Advance past recovery at t=500s.
    let report = engine.run().expect("complete run");
    let end_status = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(end_status.available_distinct_shards, 20);
    assert_eq!(end_status.durable_distinct_shards, 20);
    assert_eq!(end_status.state, SystemDataState::AvailableAndRecoverable);
    assert!(!report.metrics.irrecoverable_data_loss);
    // Availability was reduced for 400s out of 86400s:
    assert!(report.metrics.availability_empirical < 1.0);
    assert!(report.metrics.availability_empirical > 0.99);
}

#[test]
fn test_zero_failure_scenario_preserves_full_recoverability_and_availability() {
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(15);

    let mut engine =
        SimulationEngine::new(config, Box::new(FixedRepairStrategy::new(16))).expect("engine init");

    let report = engine.run().expect("run");
    assert!(!report.metrics.irrecoverable_data_loss);
    assert_eq!(report.metrics.availability_empirical, 1.0);
    assert_eq!(report.metrics.data_loss_probability_empirical, 0.0);
    assert_eq!(report.metrics.repair_operation_count, 0);
    assert_eq!(report.metrics.repair_bandwidth_bytes, 0);
}
