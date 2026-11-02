//! Parallel, worker-count-independent Monte Carlo batch experiment runner.
//!
//! Guarantees:
//! - Deterministic outputs regardless of thread count (`worker_threads = 1` vs `N`).
//! - Shared exogenous failure traces across comparison strategies for identical `(master_seed, sample_index)`.
//! - Explicit tracking of completed vs. failed/incomplete runs.
//! - Full statistical aggregation: Wilson score 95% CIs, Kaplan–Meier product-limit survival,
//!   Restricted Mean Survival Time (RMST), time-weighted availability, node offline fraction,
//!   and repair bandwidth/operation distributions.

use std::thread;

use crate::config::{ConfigError, SimulationConfig};
use crate::reliability::statistics::{
    compute_kaplan_meier_and_rmst, wilson_score_interval_95, ConfidenceInterval95,
    DistributionSummary, KaplanMeierSummary,
};
use crate::simulation::engine::SimulationEngine;
use crate::simulation::failure_generator::generate_exogenous_failure_trace;
use crate::strategies::adaptive::{FspAdaptivePolicy, FspAdaptiveStrategy};
use crate::strategies::adaptive_v2::{AdaptiveV2Config, FspAdaptiveV2Policy};
use crate::strategies::fixed_repair::FixedRepairStrategy;
use crate::strategies::replication::ReplicationStrategy;
use crate::strategies::RepairStrategy;

/// Enumeration of built-in strategies for parallel Monte Carlo batch execution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StrategyKind {
    /// Control baseline with repair disabled ($T_{\text{repair}} = 0$).
    NoRepair,
    /// Strategy A: $R$-way replication baseline ($k = 1, n = R$).
    Replication { replicas: usize },
    /// Strategy B: Fixed-threshold repair for $(n, k)$ erasure coding.
    FixedRepair { threshold: usize },
    /// Strategy B (Eager): Fixed-threshold repair that also replaces long-offline suspected shards.
    FixedRepairWithSuspect { threshold: usize },
    /// Strategy C (v1): Phase 1 FSP Adaptive Repair using the risk-cost heuristic priority policy.
    FspAdaptive,
    /// Strategy C (v2): Phase 2 FSP Adaptive Repair v2 with bounded deferral, burst guard, and blind risk estimation.
    FspAdaptiveV2 { config: AdaptiveV2Config },
}

impl StrategyKind {
    /// Instantiates a fresh boxed `RepairStrategy` for an independent Monte Carlo sample.
    pub fn build_strategy(self) -> Box<dyn RepairStrategy> {
        match self {
            Self::NoRepair => Box::new(FixedRepairStrategy::only_permanent_below(0)),
            Self::Replication { replicas } => Box::new(ReplicationStrategy::new(replicas)),
            Self::FixedRepair { threshold } => Box::new(FixedRepairStrategy::new(threshold)),
            Self::FixedRepairWithSuspect { threshold } => {
                Box::new(FixedRepairStrategy::new(threshold).with_repair_suspected_offline(true))
            }
            Self::FspAdaptive => Box::new(FspAdaptiveStrategy::new(FspAdaptivePolicy::default())),
            Self::FspAdaptiveV2 { config } => Box::new(FspAdaptiveV2Policy::new(config)),
        }
    }

    /// Canonical strategy label.
    pub fn label(self) -> String {
        match self {
            Self::NoRepair => "Control_NoRepair".to_string(),
            Self::Replication { replicas } => format!("StrategyA_Replication_{}x", replicas),
            Self::FixedRepair { threshold } => {
                format!("StrategyB_FixedRepair_T{}", threshold)
            }
            Self::FixedRepairWithSuspect { threshold } => {
                format!("StrategyB_FixedRepairSuspect_T{}", threshold)
            }
            Self::FspAdaptive => "StrategyC_FspAdaptiveRepair".to_string(),
            Self::FspAdaptiveV2 { .. } => "StrategyC_FspAdaptiveV2".to_string(),
        }
    }
}

/// Configuration for a parallel Monte Carlo batch experiment.
#[derive(Debug, Clone, PartialEq)]
pub struct MonteCarloBatchConfig {
    /// Base simulation configuration (`seed` acts as the master seed for sample derivation).
    pub base_config: SimulationConfig,
    /// Strategy to evaluate across all `num_samples`.
    pub strategy_kind: StrategyKind,
    /// Number of independent Monte Carlo samples ($R \ge 1$).
    pub num_samples: usize,
    /// Number of parallel OS worker threads (output is identical regardless of this value).
    pub worker_threads: usize,
}

