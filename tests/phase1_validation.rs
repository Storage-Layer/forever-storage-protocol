//! Phase 1 comprehensive TDD validation suite covering all 15 mandatory categories
//! plus deterministic property-based randomized invariant testing.

use fsp_sim::config::{
    BurstFailureConfig, ChurnScenario, ErasureCodeConfig, OutageDurationDistribution,
    SimulationConfig, SECONDS_PER_EPOCH,
};
use fsp_sim::model::shard::ShardId;
use fsp_sim::reliability::recoverability::{evaluate_recoverability, SystemDataState};
use fsp_sim::reliability::statistics::{
    compute_kaplan_meier_and_rmst, wilson_score_interval_95, UnrestrictedMttdlStatus,
};
use fsp_sim::reliability::survival::{
    analytical_binomial_survival_probability, analytical_correlated_survival_probability,
};
use fsp_sim::simulation::engine::SimulationEngine;
use fsp_sim::simulation::events::{EventKind, ScheduledEvent};
use fsp_sim::simulation::failure_generator::generate_exogenous_failure_trace;
use fsp_sim::simulation::monte_carlo::{
    run_monte_carlo_batch, MonteCarloBatchConfig, StrategyKind,
};
use fsp_sim::strategies::adaptive::{
    compute_heuristic_repair_priority, AdaptiveRiskTelemetry, FspAdaptivePolicy,
    FspAdaptiveStrategy, PriorityWeights,
};
use fsp_sim::strategies::fixed_repair::FixedRepairStrategy;
use fsp_sim::strategies::replication::ReplicationStrategy;

// 1. Monte Carlo seed reproducibility
#[test]
fn test_01_monte_carlo_seed_reproducibility() {
    let base_config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Percent10)
        .with_permanent_failure_rate_per_epoch(0.01)
        .with_horizon_epochs(10)
        .with_seed(2026_0001);

    let mc_cfg = MonteCarloBatchConfig {
        base_config,
        strategy_kind: StrategyKind::FspAdaptive,
        num_samples: 12,
        worker_threads: 2,
    };

    let res_1 = run_monte_carlo_batch(&mc_cfg).expect("batch 1");
    let res_2 = run_monte_carlo_batch(&mc_cfg).expect("batch 2");

    assert_eq!(res_1, res_2);
    assert_eq!(res_1.to_json(), res_2.to_json());
}

// 2. Worker-count-independent output
#[test]
fn test_02_worker_count_independent_output() {
    let base_config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Percent20)
        .with_permanent_failure_rate_per_epoch(0.015)
        .with_correlated_domain_failure_rate_per_epoch(0.002)
        .with_horizon_epochs(10)
        .with_seed(900_100);

    let res_1_worker = run_monte_carlo_batch(&MonteCarloBatchConfig {
        base_config: base_config.clone(),
        strategy_kind: StrategyKind::FixedRepair { threshold: 16 },
        num_samples: 16,
        worker_threads: 1,
    })
    .expect("1 worker");

    let res_4_workers = run_monte_carlo_batch(&MonteCarloBatchConfig {
        base_config: base_config.clone(),
        strategy_kind: StrategyKind::FixedRepair { threshold: 16 },
        num_samples: 16,
        worker_threads: 4,
    })
    .expect("4 workers");

    let res_8_workers = run_monte_carlo_batch(&MonteCarloBatchConfig {
        base_config,
        strategy_kind: StrategyKind::FixedRepair { threshold: 16 },
        num_samples: 16,
        worker_threads: 8,
    })
    .expect("8 workers");

    assert_eq!(res_1_worker, res_4_workers);
    assert_eq!(res_4_workers, res_8_workers);
}

