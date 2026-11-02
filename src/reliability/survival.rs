//! Analytical survival probability estimators (independent & domain-correlated) and
//! guarded empirical MTTDL calculations.

use crate::config::SECONDS_PER_EPOCH;

/// Point on the survival probability curve over simulation time.
#[derive(Debug, Clone, PartialEq)]
pub struct SurvivalCurvePoint {
    /// Simulation time $t$ in seconds.
    pub time_secs: f64,
    /// Empirical Monte Carlo survival probability $\hat{S}(t) \in [0.0, 1.0]$.
    pub empirical_survival: f64,
    /// Analytical no-repair binomial survival probability from $t=0$ to $t$.
    pub analytical_no_repair_survival: f64,
}

/// Guarded result of an empirical Mean Time To Data Loss (MTTDL) calculation across runs.
#[derive(Debug, Clone, PartialEq)]
pub enum EmpiricalMttdlResult {
    /// Zero data loss events occurred across all simulated runs; the data is 100% right-censored
    /// and a finite empirical MTTDL cannot be estimated without fabrication.
    RightCensoredZeroLosses,
    /// At least one data loss event occurred; provides the Total-Time-On-Test (TTT) point estimate
    /// along with the number of observed losses and total runs so callers can assess sample sufficiency.
    Estimated {
        /// Point estimate $\hat{\text{MTTDL}} = \frac{\sum \min(T_{\text{loss}, r}, T_{\max})}{N_{\text{losses}}}$ in seconds.
        mttdl_secs: f64,
        /// Number of trajectories that experienced irrecoverable data loss.
        observed_losses: usize,
        /// Total number of trajectories evaluated.
        total_runs: usize,
    },
}

/// Computes the guarded empirical MTTDL from a slice of per-run time-to-loss values
/// (`Some(loss_time_secs)` if irrecoverable data loss occurred, `None` if survived until `horizon_secs`).
pub fn estimate_empirical_mttdl(
    loss_times_secs: &[Option<f64>],
    horizon_secs: f64,
) -> EmpiricalMttdlResult {
    let total_runs = loss_times_secs.len();
    let mut observed_losses = 0usize;
    let mut total_time_on_test = 0.0f64;

    for &entry in loss_times_secs {
        match entry {
            Some(t_loss) => {
                observed_losses += 1;
                total_time_on_test += t_loss.clamp(0.0, horizon_secs);
            }
            None => {
                total_time_on_test += horizon_secs.max(0.0);
            }
        }
    }

    if observed_losses == 0 {
        EmpiricalMttdlResult::RightCensoredZeroLosses
    } else {
        EmpiricalMttdlResult::Estimated {
            mttdl_secs: total_time_on_test / (observed_losses as f64),
            observed_losses,
            total_runs,
        }
    }
}

/// Analytical binomial survival probability over window $\Delta t$ (seconds) **without repair**,
/// given $u$ currently durable shards, threshold $k$, and independent per-node per-epoch permanent
/// failure probability $p_{\text{perm\_epoch}}$.
///
/// Formula:
/// $$S_{\text{indep}}(\Delta t \mid u) = \sum_{j=k}^{u} \binom{u}{j} p_s(\Delta t)^j (1 - p_s(\Delta t))^{u - j}$$
pub fn analytical_binomial_survival_probability(
    durable_shards: usize,
    recovery_threshold: usize,
    perm_failure_prob_per_epoch: f64,
    window_secs: f64,
) -> f64 {
    if durable_shards < recovery_threshold {
        return 0.0;
    }
    if perm_failure_prob_per_epoch <= 0.0 || window_secs <= 0.0 {
        return 1.0;
    }
    if perm_failure_prob_per_epoch >= 1.0 {
        return 0.0;
    }

    let lambda_per_sec = -((1.0 - perm_failure_prob_per_epoch).ln()) / SECONDS_PER_EPOCH;
    let p_survive = (-lambda_per_sec * window_secs).exp().clamp(0.0, 1.0);
    let p_fail = 1.0 - p_survive;

    let mut total_prob = 0.0;
    for j in recovery_threshold..=durable_shards {
        let comb = binomial_coeff(durable_shards, j) as f64;
        let term = comb * p_survive.powi(j as i32) * p_fail.powi((durable_shards - j) as i32);
        total_prob += term;
    }

    total_prob.clamp(0.0, 1.0)
}

