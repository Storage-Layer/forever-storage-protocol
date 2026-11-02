//! Configuration and parameter validation for the FSP Phase 0 & Phase 1 simulator.

pub mod loader;
pub mod validation;

use crate::coding::LrcConfig;
use crate::detection::FailureDetectionConfig;
use crate::reliability::distributions::FailureDistribution;
use std::fmt;

/// Standard epoch duration in seconds (24 hours = 86,400 seconds).
pub const SECONDS_PER_EPOCH: f64 = 86_400.0;

/// Errors arising from invalid simulation or erasure-coding parameters.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    /// Total shards `n` and recovery threshold `k` violate `1 <= k <= n`.
    InvalidErasureCode {
        total_shards: usize,
        recovery_threshold: usize,
    },
    /// Cluster has fewer storage nodes than total shards `n`.
    InsufficientNodes {
        node_count: usize,
        required_shards: usize,
    },
    /// Failure domain capacity cannot hold `n` shards (`num_domains * max_shards_per_domain < n`).
    InsufficientFailureDomainCapacity {
        num_domains: usize,
        max_shards_per_domain: usize,
        total_shards: usize,
    },
    /// Invalid probability or rate parameter.
    InvalidRate { field: &'static str, value: f64 },
    /// Invalid bandwidth or horizon parameter.
    InvalidResourceParameter { field: &'static str },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidErasureCode {
                total_shards,
                recovery_threshold,
            } => write!(
                f,
                "invalid erasure code: total_shards={} must be >= recovery_threshold={} >= 1",
                total_shards, recovery_threshold
            ),
            Self::InsufficientNodes {
                node_count,
                required_shards,
            } => write!(
                f,
                "insufficient storage nodes: node_count={} < required_shards={}",
                node_count, required_shards
            ),
            Self::InsufficientFailureDomainCapacity {
                num_domains,
                max_shards_per_domain,
                total_shards,
            } => write!(
                f,
                "insufficient failure domain capacity: {} domains * {} max_shards < {} total_shards",
                num_domains, max_shards_per_domain, total_shards
            ),
            Self::InvalidRate { field, value } => {
                write!(f, "invalid rate/probability for {}: {}", field, value)
            }
            Self::InvalidResourceParameter { field } => {
                write!(f, "invalid resource parameter: {}", field)
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Logical erasure coding or replication configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErasureCodeConfig {
    /// Total distinct shards $n$.
    pub total_shards: usize,
    /// Minimum distinct valid shards required for reconstruction $k$.
    pub recovery_threshold: usize,
}

impl ErasureCodeConfig {
    /// Creates a Reed–Solomon $(n, k)$ configuration.
    pub const fn reed_solomon(total_shards: usize, recovery_threshold: usize) -> Self {
        Self {
            total_shards,
            recovery_threshold,
        }
    }

    /// Creates an $R$-way replication baseline ($n = R, k = 1$).
    pub const fn replication(replicas: usize) -> Self {
        Self {
            total_shards: replicas,
            recovery_threshold: 1,
        }
    }

    /// Nominal storage overhead ratio $\Omega = n / k$.
    pub fn nominal_storage_overhead(&self) -> f64 {
        self.total_shards as f64 / self.recovery_threshold as f64
    }

    /// Maximum tolerated unavailable/lost shards before reconstruction fails ($n - k$).
    pub const fn max_tolerated_failures(&self) -> usize {
        self.total_shards.saturating_sub(self.recovery_threshold)
    }
}

/// Canonical churn scenarios specified in FSP Phase 0 & Phase 1.
///
/// Each scenario defines the per-epoch (24h) per-node probability $p_{\text{churn}}$
/// that an online node enters a temporary outage (`NodeOutage`), without conflating
/// transient unavailability with permanent shard deletion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChurnScenario {
    /// 0% transient churn (baseline control).
    Zero,
    /// 1% per-node per-epoch transient outage probability.
    Percent1,
    /// 5% per-node per-epoch transient outage probability.
    Percent5,
    /// 10% per-node per-epoch transient outage probability.
    Percent10,
    /// 20% per-node per-epoch transient outage probability.
    Percent20,
    /// Custom per-node per-epoch transient outage probability in $[0.0, 1.0)$.
    Custom(f64),
}

impl ChurnScenario {
    /// Returns the per-epoch (24h) transient outage probability $p_{\text{churn}} \in [0.0, 1.0)$.
    pub fn transient_outage_probability_per_epoch(&self) -> f64 {
        match *self {
            Self::Zero => 0.0,
            Self::Percent1 => 0.01,
            Self::Percent5 => 0.05,
            Self::Percent10 => 0.10,
            Self::Percent20 => 0.20,
            Self::Custom(p) => p,
        }
    }

    /// Returns a human-readable label for reports.
    pub fn label(&self) -> String {
        match *self {
            Self::Zero => "0%".to_string(),
            Self::Percent1 => "1%".to_string(),
            Self::Percent5 => "5%".to_string(),
            Self::Percent10 => "10%".to_string(),
            Self::Percent20 => "20%".to_string(),
            Self::Custom(p) => format!("{:.2}%", p * 100.0),
        }
    }
}

/// Probability distribution for temporary node outage durations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutageDurationDistribution {
    /// Exponential distribution with mean `mean_secs`.
    Exponential { mean_secs: f64 },
    /// Log-normal distribution parameterized by median duration `median_secs` and log-space std dev `sigma`.
    LogNormal { median_secs: f64, sigma: f64 },
    /// Deterministic fixed duration `duration_secs`.
    Fixed { duration_secs: f64 },
    /// Uniform distribution on `[min_secs, max_secs]`.
    Uniform { min_secs: f64, max_secs: f64 },
}

