//! Phase 2 & Phase 3 Coding Architecture: Reed–Solomon vs. Local Reconstruction Codes (LRC).
//!
//! Implements both logical erasure-pattern analysis and byte-accurate $\text{GF}(2^8)$ encoding,
//! decoding, stripe framing, and local/global repair reconstruction.

pub mod codec;
pub mod gf256;
pub mod lrc;
pub mod reed_solomon;
pub mod stripe;

use std::collections::BTreeSet;

use crate::config::ConfigError;
use crate::model::shard::ShardId;

/// Repair execution method and read amplification cost for a single missing shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairMethod {
    /// Local repair within a local parity group of locality $l$: reads only $l$ shards.
    LocalGroup {
        /// Number of surviving shards in the local group that must be read ($l$).
        shards_to_read: usize,
        /// Index of the local group $j \in \{0, \dots, g-1\}$.
        group_index: usize,
    },
    /// Global reconstruction across the stripe: reads $k$ linearly independent surviving shards.
    GlobalReconstruction {
        /// Number of surviving shards across the stripe that must be read ($k$).
        shards_to_read: usize,
    },
}

impl RepairMethod {
    /// Returns the number of surviving shards read during this repair.
    pub const fn shards_to_read(&self) -> usize {
        match *self {
            Self::LocalGroup { shards_to_read, .. } => shards_to_read,
            Self::GlobalReconstruction { shards_to_read } => shards_to_read,
        }
    }

    /// Returns total network bytes transferred (read + 1 shard written) for `shard_size_bytes`.
    pub const fn total_transfer_bytes(&self, shard_size_bytes: u64) -> u64 {
        ((self.shards_to_read() as u64) + 1) * shard_size_bytes
    }
}

/// Validated Local Reconstruction Code $\text{LRC}(n, k, l)$ configuration.
///
/// Layout of the $n$ logical shards (`ShardId(0..n)`):
/// - `0..k`: $k$ data shards partitioned into $g = k / l$ disjoint local groups of $l$ data shards each.
/// - `k..(k + g)`: $g$ local parity shards $L_0, \dots, L_{g-1}$ (where $L_j$ is the XOR parity of data group $j$).
/// - `(k + g)..n`: $r_g = n - k - g$ global parity shards $G_0, \dots, G_{r_g - 1}$ in a Maximally Recoverable (MR) construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LrcConfig {
    /// Total coded shards $n$.
    pub total_shards: usize,
    /// Number of data shards $k$.
    pub data_shards: usize,
    /// Locality parameter $l$ (number of data shards per local group; must divide $k$).
    pub locality: usize,
    /// Number of local parity groups $g = k / l$.
    pub local_groups: usize,
    /// Number of global parity shards $r_g = n - k - g$.
    pub global_parities: usize,
}

impl LrcConfig {
    /// Validates and constructs an $\text{LRC}(n, k, l)$ code.
    pub fn new(
        total_shards: usize,
        data_shards: usize,
        locality: usize,
    ) -> Result<Self, ConfigError> {
        if data_shards == 0 || locality == 0 || !data_shards.is_multiple_of(locality) {
            return Err(ConfigError::InvalidErasureCode {
                total_shards,
                recovery_threshold: data_shards,
            });
        }
        let local_groups = data_shards / locality;
        if total_shards < data_shards + local_groups {
            return Err(ConfigError::InvalidErasureCode {
                total_shards,
                recovery_threshold: data_shards,
            });
        }
        let global_parities = total_shards - data_shards - local_groups;
        Ok(Self {
            total_shards,
            data_shards,
            locality,
            local_groups,
            global_parities,
        })
    }

    /// Returns the number of local parity groups $g = k / l$.
    pub const fn num_local_groups(&self) -> usize {
        self.local_groups
    }

    /// Returns the number of global parity shards $r_g = n - k - g$.
    pub const fn num_global_parities(&self) -> usize {
        self.global_parities
    }

    /// Nominal storage overhead $\Omega = n / k$.
    pub fn nominal_storage_overhead(&self) -> f64 {
        (self.total_shards as f64) / (self.data_shards as f64)
    }

    /// Gopalan et al. (2012) exact minimum distance $d_{\min} = n - k - g + 2 = r_g + 2$.
    pub const fn minimum_distance(&self) -> usize {
        self.global_parities + 2
    }

    /// Maximum number of arbitrary simultaneous erasures guaranteed to be recoverable ($d_{\min} - 1 = r_g + 1$).
    pub const fn guaranteed_arbitrary_erasures(&self) -> usize {
        self.global_parities + 1
    }

    /// Returns the `ShardId`s belonging to local group `group_idx` ($l$ data shards + $1$ local parity shard).
    pub fn local_group_members(&self, group_idx: usize) -> Vec<ShardId> {
        if group_idx >= self.local_groups {
            return vec![];
        }
        let start = group_idx * self.locality;
        let mut members: Vec<ShardId> = (start..(start + self.locality)).map(ShardId).collect();
        members.push(ShardId(self.data_shards + group_idx));
        members
    }

