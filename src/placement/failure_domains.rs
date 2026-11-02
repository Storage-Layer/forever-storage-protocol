//! Physical Failure-Domain Placement and Durability Mathematics (`src/placement/failure_domains.rs`).
//!
//! Validates erasure code shard placement across hierarchical failure domains (Host, Rack,
//! StorageDevice, Zone, Region, Operator) before allocating shards.
//!
//! Enforces mathematical recovery invariants:
//! - For RS(n, k) with parity $m = n - k$:
//!   - Single-host failure tolerance ($F=1$): requires every physical host $h$ to contain
//!     $\le m$ shards ($\max_h \text{shards}(h) \le m$). Minimum hosts: $\lceil n / m \rceil$.
//!     (e.g. for RS(20,12), $m=8 \implies \ge 3$ hosts, such as 7-7-6). Two hosts cannot survive.
//!   - Double-host failure tolerance ($F=2$): requires any two hosts to contain $\le m$ shards
//!     total. Under balanced placement, $\text{shards}(h) \le \lfloor m / 2 \rfloor = 4 \implies \ge 5$ hosts.
//!   - Rack failure tolerance: requires any failed rack $r$ to contain $\le m$ shards.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::node::NodeId;
use crate::storage::backend::StorageError;

/// Hierarchical failure domain descriptor for a physical storage server.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysicalFailureDomain {
    pub host: String,
    pub rack: String,
    pub disk: String,
    pub zone: String,
    pub region: String,
    pub operator: String,
}

impl PhysicalFailureDomain {
    pub fn new(
        host: impl Into<String>,
        rack: impl Into<String>,
        disk: impl Into<String>,
        zone: impl Into<String>,
        region: impl Into<String>,
        operator: impl Into<String>,
    ) -> Self {
        Self {
            host: host.into(),
            rack: rack.into(),
            disk: disk.into(),
            zone: zone.into(),
            region: region.into(),
            operator: operator.into(),
        }
    }

    pub fn simple(host: impl Into<String>, rack: impl Into<String>) -> Self {
        let h = host.into();
        let r = rack.into();
        Self {
            host: h.clone(),
            rack: r,
            disk: format!("{}-nvme0", h),
            zone: "zone-default".into(),
            region: "region-default".into(),
            operator: "fsp-internal".into(),
        }
    }
}

/// Candidate node available for placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementCandidate {
    pub node_id: NodeId,
    pub listen_addr: String,
    pub domain: PhysicalFailureDomain,
    pub available_bytes: u64,
    pub online: bool,
}

/// Constraints governing physical failure-domain placement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureDomainConstraints {
    pub tolerated_host_failures: usize,
    pub tolerated_rack_failures: usize,
    pub max_shards_per_host: usize,
    pub max_shards_per_rack: usize,
    pub max_shards_per_zone: usize,
    pub min_independent_hosts: usize,
}

impl Default for FailureDomainConstraints {
    fn default() -> Self {
        Self {
            tolerated_host_failures: 1,
            tolerated_rack_failures: 1,
            max_shards_per_host: 8,
            max_shards_per_rack: 8,
            max_shards_per_zone: 20,
            min_independent_hosts: 3,
        }
    }
}

/// Verification report returned after validating a topology or placement plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementValidationReport {
    pub is_valid: bool,
    pub total_shards: usize,
    pub recovery_threshold: usize,
    pub max_tolerable_losses: usize,
    pub shards_per_host: BTreeMap<String, usize>,
    pub shards_per_rack: BTreeMap<String, usize>,
    pub min_surviving_shards_on_host_failure: usize,
    pub min_surviving_shards_on_rack_failure: usize,
    pub rejection_reason: Option<String>,
}

