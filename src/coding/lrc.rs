//! Real byte-level Local Reconstruction Code ($\text{LRC}(n, k, l)$) encoder and decoder over $\text{GF}(2^8)$.
//!
//! Supports both:
//! - Fast local single-shard repair reading only $l$ local group peers via bitwise XOR (`RepairMethod::LocalGroup`)
//! - Global reconstruction across up to $n - k$ erasures via $\text{GF}(2^8)$ rank-verified basis selection and Gauss–Jordan inversion.

use std::collections::BTreeSet;

use crate::coding::gf256::{
    gf_exp, gf_inv, gf_mul, gf_mul_add_slice, xor_slice_in_place, GfMatrix,
};
use crate::coding::reed_solomon::CodecError;
use crate::coding::{LrcConfig, RepairMethod};
use crate::model::shard::ShardId;

/// Outcome of a single-shard LRC repair operation, recording whether local group XOR or global
/// reconstruction was used and the exact source shards read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LrcShardRepairOutput {
    /// Reconstructed shard ID.
    pub target_shard: ShardId,
    /// Reconstructed shard payload bytes.
    pub bytes: Vec<u8>,
    /// Repair method executed (`LocalGroup` vs `GlobalReconstruction`).
    pub method: RepairMethod,
    /// Exact source shards read during the repair.
    pub source_shards_read: Vec<ShardId>,
}

/// Real byte-level $\text{LRC}(n, k, l)$ codec over $\text{GF}(2^8)$.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LrcCodec {
    config: LrcConfig,
    generator: GfMatrix,
}

impl LrcCodec {
    /// Constructs a byte-level $\text{LRC}(n, k, l)$ codec from a validated [`LrcConfig`].
    pub fn new(config: LrcConfig) -> Result<Self, CodecError> {
        let n = config.total_shards;
        let k = config.data_shards;
        let l = config.locality;
        let g = config.local_groups;
        let r_g = config.global_parities;

        if n > 250 {
            return Err(CodecError::InvalidParameters {
                total_shards: n,
                data_shards: k,
                reason: "total_shards must be <= 250 for LRC GF(2^8) construction",
            });
        }

        let mut generator = GfMatrix::zeros(n, k);

        // 1. Rows 0..k: Systematic identity matrix I_k for data shards D_0 .. D_{k-1}
        for i in 0..k {
            generator.set(i, i, 1);
        }

        // 2. Rows k..(k+g): Local group XOR parities L_0 .. L_{g-1}
        for group_idx in 0..g {
            let row = k + group_idx;
            let col_start = group_idx * l;
            for col in col_start..(col_start + l) {
                generator.set(row, col, 1);
            }
        }

        // 3. Rows (k+g)..n: Global parities G_0 .. G_{r_g - 1} over GF(2^8)
        // Constructed using Cauchy evaluation points twisted by generator powers across local group offsets
        // so both direct columns and local-parity-eliminated difference columns (C_{p,j} ^ C_{p, group_base})
        // maintain full rank over GF(2^8).
        for p in 0..r_g {
            let row = k + g + p;
            let x_p = (128 + p) as u8;
            for j in 0..k {
                let group_idx = j / l;
                let offset_in_group = j % l;
                let y_j = (group_idx * (l + 3) + offset_in_group + 1) as u8;
                let cauchy = gf_inv(x_p ^ y_j);
                let twist = gf_exp((p + 1) * (j + 1) + group_idx * 17 + offset_in_group * 5);
                let coeff = gf_mul(cauchy, twist);
                generator.set(row, j, if coeff == 0 { 1 } else { coeff });
            }
        }

        Ok(Self { config, generator })
    }

    /// Standard Phase 2/3 equal-distance $\text{LRC}(22, 12, 4)$ ($12$ data, $3$ local parities, $7$ global parities, $d_{\min} = 9$).
    pub fn lrc_22_12_4() -> Self {
        let cfg = LrcConfig::new(22, 12, 4).expect("valid LRC(22,12,4)");
        Self::new(cfg).expect("valid LRC(22,12,4) codec")
    }

    /// Standard Phase 2/3 equal-overhead $\text{LRC}(20, 12, 4)$ ($12$ data, $3$ local parities, $5$ global parities, $d_{\min} = 7$).
    pub fn lrc_20_12_4() -> Self {
        let cfg = LrcConfig::new(20, 12, 4).expect("valid LRC(20,12,4)");
        Self::new(cfg).expect("valid LRC(20,12,4) codec")
    }

    /// Returns the underlying [`LrcConfig`].
    pub const fn config(&self) -> &LrcConfig {
        &self.config
    }

    /// Returns the $n \times k$ generator matrix over $\text{GF}(2^8)$.
    pub const fn generator_matrix(&self) -> &GfMatrix {
        &self.generator
    }