impl OutageDurationDistribution {
    /// Returns the expected mean duration in seconds.
    pub fn mean_secs(&self) -> f64 {
        match *self {
            Self::Exponential { mean_secs } => mean_secs,
            Self::LogNormal { median_secs, sigma } => median_secs * (0.5 * sigma * sigma).exp(),
            Self::Fixed { duration_secs } => duration_secs,
            Self::Uniform { min_secs, max_secs } => 0.5 * (min_secs + max_secs),
        }
    }

    /// Returns true if the distribution parameters are valid and positive.
    pub fn is_valid(&self) -> bool {
        match *self {
            Self::Exponential { mean_secs } => mean_secs > 0.0 && mean_secs.is_finite(),
            Self::LogNormal { median_secs, sigma } => {
                median_secs > 0.0 && median_secs.is_finite() && sigma >= 0.0 && sigma.is_finite()
            }
            Self::Fixed { duration_secs } => duration_secs > 0.0 && duration_secs.is_finite(),
            Self::Uniform { min_secs, max_secs } => {
                min_secs > 0.0
                    && max_secs >= min_secs
                    && min_secs.is_finite()
                    && max_secs.is_finite()
            }
        }
    }
}

/// Configuration for multi-node correlated burst failures.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BurstFailureConfig {
    /// Per-epoch probability of a multi-node burst failure event in $[0.0, 1.0)$.
    pub probability_per_epoch: f64,
    /// Number of distinct nodes simultaneously hit by the burst.
    pub affected_nodes: usize,
    /// If `true`, burst causes permanent node failures; if `false`, temporary outages.
    pub is_permanent: bool,
    /// Outage duration in seconds when `is_permanent == false`.
    pub outage_duration_secs: f64,
}

impl Default for BurstFailureConfig {
    fn default() -> Self {
        Self {
            probability_per_epoch: 0.0,
            affected_nodes: 0,
            is_permanent: false,
            outage_duration_secs: 7_200.0,
        }
    }
}