/// Aggregated statistical results of a Monte Carlo batch experiment.
#[derive(Debug, Clone, PartialEq)]
pub struct MonteCarloBatchResult {
    /// Strategy label.
    pub strategy_label: String,
    /// Churn scenario label.
    pub churn_label: String,
    /// Erasure code $(n, k)$ tuple.
    pub erasure_code_n_k: (usize, usize),
    /// Nominal storage overhead ratio $\Omega = n / k$.
    pub nominal_storage_overhead: f64,
    /// Master seed used to derive sample seeds `0..num_samples`.
    pub master_seed: u64,
    /// Horizon in seconds.
    pub horizon_secs: f64,
    /// Total requested samples.
    pub requested_samples: usize,
    /// Number of successfully completed sample trajectories.
    pub completed_samples: usize,
    /// Number of failed/incomplete sample trajectories.
    pub failed_samples: usize,
    /// Number of samples that experienced irrecoverable data loss.
    pub irrecoverable_loss_count: usize,
    /// Number of right-censored samples (survived full horizon without data loss).
    pub right_censored_count: usize,
    /// 95% Wilson score confidence interval on finite-horizon data-loss probability $\hat{P}_{\text{loss}}(T_{\max})$.
    pub loss_probability_ci: ConfidenceInterval95,
    /// 95% Wilson score confidence interval on finite-horizon survival probability $\hat{S}(T_{\max}) = 1 - \hat{P}_{\text{loss}}(T_{\max})$.
    pub survival_probability_ci: ConfidenceInterval95,
    /// Non-parametric Kaplan–Meier and Restricted Mean Survival Time (RMST) analysis.
    pub kaplan_meier: KaplanMeierSummary,
    /// Analytical domain-correlated survival probability without repair over $[0, T_{\max}]$.
    pub analytical_no_repair_survival: f64,
    /// Time-weighted object availability distribution across samples.
    pub availability_distribution: DistributionSummary,
    /// Time-weighted cluster node-offline fraction distribution across samples.
    pub node_offline_fraction_distribution: DistributionSummary,
    /// Total repair bandwidth consumed (bytes) distribution across samples.
    pub repair_bandwidth_bytes_distribution: DistributionSummary,
    /// Repair operation count distribution across samples.
    pub repair_operations_distribution: DistributionSummary,
    /// Mean repair operations per sample.
    pub mean_repair_operations: f64,
    /// Mean repair bandwidth in bytes/sec across samples.
    pub mean_repair_bandwidth_bps: f64,
    /// Peak failure-domain exposure distribution across samples.
    pub peak_domain_exposure_distribution: DistributionSummary,
    /// Combined deterministic 64-bit digest over all ordered sample trace digests.
    pub batch_trace_digest: u64,
    /// Combined deterministic 64-bit digest over all ordered exogenous failure traces.
    pub exogenous_trace_digest: u64,
}

impl MonteCarloBatchResult {
    /// Serializes the batch summary to a deterministic, machine-readable JSON string.
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                "{{",
                "\"strategy\":\"{}\",",
                "\"churn_scenario\":\"{}\",",
                "\"erasure_code\":{{\"n\":{},\"k\":{}}},",
                "\"nominal_storage_overhead\":{:.6},",
                "\"master_seed\":{},",
                "\"horizon_secs\":{:.3},",
                "\"requested_samples\":{},",
                "\"completed_samples\":{},",
                "\"failed_samples\":{},",
                "\"irrecoverable_loss_count\":{},",
                "\"right_censored_count\":{},",
                "\"loss_probability_95_ci\":{},",
                "\"survival_probability_95_ci\":{},",
                "\"rmst_secs\":{:.6},",
                "\"rmst_std_err_secs\":{:.6},",
                "\"unrestricted_mttdl\":{},",
                "\"analytical_no_repair_survival\":{:.6},",
                "\"availability\":{},",
                "\"measured_node_offline_fraction\":{},",
                "\"repair_bandwidth_bytes\":{},",
                "\"repair_operations\":{},",
                "\"mean_repair_bandwidth_bps\":{:.6},",
                "\"peak_domain_exposure\":{},",
                "\"exogenous_trace_digest\":\"0x{:016x}\",",
                "\"batch_trace_digest\":\"0x{:016x}\"",
                "}}"
            ),
            self.strategy_label,
            self.churn_label,
            self.erasure_code_n_k.0,
            self.erasure_code_n_k.1,
            self.nominal_storage_overhead,
            self.master_seed,
            self.horizon_secs,
            self.requested_samples,
            self.completed_samples,
            self.failed_samples,
            self.irrecoverable_loss_count,
            self.right_censored_count,
            self.loss_probability_ci.to_json(),
            self.survival_probability_ci.to_json(),
            self.kaplan_meier.rmst_secs,
            self.kaplan_meier.rmst_std_err_secs,
            self.kaplan_meier.unrestricted_mttdl_status.to_json(),
            self.analytical_no_repair_survival,
            self.availability_distribution.to_json(),
            self.node_offline_fraction_distribution.to_json(),
            self.repair_bandwidth_bytes_distribution.to_json(),
            self.repair_operations_distribution.to_json(),
            self.mean_repair_bandwidth_bps,
            self.peak_domain_exposure_distribution.to_json(),
            self.exogenous_trace_digest,
            self.batch_trace_digest,
        )
    }
}

