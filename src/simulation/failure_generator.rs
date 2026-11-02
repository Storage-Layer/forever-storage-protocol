//! Deterministic, orthogonal-substream exogenous failure generator supporting:
//! 1. Independent node transient outages (`NodeOutage`) with configurable duration distributions
//! 2. Independent node permanent failures (`NodePermanentFailure`)
//! 3. Failure-domain-wide transient outages (`DomainOutage`)
//! 4. Correlated permanent failure-domain disasters (`DomainPermanentFailure`)
//! 5. Multi-node correlated burst failures (`BurstNodeFailure`)
//!
//! Because each failure mode draws from a domain-separated PRNG substream derived from
//! `(master_seed, sample_index, stream_tag)` and depends only on cluster topology
//! (`num_nodes`, `num_failure_domains`, `horizon_epochs`), all comparison strategies
//! (`Replication`, `FixedRepair`, `FspAdaptive`) receive **bit-identical exogenous failure traces**.

use std::f64::consts::PI;

use crate::config::{OutageDurationDistribution, SimulationConfig, SECONDS_PER_EPOCH};
use crate::model::failure_domain::FailureDomainId;
use crate::model::node::NodeId;
use crate::simulation::engine::DeterministicRng;
use crate::simulation::events::{EventKind, ScheduledEvent};

/// Stream tags for orthogonal PRNG substream derivation.
const STREAM_TRANSIENT_CHURN: u64 = 0x0100_F590_AAAA_0001;
const STREAM_PERM_NODE_FAIL: u64 = 0x0200_F590_AAAA_0002;
const STREAM_DOMAIN_OUTAGE: u64 = 0x0300_F590_AAAA_0003;
const STREAM_DOMAIN_PERM_FAIL: u64 = 0x0400_F590_AAAA_0004;
const STREAM_BURST_FAIL: u64 = 0x0500_F590_AAAA_0005;

/// Pre-generated exogenous failure trace for a single Monte Carlo trajectory.
#[derive(Debug, Clone, PartialEq)]
pub struct ExogenousFailureTrace {
    /// Derived seed for this sample index.
    pub sample_seed: u64,
    /// Ordered list of exogenous failure events across $[0, T_{\max}]$.
    pub events: Vec<ScheduledEvent>,
    /// Deterministic 64-bit FNV-1a digest of the exogenous failure events alone.
    pub trace_digest: u64,
}

/// Derives a unique, collision-resistant sample seed from `(master_seed, sample_index)`.
pub fn derive_sample_seed(master_seed: u64, sample_index: usize) -> u64 {
    let mut rng = DeterministicRng::new(
        master_seed ^ (sample_index as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93),
    );
    rng.next_u64()
}

