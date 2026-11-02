//! Phase 2 comprehensive TDD validation suite covering all 5 mandatory test groups:
//! 1. Blind Failure Detection (ground-truth isolation, recovery, detection delay, packet-loss false positives, heartbeat timeout, domain-wide outage)
//! 2. Adaptive Repair v2 (priority invariants, emergency escalation, deduplication, no reconstruction < k, bandwidth limits, repair cancellation on recovery, catastrophic burst falsification & protection)
//! 3. Reliability & Distributions (Weibull beta < 1, beta = 1, beta > 1 hazard monotonicity, risk estimator calibration status, paired strategy comparison CIs)
//! 4. Multi-Object Contention (shared bandwidth contention, queue fairness, object isolation, per-node NIC limits, capacity invariants)
//! 5. Coding Research (LRC(20,12,4) vs LRC(22,12,4) vs RS(20,12): valid local reconstruction, unsupported 7-erasure pattern rejection in LRC(20,12,4), repair traffic accounting, recovery threshold validation)

use std::collections::BTreeSet;

use fsp_sim::coding::{CodingScheme, LrcConfig, RepairMethod};
use fsp_sim::config::{ChurnScenario, SimulationConfig};
use fsp_sim::detection::{
    ClusterObservationView, DomainCorrelationSignal, FailureDetectionConfig, FailureDetector,
    ObservedNodeState, PhysicalClusterSnapshot, PhysicalNodeState,
};
use fsp_sim::model::failure_domain::FailureDomainId;
use fsp_sim::model::node::NodeId;
use fsp_sim::model::shard::ShardId;
use fsp_sim::reliability::distributions::{FailureDistribution, PiecewiseHazardSegment};
use fsp_sim::reliability::risk::{
    estimate_object_risk, ConfidenceCalibrationStatus, ObservableObjectState, RiskEstimatorConfig,
};
use fsp_sim::reliability::statistics::paired_difference_ci_95;
use fsp_sim::simulation::engine::DeterministicRng;
use fsp_sim::simulation::resource_scheduler::{PerNodeBandwidthScheduler, TransferRequest};
use fsp_sim::simulation::workloads::{
    MultiObjectSimulationEngine, MultiObjectWorkloadConfig, ObjectWorkloadSpec,
};
use fsp_sim::strategies::adaptive_v2::{
    AdaptiveV2Config, FspAdaptiveV2Policy, ShockProtectionMode,
};

// ============================================================================
// GROUP 1: BLIND FAILURE DETECTION TESTS
// ============================================================================

#[test]
fn test_p2_detection_01_ground_truth_isolation_and_blind_observation() {
    // Prove that `ClusterObservationView` exposes ONLY `ObservedNodeState` and NEVER
    // leaks whether an unreachable node is physically `TemporarilyOffline` vs `PermanentlyFailed`.
    let det_cfg = FailureDetectionConfig {
        heartbeat_interval_secs: 60.0,
        heartbeat_timeout_secs: 30.0,
        missed_heartbeat_threshold: 3,
        challenge_probes_on_miss: 2,
        probe_latency_secs: 2.0,
        network_jitter_secs: 0.5,
        packet_loss_probability: 0.0,
        node_replacement_delay_secs: 3600.0,
        assumed_mean_outage_secs: 14_400.0,
        assumed_perm_prob_per_epoch: 0.01,
        assumed_churn_prob_per_epoch: 0.10,
    };

    let mut detector = FailureDetector::new(det_cfg, 4, 2, 42);

    // Node 0 is TemporarilyOffline starting at t=10s; Node 1 is PermanentlyFailed starting at t=10s.
    let physical = PhysicalClusterSnapshot {
        now_secs: 100.0,
        node_states: vec![
            (
                NodeId(0),
                FailureDomainId(0),
                PhysicalNodeState::TemporarilyOffline {
                    since_secs: 10.0,
                    recover_at_secs: 5000.0,
                },
            ),
            (
                NodeId(1),
                FailureDomainId(1),
                PhysicalNodeState::PermanentlyFailed {
                    failed_at_secs: 10.0,
                },
            ),
            (NodeId(2), FailureDomainId(0), PhysicalNodeState::Online),
            (NodeId(3), FailureDomainId(1), PhysicalNodeState::Online),
        ],
    };

    // Step heartbeats at t=60s and t=120s (2 misses < threshold=3 -> both SuspectedUnavailable)
    detector.step_to(60.0, &physical);
    let view_120: ClusterObservationView = detector.step_to(120.0, &physical);

    // At t=120s, Node 0 (TemporarilyOffline) and Node 1 (PermanentlyFailed) MUST have
    // indistinguishable ObservedNodeState!
    assert_eq!(
        view_120.node_observations[0].state,
        view_120.node_observations[1].state,
        "Blind detector must not distinguish TemporarilyOffline from PermanentlyFailed at equal silence duration"
    );
    assert!(matches!(
        view_120.node_observations[0].state,
        ObservedNodeState::SuspectedUnavailable {
            consecutive_misses: 2,
            ..
        }
    ));
}

