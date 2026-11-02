//! Statistical estimators for Monte Carlo reliability validation:
//! - Wilson score 95% confidence intervals for finite-horizon data loss probability
//! - Kaplan–Meier product-limit survival curve for right-censored observations
//! - Restricted Mean Survival Time (RMST) over $[0, T_{\max}]$
//! - Guarded unrestricted MTTDL identifiability status

/// 95% two-sided normal quantile $z_{0.975}$.
pub const Z_95: f64 = 1.959_963_984_540_054;

/// Confidence interval for a binomial proportion (e.g., data loss probability or survival probability).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfidenceInterval95 {
    /// Empirical point estimate $\hat{p} = k / n$.
    pub point_estimate: f64,
    /// Lower 95% confidence bound in $[0.0, 1.0]$.
    pub lower_bound: f64,
    /// Upper 95% confidence bound in $[0.0, 1.0]$.
    pub upper_bound: f64,
    /// Confidence level ($0.95$).
    pub confidence_level: f64,
    /// Sample count $n$.
    pub sample_count: usize,
}

impl ConfidenceInterval95 {
    /// Formats the confidence interval as a JSON object string.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"point":{:.6},"lower_95":{:.6},"upper_95":{:.6},"confidence_level":{:.2},"sample_count":{}}}"#,
            self.point_estimate,
            self.lower_bound,
            self.upper_bound,
            self.confidence_level,
            self.sample_count
        )
    }
}

/// Computes the Wilson score 95% confidence interval for a binomial proportion $k / n$.
///
/// Remains strictly within $[0.0, 1.0]$ and yields valid non-degenerate bounds when $k = 0$ or $k = n$.
pub fn wilson_score_interval_95(successes: usize, total: usize) -> ConfidenceInterval95 {
    if total == 0 {
        return ConfidenceInterval95 {
            point_estimate: 0.0,
            lower_bound: 0.0,
            upper_bound: 1.0,
            confidence_level: 0.95,
            sample_count: 0,
        };
    }

    let n = total as f64;
    let p_hat = (successes.min(total) as f64) / n;
    let z = Z_95;
    let z2 = z * z;

    let denom = 1.0 + z2 / n;
    let center = (p_hat + z2 / (2.0 * n)) / denom;
    let half_width = (z * ((p_hat * (1.0 - p_hat) / n) + (z2 / (4.0 * n * n))).sqrt()) / denom;

    let lower_bound = if successes == 0 {
        0.0
    } else {
        (center - half_width).clamp(0.0, 1.0)
    };
    let upper_bound = if successes >= total {
        1.0
    } else {
        (center + half_width).clamp(0.0, 1.0)
    };

    ConfidenceInterval95 {
        point_estimate: p_hat,
        lower_bound,
        upper_bound,
        confidence_level: 0.95,
        sample_count: total,
    }
}

/// A step point on the non-parametric Kaplan–Meier survival curve.
#[derive(Debug, Clone, PartialEq)]
pub struct KaplanMeierStep {
    /// Simulation time $t$ in seconds.
    pub time_secs: f64,
    /// Number of samples at risk just prior to $t$.
    pub at_risk: usize,
    /// Number of data loss events occurring at $t$.
    pub failures_at_t: usize,
    /// Kaplan–Meier survival estimate $\hat{S}_{\text{KM}}(t) \in [0.0, 1.0]$.
    pub survival_probability: f64,
    /// Greenwood standard error of $\hat{S}_{\text{KM}}(t)$.
    pub greenwood_std_err: f64,
}

/// Explicit statistical identifiability status for unrestricted MTTDL.
#[derive(Debug, Clone, PartialEq)]
pub enum UnrestrictedMttdlStatus {
    /// Zero data-loss events occurred across all $N$ samples; unrestricted MTTDL cannot be estimated.
    ZeroObservedLosses { total_samples: usize },
    /// Some samples failed, but `censored_count > 0` runs were right-censored at $T_{\max}$.
    /// Reporting the mean of only observed failure times would introduce severe downward bias;
    /// callers must use Restricted Mean Survival Time (`rmst_secs`) instead.
    NotIdentifiableDueToRightCensoring {
        observed_failures: usize,
        censored_count: usize,
    },
    /// Every single trajectory failed before $T_{\max}$ (0 right-censored samples);
    /// the sample mean of failure times is an unbiased estimator of unrestricted MTTDL.
    CompleteUncensoredEstimate {
        mttdl_secs: f64,
        std_err_secs: f64,
        total_samples: usize,
    },
}

impl UnrestrictedMttdlStatus {
    /// Formats the status as a JSON object string.
    pub fn to_json(&self) -> String {
        match self {
            Self::ZeroObservedLosses { total_samples } => format!(
                r#"{{"status":"ZeroObservedLosses","mttdl_secs":null,"total_samples":{},"note":"Experiment observed 0 data-loss events; unrestricted MTTDL cannot be established."}}"#,
                total_samples
            ),
            Self::NotIdentifiableDueToRightCensoring {
                observed_failures,
                censored_count,
            } => format!(
                r#"{{"status":"NotIdentifiableDueToRightCensoring","mttdl_secs":null,"observed_failures":{},"censored_count":{},"note":"Right-censored observations present; use RMST(T_max) rather than biased uncensored mean."}}"#,
                observed_failures, censored_count
            ),
            Self::CompleteUncensoredEstimate {
                mttdl_secs,
                std_err_secs,
                total_samples,
            } => format!(
                r#"{{"status":"CompleteUncensoredEstimate","mttdl_secs":{:.6},"std_err_secs":{:.6},"total_samples":{}}}"#,
                mttdl_secs, std_err_secs, total_samples
            ),
        }
    }
}