// 3. Identical exogenous failure traces across comparison strategies
#[test]
fn test_03_identical_exogenous_failure_traces_across_strategies() {
    let rs_cfg = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Percent10)
        .with_permanent_failure_rate_per_epoch(0.01)
        .with_domain_outage_rate_per_epoch(0.005, 7200.0)
        .with_correlated_domain_failure_rate_per_epoch(0.001)
        .with_burst_failure(BurstFailureConfig {
            probability_per_epoch: 0.01,
            affected_nodes: 3,
            is_permanent: false,
            outage_duration_secs: 3600.0,
        })
        .with_horizon_epochs(10)
        .with_seed(777_888);

    // Replication strategy config with identical cluster topology and seed
    let rep_cfg = rs_cfg
        .clone()
        .with_erasure_code(ErasureCodeConfig::replication(3));

    let trace_rs = generate_exogenous_failure_trace(&rs_cfg, 0);
    let trace_rep = generate_exogenous_failure_trace(&rep_cfg, 0);

    assert!(!trace_rs.events.is_empty());
    assert_eq!(trace_rs.trace_digest, trace_rep.trace_digest);
    assert_eq!(trace_rs.events, trace_rep.events);

    // Also verify that changing churn rate does NOT alter the permanent failure events (orthogonal streams):
    let rs_cfg_diff_churn = rs_cfg.clone().with_churn_scenario(ChurnScenario::Percent20);
    let trace_diff_churn = generate_exogenous_failure_trace(&rs_cfg_diff_churn, 0);

    let perm_events_1: Vec<_> = trace_rs
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::NodePermanentFailure { .. }))
        .collect();
    let perm_events_2: Vec<_> = trace_diff_churn
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::NodePermanentFailure { .. }))
        .collect();
    assert_eq!(perm_events_1, perm_events_2);
}

// 4. Wilson score confidence interval correctness
#[test]
fn test_04_wilson_score_confidence_interval_correctness() {
    // Case A: 0 losses out of 1000 runs -> point = 0.0, lower = 0.0, upper ≈ 0.003827
    let ci_zero = wilson_score_interval_95(0, 1000);
    assert_eq!(ci_zero.point_estimate, 0.0);
    assert_eq!(ci_zero.lower_bound, 0.0);
    assert!((ci_zero.upper_bound - 0.0038267).abs() < 1e-4);

    // Case B: 1000 losses out of 1000 runs -> point = 1.0, upper = 1.0, lower ≈ 0.996173
    let ci_all = wilson_score_interval_95(1000, 1000);
    assert_eq!(ci_all.point_estimate, 1.0);
    assert_eq!(ci_all.upper_bound, 1.0);
    assert!((ci_all.lower_bound - 0.9961733).abs() < 1e-4);

    // Case C: 50 losses out of 1000 runs -> point = 0.05, bounds strictly contain 0.05 and lie in [0, 1]
    let ci_mid = wilson_score_interval_95(50, 1000);
    assert!((ci_mid.point_estimate - 0.05).abs() < 1e-12);
    assert!(ci_mid.lower_bound > 0.038 && ci_mid.lower_bound < 0.05);
    assert!(ci_mid.upper_bound > 0.05 && ci_mid.upper_bound < 0.066);
}

// 5. Right-censoring correctness (Kaplan-Meier & RMST)
#[test]
fn test_05_right_censoring_kaplan_meier_and_rmst_correctness() {
    let horizon = 100.0;
    // 4 runs: 2 fail at t=20 and t=60, 2 survive (right-censored at t=100).
    let observations = vec![Some(20.0), Some(60.0), None, None];
    let km = compute_kaplan_meier_and_rmst(&observations, horizon);

    assert_eq!(km.total_samples, 4);
    assert_eq!(km.observed_failures, 2);
    assert_eq!(km.right_censored_samples, 2);
    // RMST = (20 + 60 + 100 + 100) / 4 = 70.0
    assert!((km.rmst_secs - 70.0).abs() < 1e-9);
    assert!((km.final_survival_probability - 0.5).abs() < 1e-9);
    // Because 2 out of 4 runs are right-censored, unrestricted MTTDL must report Censored!
    assert!(matches!(
        km.unrestricted_mttdl_status,
        UnrestrictedMttdlStatus::NotIdentifiableDueToRightCensoring {
            censored_count: 2,
            ..
        }
    ));

    // When all 4 runs are right-censored (0 losses):
    let all_censored = vec![None, None, None, None];
    let km_zero = compute_kaplan_meier_and_rmst(&all_censored, horizon);
    assert_eq!(km_zero.observed_failures, 0);
    assert_eq!(km_zero.right_censored_samples, 4);
    assert!((km_zero.rmst_secs - 100.0).abs() < 1e-9);
    assert_eq!(km_zero.final_survival_probability, 1.0);
    assert!(matches!(
        km_zero.unrestricted_mttdl_status,
        UnrestrictedMttdlStatus::ZeroObservedLosses { total_samples: 4 }
    ));
}