    /// Returns `true` iff the surviving shard set spans full rank $k$ in the actual $\text{GF}(2^8)$ generator matrix.
    pub fn can_decode(&self, surviving_shards: &BTreeSet<ShardId>) -> bool {
        let n = self.config.total_shards;
        let k = self.config.data_shards;
        let rows: Vec<usize> = surviving_shards
            .iter()
            .map(|s| s.0)
            .filter(|&idx| idx < n)
            .collect();
        if rows.len() < k {
            return false;
        }
        self.generator.submatrix_rows(&rows).rank() == k
    }

    /// Encodes $k$ equal-length data shards into all $n$ LRC shards ($k$ data + $g$ local parities + $r_g$ global parities).
    pub fn encode_stripe(&self, data_shards: &[&[u8]]) -> Result<Vec<Vec<u8>>, CodecError> {
        let k = self.config.data_shards;
        let n = self.config.total_shards;
        let l = self.config.locality;
        let g = self.config.local_groups;

        if data_shards.len() != k {
            return Err(CodecError::WrongDataShardCount {
                expected: k,
                actual: data_shards.len(),
            });
        }
        let shard_len = data_shards[0].len();
        if shard_len == 0 {
            return Err(CodecError::InvalidShardLength {
                expected: 1,
                actual: 0,
            });
        }
        for s in data_shards.iter().skip(1) {
            if s.len() != shard_len {
                return Err(CodecError::InvalidShardLength {
                    expected: shard_len,
                    actual: s.len(),
                });
            }
        }

        let mut all_shards = Vec::with_capacity(n);
        for &d in data_shards {
            all_shards.push(d.to_vec());
        }

        // Local parities: pure bitwise XOR across each local group of l data shards
        for group_idx in 0..g {
            let mut local_parity = vec![0u8; shard_len];
            let start = group_idx * l;
            for &d_slice in &data_shards[start..(start + l)] {
                xor_slice_in_place(&mut local_parity, d_slice);
            }
            all_shards.push(local_parity);
        }

        // Global parities: GF(2^8) linear combination across all k data shards
        for row_idx in (k + g)..n {
            let mut global_parity = vec![0u8; shard_len];
            let row = self.generator.row(row_idx);
            for (j, &coeff) in row.iter().enumerate() {
                gf_mul_add_slice(&mut global_parity, data_shards[j], coeff);
            }
            all_shards.push(global_parity);
        }

        Ok(all_shards)
    }