/// Samples a positive outage duration (seconds) from `dist` using `rng`.
pub fn sample_outage_duration(rng: &mut DeterministicRng, dist: OutageDurationDistribution) -> f64 {
    match dist {
        OutageDurationDistribution::Exponential { mean_secs } => {
            rng.next_exponential(mean_secs).max(60.0)
        }
        OutageDurationDistribution::LogNormal { median_secs, sigma } => {
            if sigma <= 0.0 {
                return median_secs.max(60.0);
            }
            let u1 = rng.next_f64().clamp(1e-15, 1.0 - 1e-15);
            let u2 = rng.next_f64();
            let z = (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos();
            (median_secs * (sigma * z).exp()).max(60.0)
        }
        OutageDurationDistribution::Fixed { duration_secs } => duration_secs.max(1.0),
        OutageDurationDistribution::Uniform { min_secs, max_secs } => {
            let span = (max_secs - min_secs).max(0.0);
            (min_secs + rng.next_f64() * span).max(1.0)
        }
    }
}

/// Generates a deterministic, strategy-independent exogenous failure trace for `sample_index`.
pub fn generate_exogenous_failure_trace(
    config: &SimulationConfig,
    sample_index: usize,
) -> ExogenousFailureTrace {
    let sample_seed = if sample_index == 0 {
        config.seed
    } else {
        derive_sample_seed(config.seed, sample_index)
    };

    let mut rng_churn = DeterministicRng::new(sample_seed ^ STREAM_TRANSIENT_CHURN);
    let mut rng_perm = DeterministicRng::new(sample_seed ^ STREAM_PERM_NODE_FAIL);
    let mut rng_dom_out = DeterministicRng::new(sample_seed ^ STREAM_DOMAIN_OUTAGE);
    let mut rng_dom_perm = DeterministicRng::new(sample_seed ^ STREAM_DOMAIN_PERM_FAIL);
    let mut rng_burst = DeterministicRng::new(sample_seed ^ STREAM_BURST_FAIL);

    let p_churn = config
        .churn_scenario
        .transient_outage_probability_per_epoch();
    let p_perm = config.permanent_failure_rate_per_epoch;
    let p_dom_out = config.domain_outage_rate_per_epoch;
    let p_dom_perm = config.correlated_domain_failure_rate_per_epoch;
    let burst_cfg = config.burst_failure;

    let mut events = Vec::new();

    for epoch_idx in 0..config.horizon_epochs {
        let epoch_start = f64::from(epoch_idx) * SECONDS_PER_EPOCH;

        // 1. Correlated failure-domain-wide transient outages (Stream 3)
        for dom_idx in 0..config.num_failure_domains {
            let roll = rng_dom_out.next_f64();
            let offset = rng_dom_out.next_f64() * SECONDS_PER_EPOCH;
            let duration = rng_dom_out
                .next_exponential(config.domain_outage_duration_secs)
                .max(60.0);
            if p_dom_out > 0.0 && roll < p_dom_out {
                events.push(ScheduledEvent {
                    time_secs: epoch_start + offset,
                    sequence: 0,
                    kind: EventKind::DomainOutage {
                        domain_id: FailureDomainId(dom_idx),
                        duration_secs: duration,
                    },
                });
            }
        }

        // 2. Correlated permanent domain failures (Stream 4)
        for dom_idx in 0..config.num_failure_domains {
            let roll = rng_dom_perm.next_f64();
            let offset = rng_dom_perm.next_f64() * SECONDS_PER_EPOCH;
            if p_dom_perm > 0.0 && roll < p_dom_perm {
                events.push(ScheduledEvent {
                    time_secs: epoch_start + offset,
                    sequence: 0,
                    kind: EventKind::DomainPermanentFailure {
                        domain_id: FailureDomainId(dom_idx),
                    },
                });
            }
        }

        // 3. Multi-node correlated burst failures (Stream 5)
        {
            let roll = rng_burst.next_f64();
            let offset = rng_burst.next_f64() * SECONDS_PER_EPOCH;
            if burst_cfg.probability_per_epoch > 0.0
                && burst_cfg.affected_nodes > 0
                && roll < burst_cfg.probability_per_epoch
            {
                let count = burst_cfg.affected_nodes.min(config.num_nodes);
                // Deterministic Fisher-Yates partial shuffle to pick `count` distinct nodes
                let mut pool: Vec<NodeId> = (0..config.num_nodes).map(NodeId).collect();
                for i in 0..count {
                    let rem = config.num_nodes - i;
                    let j = i + ((rng_burst.next_u64() as usize) % rem);
                    pool.swap(i, j);
                }
                pool.truncate(count);
                pool.sort();
                events.push(ScheduledEvent {
                    time_secs: epoch_start + offset,
                    sequence: 0,
                    kind: EventKind::BurstNodeFailure {
                        node_ids: pool,
                        is_permanent: burst_cfg.is_permanent,
                        outage_duration_secs: burst_cfg.outage_duration_secs,
                    },
                });
            }
        }

        // 4. Independent per-node transient outages (Stream 1) & permanent failures (Stream 2)
        // Always consume the exact same number of RNG draws per node slot regardless of `p_churn` or `p_perm`
        // so streams remain strictly orthogonal across parameter sweeps.
        for node_idx in 0..config.num_nodes {
            let node_id = NodeId(node_idx);

            let churn_roll = rng_churn.next_f64();
            let churn_offset = rng_churn.next_f64() * SECONDS_PER_EPOCH;
            let churn_duration = sample_outage_duration(&mut rng_churn, config.outage_distribution);
            if p_churn > 0.0 && churn_roll < p_churn {
                events.push(ScheduledEvent {
                    time_secs: epoch_start + churn_offset,
                    sequence: 0,
                    kind: EventKind::NodeOutage {
                        node_id,
                        duration_secs: churn_duration,
                    },
                });
            }

            let perm_roll = rng_perm.next_f64();
            let perm_offset = rng_perm.next_f64() * SECONDS_PER_EPOCH;
            let effective_p_perm = if let Some(ref dist) = config.non_stationary_failure_dist {
                dist.conditional_failure_probability(epoch_start, SECONDS_PER_EPOCH)
            } else {
                p_perm
            };
            if effective_p_perm > 0.0 && perm_roll < effective_p_perm {
                events.push(ScheduledEvent {
                    time_secs: epoch_start + perm_offset,
                    sequence: 0,
                    kind: EventKind::NodePermanentFailure { node_id },
                });
            }
        }
    }

    // Sort events deterministically by timestamp; `sequence` remains 0 until engine insertion
    events.sort_by(|a, b| a.time_secs.total_cmp(&b.time_secs));

    let mut digest: u64 = 0xCBF2_9CE4_8422_2325;
    let mut mix = |v: u64| {
        digest ^= v;
        digest = digest.wrapping_mul(0x0000_0100_0000_01B3);
    };
    for ev in &events {
        mix(ev.time_secs.to_bits());
        match &ev.kind {
            EventKind::NodeOutage {
                node_id,
                duration_secs,
            } => {
                mix(1);
                mix(node_id.0 as u64);
                mix(duration_secs.to_bits());
            }
            EventKind::NodePermanentFailure { node_id } => {
                mix(3);
                mix(node_id.0 as u64);
            }
            EventKind::DomainOutage {
                domain_id,
                duration_secs,
            } => {
                mix(5);
                mix(domain_id.0 as u64);
                mix(duration_secs.to_bits());
            }
            EventKind::DomainPermanentFailure { domain_id } => {
                mix(6);
                mix(domain_id.0 as u64);
            }
            EventKind::BurstNodeFailure {
                node_ids,
                is_permanent,
                outage_duration_secs,
            } => {
                mix(9);
                mix(u64::from(*is_permanent));
                mix(outage_duration_secs.to_bits());
                for n in node_ids {
                    mix(n.0 as u64);
                }
            }
            _ => {}
        }
    }

    ExogenousFailureTrace {
        sample_seed,
        events,
        trace_digest: digest,
    }
}