// 6. Time-weighted availability and node offline fraction correctness
#[test]
fn test_06_time_weighted_availability_and_node_offline_fraction_correctness() {
    // 1 epoch = 86,400s. 50 nodes.
    // Put 10 shard-hosting nodes offline for 43,200s (half the epoch).
    // Object has k=12, n=20 -> 20 - 10 = 10 < 12 available during the 43,200s outage.
    // Exact object time-weighted availability = 0.5.
    // Exact cluster node-time offline fraction = (10 nodes * 43,200s) / (50 nodes * 86,400s) = 10 / 100 = 0.10.
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    let mut engine = SimulationEngine::new(
        config,
        Box::new(FixedRepairStrategy::only_permanent_below(0)),
    )
    .expect("init");

    let hosted = engine.hosted_shard_nodes();
    for &node_id in hosted.iter().take(10) {
        engine.inject_event(ScheduledEvent {
            time_secs: 10_000.0,
            sequence: 0,
            kind: EventKind::NodeOutage {
                node_id,
                duration_secs: 43_200.0,
            },
        });
    }

    let report = engine.run().expect("run");
    assert!((report.metrics.availability_empirical - 0.5).abs() < 1e-6);
    assert!((report.metrics.measured_node_offline_fraction - 0.10).abs() < 1e-6);
    assert!(!report.metrics.irrecoverable_data_loss);
}

// 7. Correlated domain and burst failure behavior
#[test]
fn test_07_correlated_domain_and_burst_failure_behavior() {
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    let mut engine = SimulationEngine::new(
        config,
        Box::new(FixedRepairStrategy::only_permanent_below(0)),
    )
    .expect("init");

    let target_nodes: Vec<_> = engine.hosted_shard_nodes().into_iter().take(5).collect();
    engine.inject_event(ScheduledEvent {
        time_secs: 500.0,
        sequence: 0,
        kind: EventKind::BurstNodeFailure {
            node_ids: target_nodes,
            is_permanent: true,
            outage_duration_secs: 0.0,
        },
    });

    engine.step_until(600.0).expect("step");
    let rec = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(rec.durable_distinct_shards, 15);
    assert_eq!(rec.available_distinct_shards, 15);
}

// 8. Adaptive repair priority ordering
#[test]
fn test_08_adaptive_repair_priority_ordering() {
    let weights = PriorityWeights::default();

    // Telemetry with mild loss (19 durable shards out of 20, low domain exposure, low risk)
    let low_risk_telemetry = AdaptiveRiskTelemetry {
        now_secs: 1000.0,
        available_distinct_shards: 19,
        durable_distinct_shards: 19,
        in_flight_repairs: 0,
        recovery_threshold: 12,
        total_shards: 20,
        max_domain_concentration: 2,
        failure_domain_exposure: 0.22,
        estimated_survival_probability: 0.9999,
        window_loss_risk: 0.0001,
        single_shard_repair_cost_bytes: 130_000_000,
        queued_repair_bytes: 0,
        bandwidth_budget_bytes_per_interval: 36_000_000_000,
        expected_repair_duration_secs: 15.0,
        missing_shards: vec![ShardId(0)],
        suspected_offline_shards: vec![],
    };

    // Telemetry with critical loss (13 durable shards out of 20, high domain exposure, high risk)
    let high_risk_telemetry = AdaptiveRiskTelemetry {
        durable_distinct_shards: 13,
        available_distinct_shards: 13,
        failure_domain_exposure: 1.0,
        window_loss_risk: 0.25,
        missing_shards: (0..7).map(ShardId).collect(),
        ..low_risk_telemetry.clone()
    };

    let p_low = compute_heuristic_repair_priority(&low_risk_telemetry, &weights);
    let p_high = compute_heuristic_repair_priority(&high_risk_telemetry, &weights);

    assert!(
        p_high > p_low,
        "High risk priority ({}) must strictly exceed low risk priority ({})",
        p_high,
        p_low
    );
    assert!(p_high >= 0.8);
}