/// Complete, self-contained configuration for a deterministic simulation run.
#[derive(Debug, Clone, PartialEq)]
pub struct SimulationConfig {
    /// Total number of storage nodes in the cluster (default: 50).
    pub num_nodes: usize,
    /// Erasure coding $(n, k)$ parameters (default: RS(20, 12)).
    pub erasure_code: ErasureCodeConfig,
    /// Number of distinct failure domains (default: 10).
    pub num_failure_domains: usize,
    /// Maximum shards of the object allowed in any single failure domain (default: 2).
    pub max_shards_per_domain: usize,
    /// Transient node churn scenario (1%, 5%, 10%, 20%).
    pub churn_scenario: ChurnScenario,
    /// Mean duration of a temporary node outage in seconds (default: 4 hours = 14,400s).
    pub mean_outage_duration_secs: f64,
    /// Explicit distribution for transient node outage durations.
    pub outage_distribution: OutageDurationDistribution,
    /// Independent per-node per-epoch probability of permanent storage failure.
    pub permanent_failure_rate_per_epoch: f64,
    /// Correlated per-domain per-epoch transient outage probability (default: 0.0).
    pub domain_outage_rate_per_epoch: f64,
    /// Mean duration of a correlated domain-wide transient outage in seconds (default: 7,200s).
    pub domain_outage_duration_secs: f64,
    /// Correlated per-domain per-epoch permanent disaster probability (default: 0.0).
    pub correlated_domain_failure_rate_per_epoch: f64,
    /// Multi-node correlated burst failure parameters.
    pub burst_failure: BurstFailureConfig,
    /// Logical object size in bytes (default: 120 MB -> 10 MB per shard for k=12).
    pub object_size_bytes: u64,
    /// Maximum cluster repair bandwidth in bytes/sec (default: 10 MB/s).
    pub repair_bandwidth_bytes_per_sec: u64,
    /// Base coordination and verification latency per repair batch in seconds (default: 2.0s).
    pub repair_base_latency_secs: f64,
    /// Interval between periodic repair controller ticks in seconds (default: 3600s = 1 hour).
    pub controller_check_interval_secs: f64,
    /// Duration in seconds a node must be offline before a strategy may suspect it (default: 12 hours).
    pub unreachable_suspect_timeout_secs: f64,
    /// Simulation time horizon in epochs (days) (default: 30 epochs).
    pub horizon_epochs: u32,
    /// Deterministic PRNG seed.
    pub seed: u64,
    /// Optional Phase 2 blind failure detection configuration (`None` = Phase 1 oracle mode).
    pub blind_detection: Option<FailureDetectionConfig>,
    /// Optional Phase 2 Local Reconstruction Code configuration (`None` = standard RS(n, k)).
    pub lrc_config: Option<LrcConfig>,
    /// Optional Phase 2 non-stationary node failure distribution (`None` = stationary per-epoch rate).
    pub non_stationary_failure_dist: Option<FailureDistribution>,
}

impl SimulationConfig {
    /// Canonical default configuration:
    /// 50 storage nodes, Reed–Solomon (20, 12), 10 failure domains (max 2 shards/domain).
    pub fn default_rs_20_12() -> Self {
        Self {
            num_nodes: 50,
            erasure_code: ErasureCodeConfig::reed_solomon(20, 12),
            num_failure_domains: 10,
            max_shards_per_domain: 2,
            churn_scenario: ChurnScenario::Percent5,
            mean_outage_duration_secs: 14_400.0,
            outage_distribution: OutageDurationDistribution::Exponential {
                mean_secs: 14_400.0,
            },
            permanent_failure_rate_per_epoch: 0.002,
            domain_outage_rate_per_epoch: 0.0,
            domain_outage_duration_secs: 7_200.0,
            correlated_domain_failure_rate_per_epoch: 0.0,
            burst_failure: BurstFailureConfig::default(),
            object_size_bytes: 120_000_000,
            repair_bandwidth_bytes_per_sec: 10_000_000,
            repair_base_latency_secs: 2.0,
            controller_check_interval_secs: 3_600.0,
            unreachable_suspect_timeout_secs: 43_200.0,
            horizon_epochs: 30,
            seed: 0xF590_0000_0000_0001,
            blind_detection: None,
            lrc_config: None,
            non_stationary_failure_dist: None,
        }
    }

