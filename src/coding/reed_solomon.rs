//! Real byte-level systematic Cauchy Reed–Solomon $(n, k)$ encoder and decoder over $\text{GF}(2^8)$.

use std::collections::BTreeSet;
use std::fmt;

use crate::coding::gf256::{gf_inv, gf_mul_add_slice, GfMatrix};
use crate::model::shard::ShardId;

/// Errors returned by byte-level erasure coding operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    /// Invalid coding parameters ($k = 0$, $n < k$, or $n > 255$).
    InvalidParameters {
        total_shards: usize,
        data_shards: usize,
        reason: &'static str,
    },
    /// Input data shard count does not equal $k$.
    WrongDataShardCount { expected: usize, actual: usize },
    /// Shards in a stripe do not have identical non-zero byte length.
    InvalidShardLength { expected: usize, actual: usize },
    /// Shard index is out of bounds ($\ge n$).
    ShardIdOutOfBounds {
        shard_id: ShardId,
        total_shards: usize,
    },
    /// Duplicate shard index provided in surviving shard list.
    DuplicateShardId { shard_id: ShardId },
    /// Fewer than $k$ surviving shards (or unrecoverable LRC erasure pattern) provided.
    InsufficientShardsForRecovery {
        available: usize,
        required: usize,
        reason: &'static str,
    },
    /// Singular decoding submatrix encountered.
    SingularDecodingMatrix,
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParameters {
                total_shards,
                data_shards,
                reason,
            } => write!(
                f,
                "Invalid erasure code parameters (n={}, k={}): {}",
                total_shards, data_shards, reason
            ),
            Self::WrongDataShardCount { expected, actual } => write!(
                f,
                "Expected {} data shards, but received {}",
                expected, actual
            ),
            Self::InvalidShardLength { expected, actual } => write!(
                f,
                "Invalid shard byte length: expected {}, got {}",
                expected, actual
            ),
            Self::ShardIdOutOfBounds {
                shard_id,
                total_shards,
            } => write!(
                f,
                "ShardId({}) out of bounds for n={}",
                shard_id.0, total_shards
            ),
            Self::DuplicateShardId { shard_id } => {
                write!(f, "Duplicate ShardId({}) in decoding input", shard_id.0)
            }
            Self::InsufficientShardsForRecovery {
                available,
                required,
                reason,
            } => write!(
                f,
                "Insufficient valid shards for recovery: have {}, need >= {} ({})",
                available, required, reason
            ),
            Self::SingularDecodingMatrix => {
                write!(
                    f,
                    "Selected surviving shards form a singular GF(2^8) matrix"
                )
            }
        }
    }
}

impl std::error::Error for CodecError {}

/// Systematic Cauchy Reed–Solomon $\text{RS}(n, k)$ codec over $\text{GF}(2^8)$.
///
/// Generator matrix $G \in \text{GF}(2^8)^{n \times k}$:
/// - Rows $0..k$: $I_k$ (systematic data shards $D_0 \dots D_{k-1}$).
/// - Rows $k..n$: Cauchy matrix $C_{p, j} = \frac{1}{x_p \oplus y_j}$ where $x_p = p$ ($p \in 0..m$)
///   and $y_j = m + j$ ($j \in 0..k$). Every $k \times k$ submatrix of $G$ is strictly invertible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReedSolomonCodec {
    pub total_shards: usize,
    pub data_shards: usize,
    pub parity_shards: usize,
    generator: GfMatrix,
}

impl ReedSolomonCodec {
    /// Constructs a validated systematic Cauchy $\text{RS}(n, k)$ codec.
    pub fn new(total_shards: usize, data_shards: usize) -> Result<Self, CodecError> {
        if data_shards == 0 {
            return Err(CodecError::InvalidParameters {
                total_shards,
                data_shards,
                reason: "data_shards (k) must be >= 1",
            });
        }
        if total_shards < data_shards {
            return Err(CodecError::InvalidParameters {
                total_shards,
                data_shards,
                reason: "total_shards (n) must be >= data_shards (k)",
            });
        }
        if total_shards > 255 {
            return Err(CodecError::InvalidParameters {
                total_shards,
                data_shards,
                reason: "total_shards (n) must be <= 255 for GF(2^8) Cauchy construction",
            });
        }

        let parity_shards = total_shards - data_shards;
        let mut generator = GfMatrix::zeros(total_shards, data_shards);

        // Top k x k rows: Identity matrix I_k
        for i in 0..data_shards {
            generator.set(i, i, 1);
        }

        // Bottom m x k rows: Cauchy matrix C_{p, j} = 1 / (x_p ^ y_j)
        // where x_p = p (0..m) and y_j = m + j (m..m+k).
        // Since x_p < m and y_j >= m, x_p ^ y_j is never 0 and all x_p, y_j are distinct in GF(2^8).
        for p in 0..parity_shards {
            let x_p = p as u8;
            for j in 0..data_shards {
                let y_j = (parity_shards + j) as u8;
                let coeff = gf_inv(x_p ^ y_j);
                generator.set(data_shards + p, j, coeff);
            }
        }

        Ok(Self {
            total_shards,
            data_shards,
            parity_shards,
            generator,
        })
    }

    /// Standard FSP $\text{RS}(20, 12)$ codec ($12$ data shards + $8$ parity shards).
    pub fn fsp_default_20_12() -> Self {
        Self::new(20, 12).expect("RS(20,12) parameters are valid")
    }