// 9. Bandwidth budget enforcement & queue accounting
#[test]
fn test_09_bandwidth_budget_enforcement_and_queue_accounting() {
    // 3 missing shards, each requiring 130 MB transfer at 10 MB/s + 2s base latency = 15s each.
    // 3 sequential repairs must take 3 * 15s = 45s total before all 3 complete.
    let shard_bytes = 10_000_000;
    let config = SimulationConfig::default_rs_20_12()
        .with_object_size_bytes(shard_bytes * 12)
        .with_repair_bandwidth_bytes_per_sec(10_000_000)
        .with_repair_base_latency_secs(2.0)
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    let mut engine =
        SimulationEngine::new(config, Box::new(FixedRepairStrategy::new(19))).expect("init");

    let hosted = engine.hosted_shard_nodes();
    for &node_id in hosted.iter().take(3) {
        engine.inject_event(ScheduledEvent {
            time_secs: 100.0,
            sequence: 0,
            kind: EventKind::NodePermanentFailure { node_id },
        });
    }

    // At t = 120s (20s elapsed): 1st repair (at 115s) finished, 2nd (at 130s) and 3rd (at 145s) still in flight!
    engine.step_until(120.0).expect("step 120s");
    let rec_120 = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(rec_120.durable_distinct_shards, 18);

    // At t = 135s: 2nd finished, 3rd still in flight.
    engine.step_until(135.0).expect("step 135s");
    let rec_135 = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(rec_135.durable_distinct_shards, 19);

    // At t = 150s: all 3 finished.
    engine.step_until(150.0).expect("step 150s");
    let rec_150 = evaluate_recoverability(engine.cluster(), engine.erasure_code());
    assert_eq!(rec_150.durable_distinct_shards, 20);
}

// 10. Repair queue deduplication and loop prevention
#[test]
fn test_10_repair_queue_deduplication_and_loop_prevention() {
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Percent20)
        .with_permanent_failure_rate_per_epoch(0.02)
        .with_horizon_epochs(10)
        .with_seed(555_666);

    let mut engine = SimulationEngine::new(
        config,
        Box::new(FspAdaptiveStrategy::new(FspAdaptivePolicy::default())),
    )
    .expect("init");

    let report = engine.run().expect("run");
    // Ensure finite event processing without runaway repair scheduling loops:
    assert!(report.events_processed < 50_000);
    assert_eq!(engine.cluster().in_flight_repair_count(), 0);
}

// 11. Unrecoverable absorbing state invariants
#[test]
fn test_11_unrecoverable_absorbing_state_invariants() {
    let config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(2);

    let mut engine = SimulationEngine::new(
        config,
        Box::new(FspAdaptiveStrategy::new(FspAdaptivePolicy::default())),
    )
    .expect("init");

    // Simultaneously fail 9 shard-hosting nodes at t=50s -> 11 shards remain (< k=12).
    let hosted = engine.hosted_shard_nodes();
    for &node_id in hosted.iter().take(9) {
        engine.inject_event(ScheduledEvent {
            time_secs: 50.0,
            sequence: 0,
            kind: EventKind::NodePermanentFailure { node_id },
        });
    }

    let report = engine.run().expect("run");
    assert!(report.metrics.irrecoverable_data_loss);
    assert_eq!(report.metrics.data_loss_time_secs, Some(50.0));
    // Once irrecoverably lost at t=50s, zero repairs can ever complete afterward:
    assert_eq!(report.metrics.repair_operation_count, 0);
    assert_eq!(
        evaluate_recoverability(engine.cluster(), engine.erasure_code()).state,
        SystemDataState::IrrecoverablyLost
    );
}

