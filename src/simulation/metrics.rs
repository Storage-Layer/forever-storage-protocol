//! Observability metrics, Monte Carlo empirical summaries, and machine-readable output.

use crate::reliability::survival::{EmpiricalMttdlResult, SurvivalCurvePoint};

/// Complete set of simulation observability metrics for a single run or aggregated batch.
#[derive(Debug, Clone, PartialEq)]
pub struct SimulationMetrics {
    /// Whether irrecoverable data loss ($|\mathcal{U}(t)| < k$) occurred during the horizon.
    pub irrecoverable_data_loss: bool,
    /// Timestamp $t$ (seconds) when irrecoverable data loss first occurred, if any.
    pub data_loss_time_secs: Option<f64>,
    /// Empirical data loss probability $\hat{P}_{\text{loss}}(T_{\max}) \in [0.0, 1.0]$.
    pub data_loss_probability_empirical: f64,
    /// Time-averaged empirical data availability $\hat{A}(T_{\max}) \in [0.0, 1.0]$.
    pub availability_empirical: f64,
    /// Time-weighted fraction of cluster node-time spent offline across $[0, T_{\max}]$ in $[0.0, 1.0]$.
    pub measured_node_offline_fraction: f64,
    /// Guarded empirical MTTDL status (explicitly `RightCensoredZeroLosses` when no loss occurs).
    pub empirical_mttdl: EmpiricalMttdlResult,
    /// Reference analytical Markov MTTDL approximation in seconds (independent failure model).
    pub analytical_markov_mttdl_secs: Option<f64>,
    /// Analytical no-repair survival probability across the full horizon.
    pub analytical_horizon_survival_no_repair: f64,
    /// Total repair bandwidth consumed (bytes read + written).
    pub repair_bandwidth_bytes: u64,
    /// Average repair bandwidth in bytes/sec across the simulation horizon.
    pub mean_repair_bandwidth_bytes_per_sec: f64,
    /// Total number of completed shard repair operations.
    pub repair_operation_count: u64,
    /// Total number of aborted/failed repair attempts (e.g., due to unavailability at completion).
    pub aborted_repair_count: u64,
    /// Mean repair latency in seconds across completed repairs ($0.0$ if $0$ repairs).
    pub mean_repair_latency_secs: f64,
    /// Nominal storage overhead $\Omega = n / k$.
    pub nominal_storage_overhead: f64,
    /// Final normalized failure-domain exposure ratio $E_{\text{domain}}(T_{\max}) \in [0.0, 1.0]$.
    pub failure_domain_exposure: f64,
    /// Peak normalized failure-domain exposure ratio observed during the run.
    pub peak_failure_domain_exposure: f64,
    /// Time-series survival curve across epoch checkpoints.
    pub survival_curve: Vec<SurvivalCurvePoint>,
}

/// Complete, reproducible report emitted by `SimulationEngine::run()`.
#[derive(Debug, Clone, PartialEq)]
pub struct SimulationReport {
    /// Name of the strategy evaluated.
    pub strategy_name: String,
    /// Churn scenario label (`0%`, `1%`, `5%`, `10%`, `20%`).
    pub churn_scenario_label: String,
    /// Seed used for deterministic execution.
    pub seed: u64,
    /// Total simulated horizon in seconds.
    pub horizon_secs: f64,
    /// Total discrete events processed.
    pub events_processed: u64,
    /// Deterministic 64-bit FNV-1a digest of the executed event trace.
    pub event_trace_digest: u64,
    /// Computed observability metrics.
    pub metrics: SimulationMetrics,
}

impl SimulationReport {
    /// Serializes the report to a deterministic, machine-readable JSON string without external dependencies.
    pub fn to_json(&self) -> String {
        let loss_time_str = match self.metrics.data_loss_time_secs {
            Some(t) => format!("{:.6}", t),
            None => "null".to_string(),
        };
        let empirical_mttdl_json = match &self.metrics.empirical_mttdl {
            EmpiricalMttdlResult::RightCensoredZeroLosses => {
                r#"{"status":"RightCensoredZeroLosses","mttdl_secs":null}"#.to_string()
            }
            EmpiricalMttdlResult::Estimated {
                mttdl_secs,
                observed_losses,
                total_runs,
            } => format!(
                r#"{{"status":"Estimated","mttdl_secs":{:.6},"observed_losses":{},"total_runs":{}}}"#,
                mttdl_secs, observed_losses, total_runs
            ),
        };
        let analytical_mttdl_str = match self.metrics.analytical_markov_mttdl_secs {
            Some(v) if v.is_finite() => format!("{:.6}", v),
            _ => "null".to_string(),
        };

        format!(
            concat!(
                "{{",
                "\"strategy_name\":\"{}\",",
                "\"churn_scenario\":\"{}\",",
                "\"seed\":{},",
                "\"horizon_secs\":{:.3},",
                "\"events_processed\":{},",
                "\"event_trace_digest\":\"0x{:016x}\",",
                "\"metrics\":{{",
                "\"irrecoverable_data_loss\":{},",
                "\"data_loss_time_secs\":{},",
                "\"data_loss_probability_empirical\":{:.6},",
                "\"availability_empirical\":{:.6},",
                "\"measured_node_offline_fraction\":{:.6},",
                "\"empirical_mttdl\":{},",
                "\"analytical_markov_mttdl_secs\":{},",
                "\"analytical_horizon_survival_no_repair\":{:.6},",
                "\"repair_bandwidth_bytes\":{},",
                "\"mean_repair_bandwidth_bytes_per_sec\":{:.6},",
                "\"repair_operation_count\":{},",
                "\"aborted_repair_count\":{},",
                "\"mean_repair_latency_secs\":{:.6},",
                "\"nominal_storage_overhead\":{:.6},",
                "\"failure_domain_exposure\":{:.6},",
                "\"peak_failure_domain_exposure\":{:.6}",
                "}}",
                "}}"
            ),
            self.strategy_name,
            self.churn_scenario_label,
            self.seed,
            self.horizon_secs,
            self.events_processed,
            self.event_trace_digest,
            self.metrics.irrecoverable_data_loss,
            loss_time_str,
            self.metrics.data_loss_probability_empirical,
            self.metrics.availability_empirical,
            self.metrics.measured_node_offline_fraction,
            empirical_mttdl_json,
            analytical_mttdl_str,
            self.metrics.analytical_horizon_survival_no_repair,
            self.metrics.repair_bandwidth_bytes,
            self.metrics.mean_repair_bandwidth_bytes_per_sec,
            self.metrics.repair_operation_count,
            self.metrics.aborted_repair_count,
            self.metrics.mean_repair_latency_secs,
            self.metrics.nominal_storage_overhead,
            self.metrics.failure_domain_exposure,
            self.metrics.peak_failure_domain_exposure,
        )
    }
}