/// Summary of Kaplan–Meier survival analysis and Restricted Mean Survival Time (RMST) over $[0, T_{\max}]$.
#[derive(Debug, Clone, PartialEq)]
pub struct KaplanMeierSummary {
    /// Evaluation horizon $T_{\max}$ in seconds.
    pub horizon_secs: f64,
    /// Total number of independent Monte Carlo samples $R$.
    pub total_samples: usize,
    /// Number of samples that suffered irrecoverable data loss in $[0, T_{\max}]$.
    pub observed_failures: usize,
    /// Number of samples right-censored at $T_{\max}$ (survived without data loss).
    pub right_censored_samples: usize,
    /// Final Kaplan–Meier survival probability $\hat{S}_{\text{KM}}(T_{\max})$.
    pub final_survival_probability: f64,
    /// Restricted Mean Survival Time $\widehat{\text{RMST}}(T_{\max}) = \int_0^{T_{\max}} \hat{S}_{\text{KM}}(t)\,dt$ in seconds.
    pub rmst_secs: f64,
    /// Standard error of $\widehat{\text{RMST}}(T_{\max})$ in seconds.
    pub rmst_std_err_secs: f64,
    /// Unrestricted MTTDL identifiability status.
    pub unrestricted_mttdl_status: UnrestrictedMttdlStatus,
    /// Stepwise Kaplan–Meier curve points.
    pub steps: Vec<KaplanMeierStep>,
}

/// Computes the exact non-parametric Kaplan–Meier product-limit survival curve and
/// Restricted Mean Survival Time ($\text{RMST}(T_{\max})$) from right-censored failure times.
pub fn compute_kaplan_meier_and_rmst(
    loss_times_secs: &[Option<f64>],
    horizon_secs: f64,
) -> KaplanMeierSummary {
    let total_samples = loss_times_secs.len();
    if total_samples == 0 {
        return KaplanMeierSummary {
            horizon_secs,
            total_samples: 0,
            observed_failures: 0,
            right_censored_samples: 0,
            final_survival_probability: 1.0,
            rmst_secs: horizon_secs,
            rmst_std_err_secs: 0.0,
            unrestricted_mttdl_status: UnrestrictedMttdlStatus::ZeroObservedLosses {
                total_samples: 0,
            },
            steps: vec![],
        };
    }

    let mut failure_times: Vec<f64> = loss_times_secs
        .iter()
        .filter_map(|&opt| opt.map(|t| t.clamp(0.0, horizon_secs)))
        .collect();
    failure_times.sort_by(|a, b| a.total_cmp(b));

    let observed_failures = failure_times.len();
    let right_censored_samples = total_samples - observed_failures;

    // Build Kaplan-Meier step table at distinct failure times
    let mut steps = Vec::new();
    let mut at_risk = total_samples;
    let mut s_km = 1.0f64;
    let mut greenwood_sum = 0.0f64;

    let mut idx = 0;
    while idx < failure_times.len() {
        let t = failure_times[idx];
        let mut d_j = 1usize;
        while idx + d_j < failure_times.len() && (failure_times[idx + d_j] - t).abs() <= 1e-12 {
            d_j += 1;
        }

        let n_j = at_risk as f64;
        let d_j_f = d_j as f64;
        s_km *= 1.0 - (d_j_f / n_j);
        if at_risk > d_j {
            greenwood_sum += d_j_f / (n_j * (n_j - d_j_f));
        }
        let std_err = s_km * greenwood_sum.sqrt();

        steps.push(KaplanMeierStep {
            time_secs: t,
            at_risk,
            failures_at_t: d_j,
            survival_probability: s_km.clamp(0.0, 1.0),
            greenwood_std_err: std_err,
        });

        at_risk -= d_j;
        idx += d_j;
    }

    // Exact RMST(T_max) = (1/R) * sum_r min(T_{loss, r}, T_max), which equals int_0^{T_max} S_KM(t) dt
    let restricted_times: Vec<f64> = loss_times_secs
        .iter()
        .map(|&opt| opt.map_or(horizon_secs, |t| t.clamp(0.0, horizon_secs)))
        .collect();

    let rmst_secs = restricted_times.iter().sum::<f64>() / (total_samples as f64);
    let rmst_std_err_secs = sample_std_error(&restricted_times, rmst_secs);

    let unrestricted_mttdl_status = if observed_failures == 0 {
        UnrestrictedMttdlStatus::ZeroObservedLosses { total_samples }
    } else if right_censored_samples > 0 {
        UnrestrictedMttdlStatus::NotIdentifiableDueToRightCensoring {
            observed_failures,
            censored_count: right_censored_samples,
        }
    } else {
        UnrestrictedMttdlStatus::CompleteUncensoredEstimate {
            mttdl_secs: rmst_secs,
            std_err_secs: rmst_std_err_secs,
            total_samples,
        }
    };

    KaplanMeierSummary {
        horizon_secs,
        total_samples,
        observed_failures,
        right_censored_samples,
        final_survival_probability: s_km.clamp(0.0, 1.0),
        rmst_secs,
        rmst_std_err_secs,
        unrestricted_mttdl_status,
        steps,
    }
}