struct SampleOutcome {
    sample_index: usize,
    exogenous_digest: u64,
    trace_digest: u64,
    loss_time_secs: Option<f64>,
    availability: f64,
    node_offline_fraction: f64,
    repair_bytes: f64,
    repair_bps: f64,
    repair_ops: f64,
    peak_domain_exposure: f64,
    analytical_survival: f64,
}

/// Executes a parallel Monte Carlo batch experiment with deterministic per-sample seeding
/// and order-invariant reduction.
pub fn run_monte_carlo_batch(
    batch_cfg: &MonteCarloBatchConfig,
) -> Result<MonteCarloBatchResult, ConfigError> {
    batch_cfg.base_config.validate()?;

    let num_samples = batch_cfg.num_samples;
    let workers = batch_cfg.worker_threads.max(1).min(num_samples.max(1));

    let mut outcomes: Vec<Option<SampleOutcome>> = Vec::with_capacity(num_samples);
    outcomes.resize_with(num_samples, || None);

    if workers <= 1 || num_samples <= 1 {
        for (idx, slot) in outcomes.iter_mut().enumerate() {
            *slot = run_single_sample(&batch_cfg.base_config, batch_cfg.strategy_kind, idx).ok();
        }
    } else {
        let chunk_size = num_samples.div_ceil(workers);
        thread::scope(|s| {
            let mut handles = Vec::new();
            for (chunk_idx, chunk_slice) in outcomes.chunks_mut(chunk_size).enumerate() {
                let start_idx = chunk_idx * chunk_size;
                let cfg_clone = batch_cfg.base_config.clone();
                let strat_kind = batch_cfg.strategy_kind;
                handles.push(s.spawn(move || {
                    for (offset, slot) in chunk_slice.iter_mut().enumerate() {
                        let sample_idx = start_idx + offset;
                        *slot = run_single_sample(&cfg_clone, strat_kind, sample_idx).ok();
                    }
                }));
            }
            for h in handles {
                let _ = h.join();
            }
        });
    }

    let mut completed_samples = 0usize;
    let mut failed_samples = 0usize;
    let mut loss_times: Vec<Option<f64>> = Vec::with_capacity(num_samples);
    let mut availabilities: Vec<f64> = Vec::with_capacity(num_samples);
    let mut offline_fractions: Vec<f64> = Vec::with_capacity(num_samples);
    let mut repair_bytes_vec: Vec<f64> = Vec::with_capacity(num_samples);
    let mut repair_bps_vec: Vec<f64> = Vec::with_capacity(num_samples);
    let mut repair_ops_vec: Vec<f64> = Vec::with_capacity(num_samples);
    let mut peak_exposures: Vec<f64> = Vec::with_capacity(num_samples);
    let mut analytical_survival = 1.0f64;

    let mut batch_trace_digest: u64 = 0xCBF2_9CE4_8422_2325;
    let mut exogenous_trace_digest: u64 = 0xCBF2_9CE4_8422_2325;

    for outcome_opt in outcomes {
        match outcome_opt {
            Some(out) => {
                completed_samples += 1;
                debug_assert_eq!(out.sample_index + 1, completed_samples + failed_samples);
                batch_trace_digest ^= out.trace_digest;
                batch_trace_digest = batch_trace_digest.wrapping_mul(0x0000_0100_0000_01B3);
                exogenous_trace_digest ^= out.exogenous_digest;
                exogenous_trace_digest = exogenous_trace_digest.wrapping_mul(0x0000_0100_0000_01B3);

                loss_times.push(out.loss_time_secs);
                availabilities.push(out.availability);
                offline_fractions.push(out.node_offline_fraction);
                repair_bytes_vec.push(out.repair_bytes);
                repair_bps_vec.push(out.repair_bps);
                repair_ops_vec.push(out.repair_ops);
                peak_exposures.push(out.peak_domain_exposure);
                analytical_survival = out.analytical_survival;
            }
            None => {
                failed_samples += 1;
            }
        }
    }

    let horizon_secs = batch_cfg.base_config.horizon_secs();
    let km = compute_kaplan_meier_and_rmst(&loss_times, horizon_secs);
    let loss_count = km.observed_failures;
    let right_censored = km.right_censored_samples;

    let loss_probability_ci = wilson_score_interval_95(loss_count, completed_samples);
    let survival_probability_ci = wilson_score_interval_95(right_censored, completed_samples);

    let availability_distribution = DistributionSummary::from_slice(&availabilities);
    let node_offline_fraction_distribution = DistributionSummary::from_slice(&offline_fractions);
    let repair_bandwidth_bytes_distribution = DistributionSummary::from_slice(&repair_bytes_vec);
    let repair_operations_distribution = DistributionSummary::from_slice(&repair_ops_vec);
    let peak_domain_exposure_distribution = DistributionSummary::from_slice(&peak_exposures);

    let mean_repair_bandwidth_bps = if repair_bps_vec.is_empty() {
        0.0
    } else {
        repair_bps_vec.iter().sum::<f64>() / (repair_bps_vec.len() as f64)
    };

    Ok(MonteCarloBatchResult {
        strategy_label: batch_cfg.strategy_kind.label(),
        churn_label: batch_cfg.base_config.churn_scenario.label(),
        erasure_code_n_k: (
            batch_cfg.base_config.erasure_code.total_shards,
            batch_cfg.base_config.erasure_code.recovery_threshold,
        ),
        nominal_storage_overhead: batch_cfg
            .base_config
            .erasure_code
            .nominal_storage_overhead(),
        master_seed: batch_cfg.base_config.seed,
        horizon_secs,
        requested_samples: num_samples,
        completed_samples,
        failed_samples,
        irrecoverable_loss_count: loss_count,
        right_censored_count: right_censored,
        loss_probability_ci,
        survival_probability_ci,
        kaplan_meier: km,
        analytical_no_repair_survival: analytical_survival,
        availability_distribution,
        node_offline_fraction_distribution,
        repair_bandwidth_bytes_distribution,
        mean_repair_operations: repair_operations_distribution.mean,
        repair_operations_distribution,
        mean_repair_bandwidth_bps,
        peak_domain_exposure_distribution,
        batch_trace_digest,
        exogenous_trace_digest,
    })
}