    /// Returns `Some(group_idx)` if `shard_id` is a data shard or local parity shard belonging to a local group,
    /// or `None` if `shard_id` is a global parity shard.
    pub const fn local_group_of(&self, shard_id: ShardId) -> Option<usize> {
        let idx = shard_id.0;
        if idx < self.data_shards {
            Some(idx / self.locality)
        } else if idx < self.data_shards + self.local_groups {
            Some(idx - self.data_shards)
        } else {
            None
        }
    }

    /// Verifies exact Maximally Recoverable (MR) LRC decodability for the set of `surviving_shards`.
    ///
    /// Mathematical condition (proven in `docs/PHASE2_CODING_RESEARCH.md`):
    /// In each local group $j \in \{0, \dots, g-1\}$ of size $l + 1$, if $e_j$ shards are erased,
    /// the local parity resolves 1 erasure, leaving $\max(0, e_j - 1)$ unresolved erasures that
    /// must be solved by the surviving global parities ($r_g - e_{\text{global}}$).
    /// Thus the pattern is decodable iff:
    /// $$\sum_{j=0}^{g-1} \max(0, e_j - 1) + e_{\text{global}} \le r_g$$
    pub fn is_pattern_decodable(&self, surviving_shards: &BTreeSet<ShardId>) -> bool {
        let mut unresolved_local_erasures = 0usize;

        for g_idx in 0..self.local_groups {
            let members = self.local_group_members(g_idx);
            let erased_in_group = members
                .iter()
                .filter(|s| !surviving_shards.contains(s))
                .count();
            unresolved_local_erasures += erased_in_group.saturating_sub(1);
        }

        let global_start = self.data_shards + self.local_groups;
        let erased_global = (global_start..self.total_shards)
            .map(ShardId)
            .filter(|s| !surviving_shards.contains(s))
            .count();

        unresolved_local_erasures + erased_global <= self.global_parities
    }

    /// Plans the lowest-cost valid repair method for `missing_shard` given `online_shards`.
    ///
    /// - Returns `Some(RepairMethod::LocalGroup { shards_to_read: l, group_index })` if `missing_shard`
    ///   belongs to a local group and all other $l$ members of that local group are in `online_shards`.
    /// - Otherwise, if `is_pattern_decodable(online_shards)` holds, returns
    ///   `Some(RepairMethod::GlobalReconstruction { shards_to_read: k })`.
    /// - Returns `None` if the object cannot be reconstructed from `online_shards`.
    pub fn plan_shard_repair(
        &self,
        missing_shard: ShardId,
        online_shards: &BTreeSet<ShardId>,
    ) -> Option<RepairMethod> {
        if let Some(g_idx) = self.local_group_of(missing_shard) {
            let members = self.local_group_members(g_idx);
            let all_peers_online = members
                .iter()
                .filter(|&&s| s != missing_shard)
                .all(|s| online_shards.contains(s));
            if all_peers_online {
                return Some(RepairMethod::LocalGroup {
                    shards_to_read: self.locality,
                    group_index: g_idx,
                });
            }
        }

        if self.is_pattern_decodable(online_shards) {
            Some(RepairMethod::GlobalReconstruction {
                shards_to_read: self.data_shards,
            })
        } else {
            None
        }
    }
}

/// Unified coding scheme abstraction supporting Reed–Solomon (and replication as $k=1$) and LRC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodingScheme {
    /// Standard MDS Reed–Solomon $(n, k)$ code (or $R$-way replication when $k = 1$).
    ReedSolomon { n: usize, k: usize },
    /// Locally Repairable Code $\text{LRC}(n, k, l)$.
    Lrc(LrcConfig),
}

impl CodingScheme {
    /// Total coded shards $n$.
    pub const fn total_shards(&self) -> usize {
        match *self {
            Self::ReedSolomon { n, .. } => n,
            Self::Lrc(cfg) => cfg.total_shards,
        }
    }

    /// Number of data shards $k$.
    pub const fn data_shards(&self) -> usize {
        match *self {
            Self::ReedSolomon { k, .. } => k,
            Self::Lrc(cfg) => cfg.data_shards,
        }
    }

    /// Nominal storage overhead $\Omega = n / k$.
    pub fn nominal_storage_overhead(&self) -> f64 {
        (self.total_shards() as f64) / (self.data_shards() as f64)
    }

    /// Returns `true` iff the set of `surviving_shards` is sufficient to decode the object.
    pub fn is_decodable(&self, surviving_shards: &BTreeSet<ShardId>) -> bool {
        match *self {
            Self::ReedSolomon { k, .. } => surviving_shards.len() >= k,
            Self::Lrc(cfg) => cfg.is_pattern_decodable(surviving_shards),
        }
    }

    /// Plans the repair method and read amplification for `missing_shard` given `online_shards`.
    pub fn plan_repair(
        &self,
        missing_shard: ShardId,
        online_shards: &BTreeSet<ShardId>,
    ) -> Option<RepairMethod> {
        match *self {
            Self::ReedSolomon { k, .. } => {
                if online_shards.len() >= k {
                    Some(RepairMethod::GlobalReconstruction { shards_to_read: k })
                } else {
                    None
                }
            }
            Self::Lrc(cfg) => cfg.plan_shard_repair(missing_shard, online_shards),
        }
    }
}