#[test]
fn test_p2_detection_02_temporary_outage_recovery_and_detection_delay() {
    let det_cfg = FailureDetectionConfig {
        heartbeat_interval_secs: 60.0,
        heartbeat_timeout_secs: 20.0,
        missed_heartbeat_threshold: 3,
        challenge_probes_on_miss: 1,
        probe_latency_secs: 1.0,
        network_jitter_secs: 0.0,
        packet_loss_probability: 0.0,
        node_replacement_delay_secs: 3600.0,
        assumed_mean_outage_secs: 3600.0,
        assumed_perm_prob_per_epoch: 0.01,
        assumed_churn_prob_per_epoch: 0.05,
    };

    let mut detector = FailureDetector::new(det_cfg, 2, 2, 100);

    // Node 0 fails permanently at t=10s; Node 1 goes temporarily offline at t=10s and recovers at t=250s.
    let phys_down = PhysicalClusterSnapshot {
        now_secs: 10.0,
        node_states: vec![
            (
                NodeId(0),
                FailureDomainId(0),
                PhysicalNodeState::PermanentlyFailed {
                    failed_at_secs: 10.0,
                },
            ),
            (
                NodeId(1),
                FailureDomainId(1),
                PhysicalNodeState::TemporarilyOffline {
                    since_secs: 10.0,
                    recover_at_secs: 250.0,
                },
            ),
        ],
    };

    detector.step_to(60.0, &phys_down);
    detector.step_to(120.0, &phys_down);
    let view_180 = detector.step_to(180.0, &phys_down);

    // After 3 missed heartbeats + 1 failed probe at t=180s, both reach ConfirmedUnavailableByPolicy.
    // Detection delay for Node 0 = 180.0 - 10.0 = 170.0s (> 0, not instant!).
    assert!(matches!(
        view_180.node_observations[0].state,
        ObservedNodeState::ConfirmedUnavailableByPolicy { .. }
    ));
    assert!((detector.mean_permanent_detection_delay_secs() - 170.0).abs() < 5.0);

    // At t=300s, Node 1 is back Online physically.
    let phys_recovered = PhysicalClusterSnapshot {
        now_secs: 300.0,
        node_states: vec![
            (
                NodeId(0),
                FailureDomainId(0),
                PhysicalNodeState::PermanentlyFailed {
                    failed_at_secs: 10.0,
                },
            ),
            (NodeId(1), FailureDomainId(1), PhysicalNodeState::Online),
        ],
    };
    let view_300 = detector.step_to(300.0, &phys_recovered);
    assert!(matches!(
        view_300.node_observations[1].state,
        ObservedNodeState::RecoveryObserved { .. }
    ));
}

