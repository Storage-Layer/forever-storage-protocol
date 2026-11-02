//! Determinism tests for FSP Phase 0 simulator (Requirement #4).

use fsp_sim::config::{ChurnScenario, ErasureCodeConfig, SimulationConfig};
use fsp_sim::simulation::engine::SimulationEngine;
use fsp_sim::strategies::fixed_repair::FixedRepairStrategy;
use fsp_sim::strategies::replication::ReplicationStrategy;

#[test]
fn test_identical_seeds_and_configs_produce_identical_results() {
    let config = SimulationConfig::default_rs_20_12()
        .with_seed(42_000_001)
        .with_churn_scenario(ChurnScenario::Percent10)
        .with_permanent_failure_rate_per_epoch(0.005)
        .with_horizon_epochs(30);

    let mut engine_1 =
        SimulationEngine::new(config.clone(), Box::new(FixedRepairStrategy::new(16)))
            .expect("valid engine 1");
    let report_1 = engine_1.run().expect("run 1 succeeds");

    let mut engine_2 =
        SimulationEngine::new(config.clone(), Box::new(FixedRepairStrategy::new(16)))
            .expect("valid engine 2");
    let report_2 = engine_2.run().expect("run 2 succeeds");

    assert_eq!(report_1, report_2);
    assert_eq!(report_1.to_json(), report_2.to_json());
}

#[test]
fn test_different_seeds_produce_divergent_event_traces_under_churn() {
    let config_a = SimulationConfig::default_rs_20_12()
        .with_seed(101)
        .with_churn_scenario(ChurnScenario::Percent20)
        .with_permanent_failure_rate_per_epoch(0.01)
        .with_horizon_epochs(30);

    let config_b = config_a.clone().with_seed(202);

    let mut engine_a = SimulationEngine::new(config_a, Box::new(FixedRepairStrategy::new(16)))
        .expect("valid engine a");
    let report_a = engine_a.run().expect("run a");

    let mut engine_b = SimulationEngine::new(config_b, Box::new(FixedRepairStrategy::new(16)))
        .expect("valid engine b");
    let report_b = engine_b.run().expect("run b");

    assert_ne!(report_a.event_trace_digest, report_b.event_trace_digest);
}

#[test]
fn test_replication_baseline_deterministic_replay() {
    let config = SimulationConfig::default_rs_20_12()
        .with_erasure_code(ErasureCodeConfig::replication(3))
        .with_seed(777)
        .with_churn_scenario(ChurnScenario::Percent5)
        .with_permanent_failure_rate_per_epoch(0.002)
        .with_horizon_epochs(20);

    let mut e1 = SimulationEngine::new(config.clone(), Box::new(ReplicationStrategy::new(3)))
        .expect("valid replication engine 1");
    let mut e2 = SimulationEngine::new(config, Box::new(ReplicationStrategy::new(3)))
        .expect("valid replication engine 2");

    assert_eq!(e1.run().unwrap(), e2.run().unwrap());
}
