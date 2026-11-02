//! Unified byte-level erasure codec dispatcher wrapping [`ReedSolomonCodec`] and [`LrcCodec`].

use crate::coding::lrc::{LrcCodec, LrcShardRepairOutput};
use crate::coding::reed_solomon::{CodecError, ReedSolomonCodec};
use crate::coding::stripe::{join_data_shards_into_payload, split_payload_into_data_shards};
use crate::coding::{CodingScheme, RepairMethod};
use crate::model::shard::ShardId;

/// List of reconstructed `(ShardId, shard_bytes)` pairs.
pub type ReconstructedShards = Vec<(ShardId, Vec<u8>)>;

/// Unified byte-level erasure codec supporting both systematic Cauchy Reed–Solomon and LRC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErasureCodec {
    /// Systematic Cauchy Reed–Solomon $(n, k)$ codec.
    ReedSolomon(ReedSolomonCodec),
    /// Local Reconstruction Code $\text{LRC}(n, k, l)$ codec.
    Lrc(LrcCodec),
}

impl ErasureCodec {
    /// Constructs a byte-level [`ErasureCodec`] from a logical [`CodingScheme`].
    pub fn from_scheme(scheme: CodingScheme) -> Result<Self, CodecError> {
        match scheme {
            CodingScheme::ReedSolomon { n, k } => {
                Ok(Self::ReedSolomon(ReedSolomonCodec::new(n, k)?))
            }
            CodingScheme::Lrc(cfg) => Ok(Self::Lrc(LrcCodec::new(cfg)?)),
        }
    }

    /// Returns the corresponding [`CodingScheme`].
    pub fn scheme(&self) -> CodingScheme {
        match self {
            Self::ReedSolomon(rs) => CodingScheme::ReedSolomon {
                n: rs.total_shards(),
                k: rs.data_shards(),
            },
            Self::Lrc(lrc) => CodingScheme::Lrc(*lrc.config()),
        }
    }

    /// Total coded shards $n$.
    pub const fn total_shards(&self) -> usize {
        match self {
            Self::ReedSolomon(rs) => rs.total_shards(),
            Self::Lrc(lrc) => lrc.config().total_shards,
        }
    }

    /// Required data shards $k$.
    pub const fn data_shards(&self) -> usize {
        match self {
            Self::ReedSolomon(rs) => rs.data_shards(),
            Self::Lrc(lrc) => lrc.config().data_shards,
        }
    }

    /// Verifies that `surviving_shards` spans full rank $k$ in the codec's $\text{GF}(2^8)$ generator matrix.
    pub fn can_decode(&self, surviving_shards: &std::collections::BTreeSet<ShardId>) -> bool {
        match self {
            Self::ReedSolomon(rs) => {
                let n = rs.total_shards();
                let k = rs.data_shards();
                let rows: Vec<usize> = surviving_shards
                    .iter()
                    .map(|s| s.0)
                    .filter(|&idx| idx < n)
                    .collect();
                rows.len() >= k && rs.generator_matrix().submatrix_rows(&rows).rank() == k
            }
            Self::Lrc(lrc) => lrc.can_decode(surviving_shards),
        }
    }

    /// Encodes a raw stripe payload (`0..=stripe_size` bytes) into $n$ shards of length `shard_len`.
    /// Returns `(shard_len, all_n_shards)`.
    pub fn encode_stripe_payload(
        &self,
        payload: &[u8],
    ) -> Result<(usize, Vec<Vec<u8>>), CodecError> {
        let k = self.data_shards();
        let (shard_len, data_vecs) = split_payload_into_data_shards(payload, k);
        let data_refs: Vec<&[u8]> = data_vecs.iter().map(|v| v.as_slice()).collect();
        let all_shards = match self {
            Self::ReedSolomon(rs) => rs.encode_stripe(&data_refs)?,
            Self::Lrc(lrc) => lrc.encode_stripe(&data_refs)?,
        };
        Ok((shard_len, all_shards))
    }

    /// Decodes a stripe payload of `payload_length` bytes from `available_shards`, each of length `shard_len`.
    pub fn decode_stripe_payload(
        &self,
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
        payload_length: usize,
    ) -> Result<Vec<u8>, CodecError> {
        let k = self.data_shards();
        let data_shards = match self {
            Self::ReedSolomon(rs) => rs.decode_data_shards(available_shards, shard_len)?,
            Self::Lrc(lrc) => lrc.decode_data_shards(available_shards, shard_len)?,
        };
        join_data_shards_into_payload(&data_shards, k, shard_len, payload_length)
    }

    /// Reconstructs the specified `target_shards` from `available_shards`, also returning the
    /// [`RepairMethod`] and number of source shards read.
    pub fn repair_shards(
        &self,
        target_shards: &[ShardId],
        available_shards: &[(ShardId, &[u8])],
        shard_len: usize,
    ) -> Result<(ReconstructedShards, RepairMethod), CodecError> {
        match self {
            Self::ReedSolomon(rs) => {
                let out =
                    rs.reconstruct_specific_shards(target_shards, available_shards, shard_len)?;
                Ok((
                    out,
                    RepairMethod::GlobalReconstruction {
                        shards_to_read: rs.data_shards(),
                    },
                ))
            }
            Self::Lrc(lrc) => {
                if target_shards.len() == 1 {
                    let LrcShardRepairOutput {
                        target_shard,
                        bytes,
                        method,
                        ..
                    } = lrc.repair_single_shard(target_shards[0], available_shards, shard_len)?;
                    Ok((vec![(target_shard, bytes)], method))
                } else {
                    let out = lrc.reconstruct_specific_shards(
                        target_shards,
                        available_shards,
                        shard_len,
                    )?;
                    Ok((
                        out,
                        RepairMethod::GlobalReconstruction {
                            shards_to_read: lrc.config().data_shards,
                        },
                    ))
                }
            }
        }
    }
}