    /// Selects $k$ linearly independent surviving shards from `available_shards` using incremental
    /// $\text{GF}(2^8)$ Gaussian elimination basis selection.
    fn select_independent_basis<'a>(
        &self,
        available_shards: &[(ShardId, &'a [u8])],
        shard_len: usize,
    ) -> Result<Vec<(ShardId, &'a [u8])>, CodecError> {
        let n = self.config.total_shards;
        let k = self.config.data_shards;

        if shard_len == 0 {
            return Err(CodecError::InvalidShardLength {
                expected: 1,
                actual: 0,
            });
        }

        let mut seen = BTreeSet::new();
        for &(shard_id, bytes) in available_shards {
            if shard_id.0 >= n {
                return Err(CodecError::ShardIdOutOfBounds {
                    shard_id,
                    total_shards: n,
                });
            }
            if !seen.insert(shard_id) {
                return Err(CodecError::DuplicateShardId { shard_id });
            }
            if bytes.len() != shard_len {
                return Err(CodecError::InvalidShardLength {
                    expected: shard_len,
                    actual: bytes.len(),
                });
            }
        }

        if available_shards.len() < k {
            return Err(CodecError::InsufficientShardsForRecovery {
                available: available_shards.len(),
                required: k,
                reason: "fewer than k surviving shards provided to LRC decoder",
            });
        }

        let mut sorted: Vec<(ShardId, &'a [u8])> = available_shards.to_vec();
        sorted.sort_by_key(|&(id, _)| id);

        let mut chosen: Vec<(ShardId, &'a [u8])> = Vec::with_capacity(k);
        let mut chosen_rows: Vec<usize> = Vec::with_capacity(k);

        for (shard_id, bytes) in sorted {
            let mut candidate_rows = chosen_rows.clone();
            candidate_rows.push(shard_id.0);
            let sub = self.generator.submatrix_rows(&candidate_rows);
            if sub.rank() == candidate_rows.len() {
                chosen_rows.push(shard_id.0);
                chosen.push((shard_id, bytes));
                if chosen.len() == k {
                    return Ok(chosen);
                }
            }
        }

        Err(CodecError::InsufficientShardsForRecovery {
            available: chosen.len(),
            required: k,
            reason: "surviving LRC shards do not span k linearly independent equations",
        })
    }

    /// Repairs a single missing shard using local 4-shard XOR repair whenever its local group peers
    /// are available, or falls back to global reconstruction otherwise.
    pub fn repair_single_shard(
        &self,
        missing_shard: ShardId,
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
    ) -> Result<LrcShardRepairOutput, CodecError> {
        let n = self.config.total_shards;
        if missing_shard.0 >= n {
            return Err(CodecError::ShardIdOutOfBounds {
                shard_id: missing_shard,
                total_shards: n,
            });
        }

        // Check if local group XOR repair is possible (reads only l = 4 shards!)
        if let Some(group_idx) = self.config.local_group_of(missing_shard) {
            let members = self.config.local_group_members(group_idx);
            let mut peer_slices: Vec<(ShardId, &[u8])> = Vec::with_capacity(self.config.locality);
            for member_id in members {
                if member_id == missing_shard {
                    continue;
                }
                if let Some(&(_, slice)) = available_shards.iter().find(|&&(id, _)| id == member_id)
                {
                    if slice.len() != shard_len {
                        return Err(CodecError::InvalidShardLength {
                            expected: shard_len,
                            actual: slice.len(),
                        });
                    }
                    peer_slices.push((member_id, slice));
                }
            }

            if peer_slices.len() == self.config.locality {
                let mut repaired = vec![0u8; shard_len];
                let mut source_ids = Vec::with_capacity(self.config.locality);
                for (peer_id, slice) in peer_slices {
                    xor_slice_in_place(&mut repaired, slice);
                    source_ids.push(peer_id);
                }
                return Ok(LrcShardRepairOutput {
                    target_shard: missing_shard,
                    bytes: repaired,
                    method: RepairMethod::LocalGroup {
                        shards_to_read: self.config.locality,
                        group_index: group_idx,
                    },
                    source_shards_read: source_ids,
                });
            }
        }

        // Fall back to global reconstruction reading k linearly independent shards
        let basis = self.select_independent_basis(available_shards, shard_len)?;
        let source_ids: Vec<ShardId> = basis.iter().map(|&(id, _)| id).collect();
        let reconstructed =
            self.reconstruct_specific_shards_global(&[missing_shard], &basis, shard_len)?;
        let (_, bytes) = reconstructed.into_iter().next().unwrap();

        Ok(LrcShardRepairOutput {
            target_shard: missing_shard,
            bytes,
            method: RepairMethod::GlobalReconstruction {
                shards_to_read: self.config.data_shards,
            },
            source_shards_read: source_ids,
        })
    }

    /// Reconstructs the specified `target_shards` from `available_shards`.
    pub fn reconstruct_specific_shards(
        &self,
        target_shards: &[ShardId],
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
    ) -> Result<Vec<(ShardId, Vec<u8>)>, CodecError> {
        if target_shards.len() == 1 {
            let out = self.repair_single_shard(target_shards[0], available_shards, shard_len)?;
            return Ok(vec![(out.target_shard, out.bytes)]);
        }
        self.reconstruct_specific_shards_global(target_shards, available_shards, shard_len)
    }

    fn reconstruct_specific_shards_global(
        &self,
        target_shards: &[ShardId],
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
    ) -> Result<Vec<(ShardId, Vec<u8>)>, CodecError> {
        for &t in target_shards {
            if t.0 >= self.config.total_shards {
                return Err(CodecError::ShardIdOutOfBounds {
                    shard_id: t,
                    total_shards: self.config.total_shards,
                });
            }
        }
        if target_shards.is_empty() {
            return Ok(vec![]);
        }

        let basis = self.select_independent_basis(available_shards, shard_len)?;
        let chosen_rows: Vec<usize> = basis.iter().map(|&(id, _)| id.0).collect();

        let sub_g = self.generator.submatrix_rows(&chosen_rows);
        let inv_g = sub_g.invert().ok_or(CodecError::SingularDecodingMatrix)?;

        let target_rows: Vec<usize> = target_shards.iter().map(|t| t.0).collect();
        let target_g = self.generator.submatrix_rows(&target_rows);
        let repair_matrix = target_g.multiply(&inv_g);

        let mut results = Vec::with_capacity(target_shards.len());
        for (t_idx, &target_id) in target_shards.iter().enumerate() {
            if let Some(&(_, existing)) = available_shards.iter().find(|&&(id, _)| id == target_id)
            {
                results.push((target_id, existing.to_vec()));
                continue;
            }
            let mut buf = vec![0u8; shard_len];
            let coeff_row = repair_matrix.row(t_idx);
            for (src_idx, &coeff) in coeff_row.iter().enumerate() {
                gf_mul_add_slice(&mut buf, basis[src_idx].1, coeff);
            }
            results.push((target_id, buf));
        }

        Ok(results)
    }

    /// Decodes all $k$ data shards ($D_0 \dots D_{k-1}$) from `available_shards`.
    pub fn decode_data_shards(
        &self,
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
    ) -> Result<Vec<Vec<u8>>, CodecError> {
        let targets: Vec<ShardId> = (0..self.config.data_shards).map(ShardId).collect();
        let reconstructed =
            self.reconstruct_specific_shards(&targets, available_shards, shard_len)?;
        Ok(reconstructed.into_iter().map(|(_, bytes)| bytes).collect())
    }
}