// 12. Falsification scenario where adaptive repair performs worse than eager fixed repair
#[test]
fn test_12_falsification_scenarios_where_adaptive_repair_performs_worse() {
    // Scientific falsification test:
    // Suppose 1 shard is permanently lost at t=100s (leaving 19 durable shards).
    // Eager FixedRepair(threshold=19) immediately repairs that 1 shard at t=115s, restoring 20 shards.
    // Conservative AdaptiveRepair (which defers single-shard repair when 19/20 shards are healthy to save bandwidth)
    // leaves the object at 19 shards.
    // Then at t=200s, a sudden correlated burst permanently destroys 8 more shard-hosting nodes!
    // - Eager FixedRepair had 20 shards -> 20 - 8 = 12 >= k=12 -> SURVIVES!
    // - Deferred AdaptiveRepair had 19 shards -> 19 - 8 = 11 < k=12 -> IRRECOVERABLE DATA LOSS!
    let base_config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    // Run 1: Eager FixedRepair(19)
    let mut eager_engine =
        SimulationEngine::new(base_config.clone(), Box::new(FixedRepairStrategy::new(19)))
            .expect("eager init");
    let hosted_eager = eager_engine.hosted_shard_nodes();
    eager_engine.inject_event(ScheduledEvent {
        time_secs: 100.0,
        sequence: 0,
        kind: EventKind::NodePermanentFailure {
            node_id: hosted_eager[0],
        },
    });
    for &node_id in hosted_eager.iter().skip(1).take(8) {
        eager_engine.inject_event(ScheduledEvent {
            time_secs: 200.0,
            sequence: 0,
            kind: EventKind::NodePermanentFailure { node_id },
        });
    }
    let eager_report = eager_engine.run().expect("eager run");

    // Run 2: AdaptiveRepair (default deferred threshold skips 1-shard loss at 19/20)
    let mut adaptive_engine = SimulationEngine::new(
        base_config,
        Box::new(FspAdaptiveStrategy::new(FspAdaptivePolicy::default())),
    )
    .expect("adaptive init");
    let hosted_adaptive = adaptive_engine.hosted_shard_nodes();
    adaptive_engine.inject_event(ScheduledEvent {
        time_secs: 100.0,
        sequence: 0,
        kind: EventKind::NodePermanentFailure {
            node_id: hosted_adaptive[0],
        },
    });
    for &node_id in hosted_adaptive.iter().skip(1).take(8) {
        adaptive_engine.inject_event(ScheduledEvent {
            time_secs: 200.0,
            sequence: 0,
            kind: EventKind::NodePermanentFailure { node_id },
        });
    }
    let adaptive_report = adaptive_engine.run().expect("adaptive run");

    // Eager fixed repair survived because it repaired at t=100s, and then also repaired the 8 shards lost at t=200s (1 + 8 = 9 total repairs):
    assert!(!eager_report.metrics.irrecoverable_data_loss);
    assert_eq!(eager_report.metrics.repair_operation_count, 9);

    // Adaptive repair suffered irrecoverable data loss at t=200s because it deferred the 19->20 repair:
    assert!(adaptive_report.metrics.irrecoverable_data_loss);
    assert_eq!(adaptive_report.metrics.data_loss_time_secs, Some(200.0));
}

// 13. Zero-failure analytical sanity checks
#[test]
fn test_13_zero_failure_analytical_sanity_checks() {
    let s_indep = analytical_binomial_survival_probability(20, 12, 0.0, 30.0 * SECONDS_PER_EPOCH);
    let s_corr = analytical_correlated_survival_probability(
        20,
        12,
        10,
        2,
        0.0,
        0.0,
        30.0 * SECONDS_PER_EPOCH,
    );
    assert_eq!(s_indep, 1.0);
    assert_eq!(s_corr, 1.0);

    // Correlated survival probability with positive domain failure hazard must be strictly less than
    // independent survival probability:
    let s_indep_pos =
        analytical_binomial_survival_probability(20, 12, 0.005, 30.0 * SECONDS_PER_EPOCH);
    let s_corr_pos = analytical_correlated_survival_probability(
        20,
        12,
        10,
        2,
        0.005,
        0.005,
        30.0 * SECONDS_PER_EPOCH,
    );
    assert!(s_corr_pos < s_indep_pos);
}