/// Computes mean, standard deviation, median (P50), P95, and P99 for a slice of `f64` values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DistributionSummary {
    /// Sample mean.
    pub mean: f64,
    /// Sample standard deviation.
    pub std_dev: f64,
    /// Minimum observed value.
    pub min: f64,
    /// 50th percentile (median).
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
    /// Maximum observed value.
    pub max: f64,
}

impl DistributionSummary {
    /// Computes distribution summary statistics from an arbitrary slice of values.
    pub fn from_slice(values: &[f64]) -> Self {
        if values.is_empty() {
            return Self {
                mean: 0.0,
                std_dev: 0.0,
                min: 0.0,
                p50: 0.0,
                p95: 0.0,
                p99: 0.0,
                max: 0.0,
            };
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let n = sorted.len() as f64;
        let mean = sorted.iter().sum::<f64>() / n;
        let var = if sorted.len() > 1 {
            sorted.iter().map(|&x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0)
        } else {
            0.0
        };
        let percentile = |q: f64| -> f64 {
            let idx = ((q * ((sorted.len() - 1) as f64)).round() as usize).min(sorted.len() - 1);
            sorted[idx]
        };
        Self {
            mean,
            std_dev: var.sqrt(),
            min: sorted[0],
            p50: percentile(0.50),
            p95: percentile(0.95),
            p99: percentile(0.99),
            max: sorted[sorted.len() - 1],
        }
    }

    /// Formats the distribution summary as a JSON object string.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"mean":{:.6},"std_dev":{:.6},"min":{:.6},"p50":{:.6},"p95":{:.6},"p99":{:.6},"max":{:.6}}}"#,
            self.mean, self.std_dev, self.min, self.p50, self.p95, self.p99, self.max
        )
    }
}

fn sample_std_error(values: &[f64], mean: f64) -> f64 {
    if values.len() <= 1 {
        return 0.0;
    }
    let n = values.len() as f64;
    let var = values.iter().map(|&x| (x - mean) * (x - mean)).sum::<f64>() / (n - 1.0);
    (var / n).sqrt()
}

/// Paired difference 95% confidence interval for comparing two strategies evaluated on identical
/// exogenous failure traces ($d_i = a_i - b_i$).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairedDifferenceCi95 {
    /// Sample mean of paired differences $\bar{d} = \frac{1}{R} \sum (a_i - b_i)$.
    pub mean_difference: f64,
    /// Standard error of $\bar{d}$.
    pub std_error: f64,
    /// Lower 95% confidence bound $\bar{d} - z_{0.975} \text{SE}$.
    pub lower_95: f64,
    /// Upper 95% confidence bound $\bar{d} + z_{0.975} \text{SE}$.
    pub upper_95: f64,
    /// Number of paired samples $R$.
    pub sample_pairs: usize,
    /// Whether the 95% CI strictly excludes 0.0 (statistically significant at $\alpha = 0.05$).
    pub statistically_significant_95: bool,
}

impl PairedDifferenceCi95 {
    /// Formats the paired difference CI as a JSON object string.
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"mean_diff":{:.6},"std_err":{:.6},"lower_95":{:.6},"upper_95":{:.6},"sample_pairs":{},"statistically_significant_95":{}}}"#,
            self.mean_difference,
            self.std_error,
            self.lower_95,
            self.upper_95,
            self.sample_pairs,
            self.statistically_significant_95
        )
    }
}

/// Computes the paired difference 95% confidence interval across matched trajectories `series_a` and `series_b`.
pub fn paired_difference_ci_95(series_a: &[f64], series_b: &[f64]) -> PairedDifferenceCi95 {
    let n = series_a.len().min(series_b.len());
    if n == 0 {
        return PairedDifferenceCi95 {
            mean_difference: 0.0,
            std_error: 0.0,
            lower_95: 0.0,
            upper_95: 0.0,
            sample_pairs: 0,
            statistically_significant_95: false,
        };
    }
    let diffs: Vec<f64> = (0..n).map(|i| series_a[i] - series_b[i]).collect();
    let mean = diffs.iter().sum::<f64>() / (n as f64);
    let se = sample_std_error(&diffs, mean);
    let lower_95 = mean - Z_95 * se;
    let upper_95 = mean + Z_95 * se;
    let sig = (lower_95 > 0.0 || upper_95 < 0.0) && se > 0.0;

    PairedDifferenceCi95 {
        mean_difference: mean,
        std_error: se,
        lower_95,
        upper_95,
        sample_pairs: n,
        statistically_significant_95: sig,
    }
}