/// Validates whether a proposed shard distribution satisfies claimed physical failure-domain tolerance.
pub fn validate_placement_durability(
    shards_to_domains: &[PhysicalFailureDomain],
    n: usize,
    k: usize,
    constraints: &FailureDomainConstraints,
) -> Result<PlacementValidationReport, StorageError> {
    if n != shards_to_domains.len() {
        return Err(StorageError::Network(format!(
            "shard count mismatch: expected {}, got {}",
            n,
            shards_to_domains.len()
        )));
    }
    if k > n || k == 0 {
        return Err(StorageError::Network(format!(
            "invalid erasure parameters n={}, k={}",
            n, k
        )));
    }

    let max_loss = n.saturating_sub(k);

    let mut host_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut rack_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut zone_counts: BTreeMap<String, usize> = BTreeMap::new();

    for d in shards_to_domains {
        *host_counts.entry(d.host.clone()).or_insert(0) += 1;
        *rack_counts.entry(d.rack.clone()).or_insert(0) += 1;
        *zone_counts.entry(d.zone.clone()).or_insert(0) += 1;
    }

    // 1. Minimum independent hosts check
    if host_counts.len() < constraints.min_independent_hosts {
        let msg = format!(
            "insufficient independent hosts: topology has {} distinct hosts, minimum required is {}",
            host_counts.len(),
            constraints.min_independent_hosts
        );
        return Ok(PlacementValidationReport {
            is_valid: false,
            total_shards: n,
            recovery_threshold: k,
            max_tolerable_losses: max_loss,
            shards_per_host: host_counts,
            shards_per_rack: rack_counts,
            min_surviving_shards_on_host_failure: 0,
            min_surviving_shards_on_rack_failure: 0,
            rejection_reason: Some(msg),
        });
    }

    // 2. Single host failure tolerance check: every host must contain <= max_loss shards
    let mut worst_host_loss = 0usize;
    for (host, count) in &host_counts {
        if *count > worst_host_loss {
            worst_host_loss = *count;
        }
        if *count > constraints.max_shards_per_host {
            let msg = format!(
                "host {} exceeds max_shards_per_host: has {} shards, limit is {}",
                host, count, constraints.max_shards_per_host
            );
            return Ok(PlacementValidationReport {
                is_valid: false,
                total_shards: n,
                recovery_threshold: k,
                max_tolerable_losses: max_loss,
                shards_per_host: host_counts,
                shards_per_rack: rack_counts,
                min_surviving_shards_on_host_failure: n.saturating_sub(worst_host_loss),
                min_surviving_shards_on_rack_failure: 0,
                rejection_reason: Some(msg),
            });
        }
    }

    if constraints.tolerated_host_failures >= 1 && worst_host_loss > max_loss {
        let msg = format!(
            "single host failure untolerable: worst host has {} shards > max tolerable losses {}",
            worst_host_loss, max_loss
        );
        return Ok(PlacementValidationReport {
            is_valid: false,
            total_shards: n,
            recovery_threshold: k,
            max_tolerable_losses: max_loss,
            shards_per_host: host_counts,
            shards_per_rack: rack_counts,
            min_surviving_shards_on_host_failure: n.saturating_sub(worst_host_loss),
            min_surviving_shards_on_rack_failure: 0,
            rejection_reason: Some(msg),
        });
    }

    // 3. Double host failure tolerance check (if configured)
    if constraints.tolerated_host_failures >= 2 {
        let mut sorted_host_counts: Vec<usize> = host_counts.values().copied().collect();
        sorted_host_counts.sort_unstable_by(|a, b| b.cmp(a));
        let top2_loss: usize = sorted_host_counts.iter().take(2).sum();
        if top2_loss > max_loss {
            let msg = format!(
                "double host failure untolerable: top 2 hosts contain {} shards > max tolerable losses {}",
                top2_loss, max_loss
            );
            return Ok(PlacementValidationReport {
                is_valid: false,
                total_shards: n,
                recovery_threshold: k,
                max_tolerable_losses: max_loss,
                shards_per_host: host_counts,
                shards_per_rack: rack_counts,
                min_surviving_shards_on_host_failure: n.saturating_sub(worst_host_loss),
                min_surviving_shards_on_rack_failure: 0,
                rejection_reason: Some(msg),
            });
        }
    }

    // 4. Rack failure tolerance check
    let mut worst_rack_loss = 0usize;
    for (rack, count) in &rack_counts {
        if *count > worst_rack_loss {
            worst_rack_loss = *count;
        }
        if *count > constraints.max_shards_per_rack {
            let msg = format!(
                "rack {} exceeds max_shards_per_rack: has {} shards, limit is {}",
                rack, count, constraints.max_shards_per_rack
            );
            return Ok(PlacementValidationReport {
                is_valid: false,
                total_shards: n,
                recovery_threshold: k,
                max_tolerable_losses: max_loss,
                shards_per_host: host_counts,
                shards_per_rack: rack_counts,
                min_surviving_shards_on_host_failure: n.saturating_sub(worst_host_loss),
                min_surviving_shards_on_rack_failure: n.saturating_sub(worst_rack_loss),
                rejection_reason: Some(msg),
            });
        }
    }

    if constraints.tolerated_rack_failures >= 1 && worst_rack_loss > max_loss {
        let msg = format!(
            "single rack failure untolerable: worst rack has {} shards > max tolerable losses {}",
            worst_rack_loss, max_loss
        );
        return Ok(PlacementValidationReport {
            is_valid: false,
            total_shards: n,
            recovery_threshold: k,
            max_tolerable_losses: max_loss,
            shards_per_host: host_counts,
            shards_per_rack: rack_counts,
            min_surviving_shards_on_host_failure: n.saturating_sub(worst_host_loss),
            min_surviving_shards_on_rack_failure: n.saturating_sub(worst_rack_loss),
            rejection_reason: Some(msg),
        });
    }

    let min_surviving_host = n.saturating_sub(worst_host_loss);
    let min_surviving_rack = n.saturating_sub(worst_rack_loss);

    Ok(PlacementValidationReport {
        is_valid: true,
        total_shards: n,
        recovery_threshold: k,
        max_tolerable_losses: max_loss,
        shards_per_host: host_counts,
        shards_per_rack: rack_counts,
        min_surviving_shards_on_host_failure: min_surviving_host,
        min_surviving_shards_on_rack_failure: min_surviving_rack,
        rejection_reason: None,
    })
}