// 14. No-repair vs active repair comparison
#[test]
fn test_14_no_repair_vs_active_repair_comparison() {
    let base_config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Percent5)
        .with_permanent_failure_rate_per_epoch(0.05)
        .with_horizon_epochs(15)
        .with_seed(314_159);

    let no_repair_batch = run_monte_carlo_batch(&MonteCarloBatchConfig {
        base_config: base_config.clone(),
        strategy_kind: StrategyKind::NoRepair,
        num_samples: 16,
        worker_threads: 4,
    })
    .expect("no repair batch");

    let active_repair_batch = run_monte_carlo_batch(&MonteCarloBatchConfig {
        base_config,
        strategy_kind: StrategyKind::FspAdaptive,
        num_samples: 16,
        worker_threads: 4,
    })
    .expect("active repair batch");

    assert!(
        active_repair_batch.loss_probability_ci.point_estimate
            < no_repair_batch.loss_probability_ci.point_estimate,
        "Active repair loss rate ({}) must be strictly lower than NoRepair ({})",
        active_repair_batch.loss_probability_ci.point_estimate,
        no_repair_batch.loss_probability_ci.point_estimate
    );
    assert_eq!(no_repair_batch.mean_repair_operations, 0.0);
    assert!(active_repair_batch.mean_repair_operations > 0.0);
}

// 15. Cross-strategy storage overhead accounting (including matched overhead RS(24,12) vs Rep(2))
#[test]
fn test_15_cross_strategy_storage_overhead_accounting() {
    let ec_rep2 = ErasureCodeConfig::replication(2);
    let ec_rep3 = ErasureCodeConfig::replication(3);
    let ec_rs_20_12 = ErasureCodeConfig::reed_solomon(20, 12);
    let ec_rs_24_12 = ErasureCodeConfig::reed_solomon(24, 12);

    assert!((ec_rep2.nominal_storage_overhead() - 2.0).abs() < 1e-12);
    assert!((ec_rep3.nominal_storage_overhead() - 3.0).abs() < 1e-12);
    assert!((ec_rs_20_12.nominal_storage_overhead() - (20.0 / 12.0)).abs() < 1e-12);
    assert!((ec_rs_24_12.nominal_storage_overhead() - 2.0).abs() < 1e-12);

    // Matched storage overhead check: RS(24,12) and Replication(2) have identical 2.0x overhead!
    assert_eq!(
        ec_rep2.nominal_storage_overhead(),
        ec_rs_24_12.nominal_storage_overhead()
    );
}

// 16. Property-based randomized invariant testing across 100 seeds & configurations
#[test]
fn test_16_property_based_randomized_invariants_across_100_seeds() {
    for i in 0..100u64 {
        let churn = match i % 4 {
            0 => ChurnScenario::Percent1,
            1 => ChurnScenario::Percent5,
            2 => ChurnScenario::Percent10,
            _ => ChurnScenario::Percent20,
        };
        let perm_rate = 0.002 * ((i % 10) as f64);
        let dist = if i % 2 == 0 {
            OutageDurationDistribution::Exponential { mean_secs: 7200.0 }
        } else {
            OutageDurationDistribution::LogNormal {
                median_secs: 7200.0,
                sigma: 0.5,
            }
        };

        let config = SimulationConfig::default_rs_20_12()
            .with_churn_scenario(churn)
            .with_outage_distribution(dist)
            .with_permanent_failure_rate_per_epoch(perm_rate)
            .with_horizon_epochs(5)
            .with_seed(1_000_000 + i);

        let strategy: Box<dyn fsp_sim::strategies::RepairStrategy> = match i % 3 {
            0 => Box::new(ReplicationStrategy::new(20)),
            1 => Box::new(FixedRepairStrategy::new(16)),
            _ => Box::new(FspAdaptiveStrategy::new(FspAdaptivePolicy::default())),
        };

        let mut engine = SimulationEngine::new(config, strategy).expect("valid engine");
        let report = engine.run().expect("run");

        // Universal invariants that must hold for every seed:
        assert!((0.0..=1.0).contains(&report.metrics.availability_empirical));
        assert!((0.0..=1.0).contains(&report.metrics.measured_node_offline_fraction));
        assert!((0.0..=1.0).contains(&report.metrics.failure_domain_exposure));
        assert!((0.0..=1.0).contains(&report.metrics.peak_failure_domain_exposure));
        assert!(
            report.metrics.peak_failure_domain_exposure + 1e-12
                >= report.metrics.failure_domain_exposure
        );
        if report.metrics.irrecoverable_data_loss {
            assert!(report.metrics.data_loss_time_secs.is_some());
            assert_eq!(report.metrics.data_loss_probability_empirical, 1.0);
        } else {
            assert!(report.metrics.data_loss_time_secs.is_none());
            assert_eq!(report.metrics.data_loss_probability_empirical, 0.0);
        }
    }
}