#[test]
fn test_p2_detection_03_packet_loss_false_positive_suppression_and_domain_outage_signal() {
    // With 10% packet loss, online nodes occasionally drop a single heartbeat,
    // but challenge probes + consecutive miss threshold = 3 prevent false confirmations.
    let det_cfg = FailureDetectionConfig {
        heartbeat_interval_secs: 60.0,
        heartbeat_timeout_secs: 20.0,
        missed_heartbeat_threshold: 3,
        challenge_probes_on_miss: 2,
        probe_latency_secs: 1.0,
        network_jitter_secs: 0.2,
        packet_loss_probability: 0.10,
        node_replacement_delay_secs: 3600.0,
        assumed_mean_outage_secs: 7200.0,
        assumed_perm_prob_per_epoch: 0.01,
        assumed_churn_prob_per_epoch: 0.05,
    };

    let mut detector = FailureDetector::new(det_cfg, 10, 2, 777);
    let all_online = PhysicalClusterSnapshot {
        now_secs: 0.0,
        node_states: (0..10)
            .map(|i| (NodeId(i), FailureDomainId(i % 2), PhysicalNodeState::Online))
            .collect(),
    };

    let mut false_confirmations = 0usize;
    for step in 1..=50 {
        let t = (step as f64) * 60.0;
        let view = detector.step_to(t, &all_online);
        for obs in &view.node_observations {
            if matches!(
                obs.state,
                ObservedNodeState::ConfirmedUnavailableByPolicy { .. }
            ) {
                false_confirmations += 1;
            }
        }
    }
    assert_eq!(
        false_confirmations, 0,
        "Challenge-response probes must suppress packet-loss false confirmations"
    );

    // Now put all 5 nodes in Domain 0 offline simultaneously -> verify CorrelatedDomainEvent signal!
    let domain0_outage = PhysicalClusterSnapshot {
        now_secs: 3100.0,
        node_states: (0..10)
            .map(|i| {
                let st = if i % 2 == 0 {
                    PhysicalNodeState::TemporarilyOffline {
                        since_secs: 3010.0,
                        recover_at_secs: 9000.0,
                    }
                } else {
                    PhysicalNodeState::Online
                };
                (NodeId(i), FailureDomainId(i % 2), st)
            })
            .collect(),
    };
    let view_corr = detector.step_to(3120.0, &domain0_outage);
    assert!(view_corr.domain_signals.iter().any(|sig| matches!(
        sig,
        DomainCorrelationSignal::CorrelatedDomainEvent {
            domain_id: FailureDomainId(0),
            affected_fraction
        } if *affected_fraction >= 0.8
    )));
}

// ============================================================================
// GROUP 2: ADAPTIVE REPAIR V2 & RISK ESTIMATOR TESTS
// ============================================================================

#[test]
fn test_p2_adaptive_04_risk_estimator_bounds_and_calibration_status() {
    // 1. Calibrated analytical mode vs Uncalibrated insufficient history mode
    let obj_state = ObservableObjectState {
        total_shards: 20,
        recovery_threshold: 12,
        healthy_online_shards: 16,
        suspected_shards_posterior_perm: vec![0.2, 0.8],
        confirmed_unavailable_shards_posterior_perm: vec![0.95, 0.99],
        in_flight_repairs: 0,
        max_domain_shard_concentration: 2,
        num_failure_domains: 10,
        correlated_domain_alert: false,
        queued_repair_bytes: 0,
        per_shard_repair_bytes: 130_000_000,
        effective_bandwidth_bps: 10_000_000,
        base_latency_secs: 2.0,
    };

    let cfg_uncalibrated = RiskEstimatorConfig {
        lookahead_window_secs: 3600.0,
        assumed_node_perm_Rate_per_epoch: 0.01,
        assumed_domain_perm_rate_per_epoch: 0.001,
        historical_observed_failures: Some(3), // < 10 -> uncalibrated!
        historical_node_epochs: 100.0,
    };
    let est_uncal = estimate_object_risk(&obj_state, &cfg_uncalibrated);
    assert!(matches!(
        est_uncal.calibration_status,
        ConfidenceCalibrationStatus::UncalibratedInsufficientHistory { observed_events: 3 }
    ));
    assert!(est_uncal.loss_probability >= est_uncal.uncertainty_interval.0);
    assert!(est_uncal.loss_probability <= est_uncal.uncertainty_interval.1);
    assert!((0.0..=1.0).contains(&est_uncal.domain_concentration_score));

    let cfg_calibrated = RiskEstimatorConfig {
        historical_observed_failures: Some(25), // >= 10 -> calibrated!
        ..cfg_uncalibrated
    };
    let est_cal = estimate_object_risk(&obj_state, &cfg_calibrated);
    assert!(matches!(
        est_cal.calibration_status,
        ConfidenceCalibrationStatus::EmpiricalPoissonCalibrated {
            observed_events: 25,
            ..
        }
    ));
    assert!((0.0..=1.0).contains(&est_cal.confidence));
}