    /// Validates all mathematical and structural constraints.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.erasure_code.recovery_threshold == 0
            || self.erasure_code.total_shards < self.erasure_code.recovery_threshold
        {
            return Err(ConfigError::InvalidErasureCode {
                total_shards: self.erasure_code.total_shards,
                recovery_threshold: self.erasure_code.recovery_threshold,
            });
        }
        if self.num_nodes < self.erasure_code.total_shards {
            return Err(ConfigError::InsufficientNodes {
                node_count: self.num_nodes,
                required_shards: self.erasure_code.total_shards,
            });
        }
        if self.num_failure_domains == 0
            || self
                .num_failure_domains
                .saturating_mul(self.max_shards_per_domain)
                < self.erasure_code.total_shards
        {
            return Err(ConfigError::InsufficientFailureDomainCapacity {
                num_domains: self.num_failure_domains,
                max_shards_per_domain: self.max_shards_per_domain,
                total_shards: self.erasure_code.total_shards,
            });
        }
        let p_churn = self.churn_scenario.transient_outage_probability_per_epoch();
        if !(0.0..1.0).contains(&p_churn) || p_churn.is_nan() {
            return Err(ConfigError::InvalidRate {
                field: "churn_scenario",
                value: p_churn,
            });
        }
        if !(0.0..1.0).contains(&self.permanent_failure_rate_per_epoch)
            || self.permanent_failure_rate_per_epoch.is_nan()
        {
            return Err(ConfigError::InvalidRate {
                field: "permanent_failure_rate_per_epoch",
                value: self.permanent_failure_rate_per_epoch,
            });
        }
        if !(0.0..1.0).contains(&self.domain_outage_rate_per_epoch)
            || self.domain_outage_rate_per_epoch.is_nan()
        {
            return Err(ConfigError::InvalidRate {
                field: "domain_outage_rate_per_epoch",
                value: self.domain_outage_rate_per_epoch,
            });
        }
        if !(0.0..1.0).contains(&self.correlated_domain_failure_rate_per_epoch)
            || self.correlated_domain_failure_rate_per_epoch.is_nan()
        {
            return Err(ConfigError::InvalidRate {
                field: "correlated_domain_failure_rate_per_epoch",
                value: self.correlated_domain_failure_rate_per_epoch,
            });
        }
        if !(0.0..1.0).contains(&self.burst_failure.probability_per_epoch)
            || self.burst_failure.probability_per_epoch.is_nan()
        {
            return Err(ConfigError::InvalidRate {
                field: "burst_failure.probability_per_epoch",
                value: self.burst_failure.probability_per_epoch,
            });
        }
        if self.repair_bandwidth_bytes_per_sec == 0
            || self.object_size_bytes == 0
            || self.horizon_epochs == 0
            || self.mean_outage_duration_secs <= 0.0
            || !self.outage_distribution.is_valid()
        {
            return Err(ConfigError::InvalidResourceParameter {
                field: "bandwidth, object_size, horizon, or outage_distribution",
            });
        }
        Ok(())
    }

    /// Size of a single encoded shard in bytes: $\lceil S_{\text{obj}} / k \rceil$.
    pub fn shard_size_bytes(&self) -> u64 {
        let k = self.erasure_code.recovery_threshold as u64;
        self.object_size_bytes.div_ceil(k)
    }

    /// Total simulation horizon in seconds.
    pub fn horizon_secs(&self) -> f64 {
        f64::from(self.horizon_epochs) * SECONDS_PER_EPOCH
    }

    /// Builder: set PRNG seed.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Builder: set erasure code configuration and adjust domain cap if needed.
    pub fn with_erasure_code(mut self, ec: ErasureCodeConfig) -> Self {
        self.erasure_code = ec;
        let min_per_domain = ec.total_shards.div_ceil(self.num_failure_domains.max(1));
        if self.max_shards_per_domain < min_per_domain {
            self.max_shards_per_domain = min_per_domain;
        }
        self
    }

    /// Builder: set failure domains and max shards per domain.
    pub fn with_failure_domains(
        mut self,
        num_failure_domains: usize,
        max_shards_per_domain: usize,
    ) -> Self {
        self.num_failure_domains = num_failure_domains;
        self.max_shards_per_domain = max_shards_per_domain;
        self
    }

    /// Builder: set transient churn scenario.
    pub fn with_churn_scenario(mut self, scenario: ChurnScenario) -> Self {
        self.churn_scenario = scenario;
        self
    }

    /// Builder: set outage duration distribution.
    pub fn with_outage_distribution(mut self, dist: OutageDurationDistribution) -> Self {
        self.mean_outage_duration_secs = dist.mean_secs();
        self.outage_distribution = dist;
        self
    }

    /// Builder: set permanent node failure rate per epoch.
    pub fn with_permanent_failure_rate_per_epoch(mut self, rate: f64) -> Self {
        self.permanent_failure_rate_per_epoch = rate;
        self
    }

    /// Builder: set correlated domain transient outage rate and duration.
    pub fn with_domain_outage_rate_per_epoch(mut self, rate: f64, duration_secs: f64) -> Self {
        self.domain_outage_rate_per_epoch = rate;
        self.domain_outage_duration_secs = duration_secs;
        self
    }

    /// Builder: set correlated domain permanent failure rate per epoch.
    pub fn with_correlated_domain_failure_rate_per_epoch(mut self, rate: f64) -> Self {
        self.correlated_domain_failure_rate_per_epoch = rate;
        self
    }

    /// Builder: set multi-node burst failure parameters.
    pub fn with_burst_failure(mut self, burst: BurstFailureConfig) -> Self {
        self.burst_failure = burst;
        self
    }

    /// Builder: set horizon in epochs.
    pub fn with_horizon_epochs(mut self, epochs: u32) -> Self {
        self.horizon_epochs = epochs;
        self
    }

    /// Builder: set logical object size in bytes.
    pub fn with_object_size_bytes(mut self, bytes: u64) -> Self {
        self.object_size_bytes = bytes;
        self
    }

    /// Builder: set repair bandwidth cap in bytes/sec.
    pub fn with_repair_bandwidth_bytes_per_sec(mut self, bps: u64) -> Self {
        self.repair_bandwidth_bytes_per_sec = bps;
        self
    }

    /// Builder: set base repair latency in seconds.
    pub fn with_repair_base_latency_secs(mut self, secs: f64) -> Self {
        self.repair_base_latency_secs = secs;
        self
    }

    /// Builder: set unreachable suspect timeout in seconds.
    pub fn with_unreachable_suspect_timeout_secs(mut self, secs: f64) -> Self {
        self.unreachable_suspect_timeout_secs = secs;
        self
    }

    /// Builder: enable Phase 2 blind failure detection and node replacement delay.
    pub fn with_blind_detection(mut self, det: FailureDetectionConfig) -> Self {
        self.blind_detection = Some(det);
        self
    }

    /// Builder: enable Phase 2 Local Reconstruction Code (LRC) repair cost & decodability.
    pub fn with_lrc(mut self, lrc: LrcConfig) -> Self {
        self.erasure_code = ErasureCodeConfig::reed_solomon(lrc.total_shards, lrc.data_shards);
        let min_per_domain = lrc.total_shards.div_ceil(self.num_failure_domains.max(1));
        if self.max_shards_per_domain < min_per_domain {
            self.max_shards_per_domain = min_per_domain;
        }
        self.lrc_config = Some(lrc);
        self
    }

    /// Builder: enable Phase 2 non-stationary node failure distribution (e.g., Weibull).
    pub fn with_non_stationary_failure_dist(mut self, dist: FailureDistribution) -> Self {
        self.non_stationary_failure_dist = Some(dist);
        self
    }
}