fn run_single_sample(
    base_config: &SimulationConfig,
    strategy_kind: StrategyKind,
    sample_index: usize,
) -> Result<SampleOutcome, ConfigError> {
    let trace = generate_exogenous_failure_trace(base_config, sample_index);
    let exogenous_digest = trace.trace_digest;
    let strategy = strategy_kind.build_strategy();
    let mut engine =
        SimulationEngine::new_with_exogenous_trace(base_config.clone(), strategy, trace)?;
    let report = engine.run()?;

    Ok(SampleOutcome {
        sample_index,
        exogenous_digest,
        trace_digest: report.event_trace_digest,
        loss_time_secs: report.metrics.data_loss_time_secs,
        availability: report.metrics.availability_empirical,
        node_offline_fraction: report.metrics.measured_node_offline_fraction,
        repair_bytes: report.metrics.repair_bandwidth_bytes as f64,
        repair_bps: report.metrics.mean_repair_bandwidth_bytes_per_sec,
        repair_ops: report.metrics.repair_operation_count as f64,
        peak_domain_exposure: report.metrics.peak_failure_domain_exposure,
        analytical_survival: report.metrics.analytical_horizon_survival_no_repair,
    })
}

/// Paired comparison summary between Strategy A and Strategy B across identical exogenous traces.
#[derive(Debug, Clone, PartialEq)]
pub struct PairedStrategyComparison {
    /// Label of Strategy A.
    pub strategy_a: String,
    /// Label of Strategy B.
    pub strategy_b: String,
    /// Churn scenario label.
    pub churn_label: String,
    /// Number of paired Monte Carlo trajectories evaluated.
    pub sample_pairs: usize,
    /// Paired 95% CI on difference in finite-horizon data-loss indicator ($L_{A, i} - L_{B, i}$).
    pub loss_probability_diff_ci: crate::reliability::statistics::PairedDifferenceCi95,
    /// Paired 95% CI on difference in mean repair bandwidth bps ($B_{A, i} - B_{B, i}$).
    pub repair_bandwidth_bps_diff_ci: crate::reliability::statistics::PairedDifferenceCi95,
    /// Paired 95% CI on difference in time-weighted availability ($A_{A, i} - A_{B, i}$).
    pub availability_diff_ci: crate::reliability::statistics::PairedDifferenceCi95,
    /// Number of trajectories where Strategy A survived and Strategy B lost data.
    pub a_survived_b_lost: usize,
    /// Number of trajectories where Strategy B survived and Strategy A lost data.
    pub b_survived_a_lost: usize,
}