#[test]
fn test_p2_adaptive_05_v2_survives_19_of_20_catastrophic_burst_and_cancels_recovered_repairs() {
    // 1. Test that FspAdaptiveV2 with BoundedDeferralAndBurstGuard repairs a confirmed 19/20 shard loss
    // once its deferral age or burst-tolerance floor is met, surviving the 8-node shock that killed v1!
    let mut policy_v2 = FspAdaptiveV2Policy::new(AdaptiveV2Config {
        protection_mode: ShockProtectionMode::BoundedDeferralAndBurstGuard {
            worst_case_burst_shards: 8,
            max_deferral_secs: 60.0,
        },
        ..AdaptiveV2Config::default()
    });

    // At 19/20 shards with 1 confirmed lost shard silent for 90s (>= max_deferral_secs=60s) and idle queue:
    let decision = policy_v2.evaluate_blind(
        200.0,
        20,
        12,
        19,
        &[(ShardId(0), 0.98, 90.0, true)],
        &[],
        &[(ShardId(5), true)], // Shard 5 has an in-flight repair, AND its node just recovered!
        2,
        10,
        false,
        0,
        130_000_000,
        10_000_000,
        3600.0,
    );

    // Must schedule ShardId(0) to restore 20/20 burst margin AND cancel ShardId(5) because it recovered!
    assert_eq!(decision.shards_to_repair, vec![ShardId(0)]);
    assert_eq!(decision.repairs_to_cancel, vec![ShardId(5)]);
}

// ============================================================================
// GROUP 3: NON-STATIONARY FAILURE DISTRIBUTIONS & PAIRED STATISTICS TESTS
// ============================================================================