/// Generates a valid physical placement plan distributing `n` shards across candidates.
pub fn plan_physical_placement(
    candidates: &[PlacementCandidate],
    n: usize,
    k: usize,
    constraints: &FailureDomainConstraints,
) -> Result<Vec<PlacementCandidate>, StorageError> {
    if candidates.len() < n {
        return Err(StorageError::Network(format!(
            "insufficient candidates: have {}, need {}",
            candidates.len(),
            n
        )));
    }

    // Filter online candidates
    let online: Vec<&PlacementCandidate> = candidates.iter().filter(|c| c.online).collect();
    if online.len() < n {
        return Err(StorageError::Network(format!(
            "insufficient online candidates: have {}, need {}",
            online.len(),
            n
        )));
    }

    // Greedily select nodes while respecting host and rack caps
    let mut selected: Vec<PlacementCandidate> = Vec::with_capacity(n);
    let mut host_usage: BTreeMap<String, usize> = BTreeMap::new();
    let mut rack_usage: BTreeMap<String, usize> = BTreeMap::new();
    let mut chosen_nodes: BTreeSet<NodeId> = BTreeSet::new();

    for candidate in &online {
        if selected.len() == n {
            break;
        }
        if chosen_nodes.contains(&candidate.node_id) {
            continue;
        }

        let cur_host = host_usage.get(&candidate.domain.host).copied().unwrap_or(0);
        let cur_rack = rack_usage.get(&candidate.domain.rack).copied().unwrap_or(0);

        if cur_host < constraints.max_shards_per_host && cur_rack < constraints.max_shards_per_rack
        {
            selected.push((*candidate).clone());
            chosen_nodes.insert(candidate.node_id);
            *host_usage.entry(candidate.domain.host.clone()).or_insert(0) += 1;
            *rack_usage.entry(candidate.domain.rack.clone()).or_insert(0) += 1;
        }
    }

    if selected.len() < n {
        return Err(StorageError::Network(format!(
            "could not satisfy failure domain caps: selected only {} of {} shards",
            selected.len(),
            n
        )));
    }

    // Verify durability of the resulting plan
    let domains: Vec<PhysicalFailureDomain> = selected.iter().map(|s| s.domain.clone()).collect();
    let report = validate_placement_durability(&domains, n, k, constraints)?;
    if !report.is_valid {
        return Err(StorageError::Network(
            report.rejection_reason.unwrap_or_default(),
        ));
    }

    Ok(selected)
}