impl PairedStrategyComparison {
    /// Formats the paired comparison as a deterministic JSON object string.
    pub fn to_json(&self) -> String {
        format!(
            concat!(
                "{{",
                "\"strategy_a\":\"{}\",",
                "\"strategy_b\":\"{}\",",
                "\"churn_scenario\":\"{}\",",
                "\"sample_pairs\":{},",
                "\"a_survived_b_lost\":{},",
                "\"b_survived_a_lost\":{},",
                "\"loss_probability_diff_95_ci\":{},",
                "\"repair_bandwidth_bps_diff_95_ci\":{},",
                "\"availability_diff_95_ci\":{}",
                "}}"
            ),
            self.strategy_a,
            self.strategy_b,
            self.churn_label,
            self.sample_pairs,
            self.a_survived_b_lost,
            self.b_survived_a_lost,
            self.loss_probability_diff_ci.to_json(),
            self.repair_bandwidth_bps_diff_ci.to_json(),
            self.availability_diff_ci.to_json()
        )
    }
}

/// Runs two strategies across the exact same `num_samples` exogenous failure traces and computes
/// paired 95% confidence intervals on their differences.
pub fn run_paired_monte_carlo_comparison(
    base_config: &SimulationConfig,
    strategy_a: StrategyKind,
    strategy_b: StrategyKind,
    num_samples: usize,
    worker_threads: usize,
) -> Result<PairedStrategyComparison, ConfigError> {
    base_config.validate()?;
    let workers = worker_threads.max(1).min(num_samples.max(1));

    let mut pairs: Vec<Option<(SampleOutcome, SampleOutcome)>> = Vec::with_capacity(num_samples);
    pairs.resize_with(num_samples, || None);

    let chunk_size = num_samples.div_ceil(workers);
    thread::scope(|s| {
        let mut handles = Vec::new();
        for (chunk_idx, chunk_slice) in pairs.chunks_mut(chunk_size).enumerate() {
            let start_idx = chunk_idx * chunk_size;
            let cfg_clone = base_config.clone();
            handles.push(s.spawn(move || {
                for (offset, slot) in chunk_slice.iter_mut().enumerate() {
                    let idx = start_idx + offset;
                    if let (Ok(out_a), Ok(out_b)) = (
                        run_single_sample(&cfg_clone, strategy_a, idx),
                        run_single_sample(&cfg_clone, strategy_b, idx),
                    ) {
                        *slot = Some((out_a, out_b));
                    }
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }
    });

    let mut losses_a = Vec::with_capacity(num_samples);
    let mut losses_b = Vec::with_capacity(num_samples);
    let mut bw_a = Vec::with_capacity(num_samples);
    let mut bw_b = Vec::with_capacity(num_samples);
    let mut avail_a = Vec::with_capacity(num_samples);
    let mut avail_b = Vec::with_capacity(num_samples);
    let mut a_survived_b_lost = 0usize;
    let mut b_survived_a_lost = 0usize;

    for (out_a, out_b) in pairs.into_iter().flatten() {
        let la = if out_a.loss_time_secs.is_some() {
            1.0
        } else {
            0.0
        };
        let lb = if out_b.loss_time_secs.is_some() {
            1.0
        } else {
            0.0
        };
        if la == 0.0 && lb == 1.0 {
            a_survived_b_lost += 1;
        } else if lb == 0.0 && la == 1.0 {
            b_survived_a_lost += 1;
        }
        losses_a.push(la);
        losses_b.push(lb);
        bw_a.push(out_a.repair_bps);
        bw_b.push(out_b.repair_bps);
        avail_a.push(out_a.availability);
        avail_b.push(out_b.availability);
    }

    Ok(PairedStrategyComparison {
        strategy_a: strategy_a.label(),
        strategy_b: strategy_b.label(),
        churn_label: base_config.churn_scenario.label(),
        sample_pairs: losses_a.len(),
        loss_probability_diff_ci: crate::reliability::statistics::paired_difference_ci_95(
            &losses_a, &losses_b,
        ),
        repair_bandwidth_bps_diff_ci: crate::reliability::statistics::paired_difference_ci_95(
            &bw_a, &bw_b,
        ),
        availability_diff_ci: crate::reliability::statistics::paired_difference_ci_95(
            &avail_a, &avail_b,
        ),
        a_survived_b_lost,
        b_survived_a_lost,
    })
}