/// Analytical domain-correlated survival probability over window $\Delta t$ (seconds) **without repair**.
///
/// Explicitly accounts for both correlated permanent failure-domain disasters (`domain_perm_prob_per_epoch`)
/// across `num_domains` (with max concentration `max_shards_per_domain`) and independent per-node
/// permanent failures (`node_perm_prob_per_epoch`).
pub fn analytical_correlated_survival_probability(
    durable_shards: usize,
    recovery_threshold: usize,
    num_domains: usize,
    max_shards_per_domain: usize,
    node_perm_prob_per_epoch: f64,
    domain_perm_prob_per_epoch: f64,
    window_secs: f64,
) -> f64 {
    if durable_shards < recovery_threshold {
        return 0.0;
    }
    if window_secs <= 0.0 {
        return 1.0;
    }
    if domain_perm_prob_per_epoch <= 0.0 || num_domains == 0 || max_shards_per_domain == 0 {
        return analytical_binomial_survival_probability(
            durable_shards,
            recovery_threshold,
            node_perm_prob_per_epoch,
            window_secs,
        );
    }
    if domain_perm_prob_per_epoch >= 1.0 {
        return 0.0;
    }

    let lambda_dom = -((1.0 - domain_perm_prob_per_epoch).ln()) / SECONDS_PER_EPOCH;
    let q_dom_survive = (-lambda_dom * window_secs).exp().clamp(0.0, 1.0);
    let q_dom_fail = 1.0 - q_dom_survive;

    let mut total_survival = 0.0;
    for m_surviving_domains in 0..=num_domains {
        let failed_domains = num_domains - m_surviving_domains;
        let shards_destroyed_by_domains = failed_domains.saturating_mul(max_shards_per_domain);
        let remaining_shards = durable_shards.saturating_sub(shards_destroyed_by_domains);
        if remaining_shards < recovery_threshold {
            continue;
        }
        let dom_comb = binomial_coeff(num_domains, m_surviving_domains) as f64;
        let dom_prob = dom_comb
            * q_dom_survive.powi(m_surviving_domains as i32)
            * q_dom_fail.powi(failed_domains as i32);
        let cond_node_surv = analytical_binomial_survival_probability(
            remaining_shards,
            recovery_threshold,
            node_perm_prob_per_epoch,
            window_secs,
        );
        total_survival += dom_prob * cond_node_surv;
    }

    total_survival.clamp(0.0, 1.0)
}

/// Analytical birth-death Markov chain approximation of MTTDL (in seconds) under independent
/// shard failures at per-shard rate $\lambda$ and sequential shard repair rate $\mu$.
pub fn analytical_markov_mttdl_secs(
    total_shards: usize,
    recovery_threshold: usize,
    perm_failure_prob_per_epoch: f64,
    mean_single_shard_repair_secs: f64,
) -> Option<f64> {
    if total_shards < recovery_threshold
        || recovery_threshold == 0
        || perm_failure_prob_per_epoch <= 0.0
        || mean_single_shard_repair_secs <= 0.0
    {
        return None;
    }
    let lambda = -((1.0 - perm_failure_prob_per_epoch).ln()) / SECONDS_PER_EPOCH;
    let mu = 1.0 / mean_single_shard_repair_secs;

    let mut ratio_prod = 1.0 / ((recovery_threshold as f64) * lambda);
    for i in (recovery_threshold + 1)..=total_shards {
        ratio_prod *= mu / ((i as f64) * lambda);
    }
    if ratio_prod.is_finite() {
        Some(ratio_prod)
    } else {
        None
    }
}

pub(crate) fn binomial_coeff(n: usize, k: usize) -> u128 {
    if k > n {
        return 0;
    }
    let k = k.min(n - k);
    let mut res: u128 = 1;
    for i in 0..k {
        res = res * ((n - i) as u128) / ((i + 1) as u128);
    }
    res
}