    /// Total shards $n$.
    pub const fn total_shards(&self) -> usize {
        self.total_shards
    }

    /// Required data shards $k$.
    pub const fn data_shards(&self) -> usize {
        self.data_shards
    }

    /// Parity shards $m = n - k$.
    pub const fn parity_shards(&self) -> usize {
        self.parity_shards
    }

    /// Returns a reference to the $n \times k$ generator matrix over $\text{GF}(2^8)$.
    pub const fn generator_matrix(&self) -> &GfMatrix {
        &self.generator
    }

    /// Encodes $k$ equal-length data shards into all $n$ coded shards ($k$ systematic data + $m$ parity).
    pub fn encode_stripe(&self, data_shards: &[&[u8]]) -> Result<Vec<Vec<u8>>, CodecError> {
        if data_shards.len() != self.data_shards {
            return Err(CodecError::WrongDataShardCount {
                expected: self.data_shards,
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

        let mut all_shards = Vec::with_capacity(self.total_shards);
        for &d in data_shards {
            all_shards.push(d.to_vec());
        }

        for p in 0..self.parity_shards {
            let mut parity_buf = vec![0u8; shard_len];
            let row = self.generator.row(self.data_shards + p);
            for (j, &coeff) in row.iter().enumerate() {
                gf_mul_add_slice(&mut parity_buf, data_shards[j], coeff);
            }
            all_shards.push(parity_buf);
        }

        Ok(all_shards)
    }

    /// Reconstructs only the specified `target_shards` from at least $k$ valid `available_shards`.
    ///
    /// Uses single-pass matrix multiplication $R_{\text{target}} = G_{\text{target}} \cdot G_S^{-1}$.
    pub fn reconstruct_specific_shards(
        &self,
        target_shards: &[ShardId],
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
    ) -> Result<Vec<(ShardId, Vec<u8>)>, CodecError> {
        if shard_len == 0 {
            return Err(CodecError::InvalidShardLength {
                expected: 1,
                actual: 0,
            });
        }
        for &t in target_shards {
            if t.0 >= self.total_shards {
                return Err(CodecError::ShardIdOutOfBounds {
                    shard_id: t,
                    total_shards: self.total_shards,
                });
            }
        }

        let mut seen = BTreeSet::new();
        for &(shard_id, bytes) in available_shards {
            if shard_id.0 >= self.total_shards {
                return Err(CodecError::ShardIdOutOfBounds {
                    shard_id,
                    total_shards: self.total_shards,
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

        if available_shards.len() < self.data_shards {
            return Err(CodecError::InsufficientShardsForRecovery {
                available: available_shards.len(),
                required: self.data_shards,
                reason: "fewer than k valid distinct shards available",
            });
        }

        if target_shards.is_empty() {
            return Ok(vec![]);
        }

        // Sort available shards by ShardId ascending so systematic data shards 0..k are preferred first
        let mut sorted_avail: Vec<(ShardId, &[u8])> = available_shards.to_vec();
        sorted_avail.sort_by_key(|&(id, _)| id);
        let chosen = &sorted_avail[..self.data_shards];
        let chosen_rows: Vec<usize> = chosen.iter().map(|&(id, _)| id.0).collect();

        let sub_g = self.generator.submatrix_rows(&chosen_rows);
        let inv_g = sub_g.invert().ok_or(CodecError::SingularDecodingMatrix)?;

        let target_rows: Vec<usize> = target_shards.iter().map(|t| t.0).collect();
        let target_g = self.generator.submatrix_rows(&target_rows);
        let repair_matrix = target_g.multiply(&inv_g);

        let mut results = Vec::with_capacity(target_shards.len());
        for (t_idx, &target_id) in target_shards.iter().enumerate() {
            // If target_id is already among available_shards, copy directly
            if let Some(&(_, existing_bytes)) =
                sorted_avail.iter().find(|&&(id, _)| id == target_id)
            {
                results.push((target_id, existing_bytes.to_vec()));
                continue;
            }
            let mut buf = vec![0u8; shard_len];
            let coeff_row = repair_matrix.row(t_idx);
            for (src_idx, &coeff) in coeff_row.iter().enumerate() {
                gf_mul_add_slice(&mut buf, chosen[src_idx].1, coeff);
            }
            results.push((target_id, buf));
        }

        Ok(results)
    }

    /// Decodes all $k$ data shards ($D_0 \dots D_{k-1}$) from any $\ge k$ valid distinct shards.
    pub fn decode_data_shards(
        &self,
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
    ) -> Result<Vec<Vec<u8>>, CodecError> {
        let targets: Vec<ShardId> = (0..self.data_shards).map(ShardId).collect();
        let reconstructed =
            self.reconstruct_specific_shards(&targets, available_shards, shard_len)?;
        Ok(reconstructed.into_iter().map(|(_, bytes)| bytes).collect())
    }

    /// Reconstructs all $n$ shards ($0..n$) of the stripe from any $\ge k$ valid distinct shards.
    pub fn decode_full_stripe(
        &self,
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
    ) -> Result<Vec<Vec<u8>>, CodecError> {
        let targets: Vec<ShardId> = (0..self.total_shards).map(ShardId).collect();
        let reconstructed =
            self.reconstruct_specific_shards(&targets, available_shards, shard_len)?;
        Ok(reconstructed.into_iter().map(|(_, bytes)| bytes).collect())
    }
}