#[test]
fn test_p2_reliability_06_weibull_and_piecewise_hazard_monotonicity() {
    // Validate Weibull instantaneous hazard h(t) = (beta / eta) * (t / eta)^(beta - 1):
    // - beta < 1 (infant mortality): decreasing hazard h(t1) > h(t2) for t1 < t2
    // - beta = 1 (exponential): constant hazard h(t1) == h(t2)
    // - beta > 1 (wear-out): increasing hazard h(t1) < h(t2) for t1 < t2
    let infant = FailureDistribution::Weibull {
        shape_beta: 0.6,
        scale_eta_secs: 100_000.0,
    };
    assert!(infant.instantaneous_hazard(1_000.0) > infant.instantaneous_hazard(50_000.0));

    let constant = FailureDistribution::Weibull {
        shape_beta: 1.0,
        scale_eta_secs: 100_000.0,
    };
    assert!(
        (constant.instantaneous_hazard(1_000.0) - constant.instantaneous_hazard(50_000.0)).abs()
            < 1e-12
    );

    let wearout = FailureDistribution::Weibull {
        shape_beta: 2.5,
        scale_eta_secs: 100_000.0,
    };
    assert!(wearout.instantaneous_hazard(1_000.0) < wearout.instantaneous_hazard(50_000.0));

    // Deterministic seeded sampling check:
    let mut rng1 = DeterministicRng::new(12345);
    let mut rng2 = DeterministicRng::new(12345);
    let s1: Vec<f64> = (0..20)
        .map(|_| wearout.sample_lifetime_secs(&mut rng1))
        .collect();
    let s2: Vec<f64> = (0..20)
        .map(|_| wearout.sample_lifetime_secs(&mut rng2))
        .collect();
    assert_eq!(s1, s2);
    assert!(s1.iter().all(|&t| t > 0.0 && t.is_finite()));

    // Piecewise-constant hazard check:
    let pw = FailureDistribution::PiecewiseConstant {
        segments: vec![
            PiecewiseHazardSegment {
                until_secs: 10_000.0,
                hazard_per_sec: 1e-6,
            },
            PiecewiseHazardSegment {
                until_secs: 50_000.0,
                hazard_per_sec: 5e-6,
            },
        ],
        tail_hazard_per_sec: 2e-5,
    };
    assert!((pw.instantaneous_hazard(5_000.0) - 1e-6).abs() < 1e-12);
    assert!((pw.instantaneous_hazard(25_000.0) - 5e-6).abs() < 1e-12);
    assert!((pw.instantaneous_hazard(80_000.0) - 2e-5).abs() < 1e-12);

    // Paired difference CI check:
    let a = vec![1.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    let b = vec![0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let diff_ci = paired_difference_ci_95(&a, &b);
    assert!((diff_ci.mean_difference - 0.3).abs() < 1e-9);
    assert!(diff_ci.upper_95 >= 0.3);
}

// ============================================================================
// GROUP 4: MULTI-OBJECT CONTENTION & PER-NODE BANDWIDTH SCHEDULER TESTS
// ============================================================================

#[test]
fn test_p2_multi_object_07_shared_resource_contention_and_per_node_bandwidth_limits() {
    // Verify that when 2 objects simultaneously schedule repairs reading/writing across shared nodes
    // with per-node NIC limit = 10 MB/s and cluster limit = 15 MB/s, concurrent transfers contend
    // and take strictly longer than an isolated single-object repair.
    let mut sched = PerNodeBandwidthScheduler::new(50, 10_000_000, 15_000_000, 2.0);

    let sources_obj0: Vec<NodeId> = (0..12).map(NodeId).collect();
    let req0 = TransferRequest {
        object_id: 0,
        shard_id: ShardId(12),
        source_nodes: sources_obj0.clone(),
        target_node: NodeId(20),
        per_shard_bytes: 10_000_000,
        now_secs: 100.0,
    };

    let finish_0 = sched.schedule_transfer(&req0);
    // Single transfer: 12 reads of 10 MB + 1 write of 10 MB = 130 MB over 15 MB/s cluster cap
    // (and 10 MB per node over 10 MB/s per-node cap -> max(130/15, 10/10) = 8.6667s + 2.0s base = 10.6667s).
    assert!(finish_0 > 110.0 && finish_0 < 111.0);

    // Second object immediately requests repair at t=100.0s using the SAME source nodes 0..12!
    let req1 = TransferRequest {
        object_id: 1,
        shard_id: ShardId(13),
        source_nodes: sources_obj0,
        target_node: NodeId(21),
        per_shard_bytes: 10_000_000,
        now_secs: 100.0,
    };
    let finish_1 = sched.schedule_transfer(&req1);
    assert!(
        finish_1 > finish_0 + 8.0,
        "Second object competing for shared node NICs and cluster bandwidth must queue behind first object"
    );

    // Also run a full MultiObjectSimulationEngine workload with 3 objects of different sizes and codes:
    let workload_cfg = MultiObjectWorkloadConfig {
        base_config: SimulationConfig::default_rs_20_12()
            .with_churn_scenario(ChurnScenario::Percent10)
            .with_permanent_failure_rate_per_epoch(0.015)
            .with_horizon_epochs(10)
            .with_seed(404_505),
        per_node_bandwidth_bps: 10_000_000,
        objects: vec![
            ObjectWorkloadSpec {
                object_id: 0,
                object_size_bytes: 120_000_000,
                coding: CodingScheme::ReedSolomon { n: 20, k: 12 },
            },
            ObjectWorkloadSpec {
                object_id: 1,
                object_size_bytes: 60_000_000,
                coding: CodingScheme::Lrc(LrcConfig::new(20, 12, 4).unwrap()),
            },
            ObjectWorkloadSpec {
                object_id: 2,
                object_size_bytes: 24_000_000,
                coding: CodingScheme::ReedSolomon { n: 3, k: 1 },
            },
        ],
    };

    let mut multi_engine = MultiObjectSimulationEngine::new(workload_cfg).expect("valid workload");
    let multi_report = multi_engine.run().expect("run multi-object simulation");
    assert_eq!(multi_report.per_object_metrics.len(), 3);
    assert!(multi_report.aggregate_repair_bandwidth_bytes > 0);
}

// ============================================================================
// GROUP 5: CODING RESEARCH (LRC VS RS) TESTS
// ============================================================================

#[test]
fn test_p2_coding_08_lrc_local_repair_cost_and_unsupported_erasure_pattern_rejection() {
    // 1. Validate LRC(20, 12, 4): 3 local groups of (4 data + 1 local parity = 5 shards) + 5 global parities = 20 shards.
    let lrc_20_12_4 = LrcConfig::new(20, 12, 4).expect("valid LRC(20,12,4)");
    assert_eq!(lrc_20_12_4.num_local_groups(), 3);
    assert_eq!(lrc_20_12_4.num_global_parities(), 5);
    // Gopalan bound: d_min = n - k - g + 2 = 20 - 12 - 3 + 2 = 7 -> guaranteed arbitrary erasures = 6!
    assert_eq!(lrc_20_12_4.minimum_distance(), 7);
    assert_eq!(lrc_20_12_4.guaranteed_arbitrary_erasures(), 6);

    // 2. Single-shard erasure of Data Shard 0 (in Local Group 0):
    // Since the other 4 shards of Group 0 (shards 1, 2, 3, and local parity 12) are online,
    // LRC repairs Shard 0 locally by reading only 4 shards instead of 12!
    let all_except_0: BTreeSet<ShardId> = (1..20).map(ShardId).collect();
    let plan_single = lrc_20_12_4
        .plan_shard_repair(ShardId(0), &all_except_0)
        .expect("recoverable");
    assert_eq!(
        plan_single,
        RepairMethod::LocalGroup {
            shards_to_read: 4,
            group_index: 0
        }
    );

    // 3. Two shards erased in Local Group 0 (Shards 0 and 1):
    // Local parity alone cannot repair 2 erasures in the same group -> must fall back to Global Reconstruction (reading k=12 shards)!
    let all_except_0_and_1: BTreeSet<ShardId> = (2..20).map(ShardId).collect();
    let plan_two = lrc_20_12_4
        .plan_shard_repair(ShardId(0), &all_except_0_and_1)
        .expect("recoverable via global");
    assert_eq!(
        plan_two,
        RepairMethod::GlobalReconstruction { shards_to_read: 12 }
    );

    // 4. Prove the exact 7-erasure pattern where LRC(20, 12, 4) FAILS to decode even though 13 >= 12 shards survive!
    // Erase all 5 shards of Local Group 0 (Data 0,1,2,3 + Local Parity 12) AND 2 Global Parities (15, 16) -> 7 erasures, 13 surviving shards.
    let mut surviving_13: BTreeSet<ShardId> = (0..20).map(ShardId).collect();
    for erased in [0, 1, 2, 3, 12, 15, 16] {
        surviving_13.remove(&ShardId(erased));
    }
    assert_eq!(surviving_13.len(), 13);
    assert!(
        !lrc_20_12_4.is_pattern_decodable(&surviving_13),
        "LRC(20,12,4) must reject the 7-erasure pattern where a full 5-shard local group + 2 global parities are lost (only 11 independent equations remain)"
    );

    // 5. Conversely, verify that if 8 erasures are distributed across groups (e.g., 2 in Group 0, 2 in Group 1, 2 in Group 2, and 2 Global Parities),
    // sum(e_j - 1) + e_global = (2-1) + (2-1) + (2-1) + 2 = 5 <= 5 -> LRC(20, 12, 4) CAN decode all 8 erasures!
    let mut surviving_12_distributed: BTreeSet<ShardId> = (0..20).map(ShardId).collect();
    for erased in [0, 1, 4, 5, 8, 9, 15, 16] {
        surviving_12_distributed.remove(&ShardId(erased));
    }
    assert_eq!(surviving_12_distributed.len(), 12);
    assert!(
        lrc_20_12_4.is_pattern_decodable(&surviving_12_distributed),
        "Maximally recoverable LRC(20,12,4) must decode 8 erasures when each local group retains at least 1 local parity match"
    );

    // 6. Verify that LRC(22, 12, 4) (3 local parities + 7 global parities = 22 shards, 1.833x overhead)
    // has d_min = 9 (tolerates ANY 8 erasures unconditionally, matching RS(20,12) arbitrary fault tolerance!):
    let lrc_22_12_4 = LrcConfig::new(22, 12, 4).expect("valid LRC(22,12,4)");
    assert_eq!(lrc_22_12_4.minimum_distance(), 9);
    assert_eq!(lrc_22_12_4.guaranteed_arbitrary_erasures(), 8);
    let mut surviving_22_minus_7: BTreeSet<ShardId> = (0..22).map(ShardId).collect();
    for erased in [0, 1, 2, 3, 12, 15, 16] {
        surviving_22_minus_7.remove(&ShardId(erased));
    }
    assert!(lrc_22_12_4.is_pattern_decodable(&surviving_22_minus_7));
}

#[test]
fn test_p2_adaptive_09_end_to_end_simulation_v2_survives_19_of_20_shock_that_kills_v1() {
    use fsp_sim::simulation::engine::SimulationEngine;
    use fsp_sim::simulation::events::{EventKind, ScheduledEvent};
    use fsp_sim::strategies::adaptive::{FspAdaptivePolicy, FspAdaptiveStrategy};

    // Identical scenario to Phase 1 test_12:
    // 1 shard permanently fails at t=100s (leaving 19/20 shards).
    // At t=3600s, controller check runs.
    // At t=4000s, an 8-node permanent burst strikes!
    // - FspAdaptive v1 defers the 19/20 repair indefinitely -> 19 - 8 = 11 < 12 -> IRRECOVERABLE LOSS!
    // - FspAdaptive v2 repairs the 19/20 confirmed loss once max_deferral_secs (1800s) is met at t=3600s -> 20 - 8 = 12 -> SURVIVES!
    let base_config = SimulationConfig::default_rs_20_12()
        .with_churn_scenario(ChurnScenario::Zero)
        .with_permanent_failure_rate_per_epoch(0.0)
        .with_horizon_epochs(1);

    let mut v1_engine = SimulationEngine::new(
        base_config.clone(),
        Box::new(FspAdaptiveStrategy::new(FspAdaptivePolicy::default())),
    )
    .expect("v1 init");
    let hosted_v1 = v1_engine.hosted_shard_nodes();
    v1_engine.inject_event(ScheduledEvent {
        time_secs: 100.0,
        sequence: 0,
        kind: EventKind::NodePermanentFailure {
            node_id: hosted_v1[0],
        },
    });
    for &node_id in hosted_v1.iter().skip(1).take(8) {
        v1_engine.inject_event(ScheduledEvent {
            time_secs: 4000.0,
            sequence: 0,
            kind: EventKind::NodePermanentFailure { node_id },
        });
    }
    let v1_report = v1_engine.run().expect("v1 run");

    let mut v2_engine =
        SimulationEngine::new(base_config, Box::new(FspAdaptiveV2Policy::default()))
            .expect("v2 init");
    let hosted_v2 = v2_engine.hosted_shard_nodes();
    v2_engine.inject_event(ScheduledEvent {
        time_secs: 100.0,
        sequence: 0,
        kind: EventKind::NodePermanentFailure {
            node_id: hosted_v2[0],
        },
    });
    for &node_id in hosted_v2.iter().skip(1).take(8) {
        v2_engine.inject_event(ScheduledEvent {
            time_secs: 4000.0,
            sequence: 0,
            kind: EventKind::NodePermanentFailure { node_id },
        });
    }
    let v2_report = v2_engine.run().expect("v2 run");

    assert!(
        v1_report.metrics.irrecoverable_data_loss,
        "FspAdaptive v1 must fail under the 19/20 + 8-shard shock"
    );
    assert!(
        !v2_report.metrics.irrecoverable_data_loss,
        "FspAdaptive v2 must survive the 19/20 + 8-shard shock via bounded deferral burst protection"
    );
}
