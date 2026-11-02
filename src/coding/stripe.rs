//! Deterministic stripe framing, 8-byte aligned shard sizing, and zero-padding for arbitrary
//! object lengths (including `0 B`, `1 B`, odd unaligned sizes, and multi-megabyte streams).

use crate::coding::reed_solomon::CodecError;

/// Default maximum payload bytes per stripe (`1 MiB = 1,048,576` bytes).
pub const DEFAULT_STRIPE_SIZE_BYTES: usize = 1_048_576;

/// Minimum allowed stripe size (`64 B`).
pub const MIN_STRIPE_SIZE_BYTES: usize = 64;

/// Maximum allowed stripe size (`64 MiB = 67,108,864` bytes) to bound memory allocation.
pub const MAX_STRIPE_SIZE_BYTES: usize = 67_108_864;

/// Minimum per-shard byte length (`64 B`, multiple of 8).
pub const MIN_SHARD_LENGTH_BYTES: usize = 64;

/// Computes the deterministic per-shard byte length for a stripe containing `payload_length` bytes
/// across `data_shards` ($k$) systematic data shards.
///
/// Formula:
/// $$\text{shard\_len} = \max\left(64,\; \left\lceil \frac{\text{payload\_length}}{8 \cdot k} \right\rceil \cdot 8\right)$$
#[inline]
pub const fn compute_stripe_shard_length(payload_length: usize, data_shards: usize) -> usize {
    if data_shards == 0 {
        return MIN_SHARD_LENGTH_BYTES;
    }
    let divisor = 8 * data_shards;
    let blocks = payload_length.div_ceil(divisor);
    let aligned = blocks * 8;
    if aligned < MIN_SHARD_LENGTH_BYTES {
        MIN_SHARD_LENGTH_BYTES
    } else {
        aligned
    }
}

/// Splits a raw stripe payload (`0..=stripe_size` bytes) into $k$ equal-length, 8-byte-aligned
/// data shard buffers, zero-padding the final bytes when `payload.len() < k * shard_length`.
pub fn split_payload_into_data_shards(payload: &[u8], data_shards: usize) -> (usize, Vec<Vec<u8>>) {
    let shard_len = compute_stripe_shard_length(payload.len(), data_shards);
    let mut shards = Vec::with_capacity(data_shards);
    let mut offset = 0usize;

    for _ in 0..data_shards {
        let mut buf = vec![0u8; shard_len];
        if offset < payload.len() {
            let copy_len = (payload.len() - offset).min(shard_len);
            buf[..copy_len].copy_from_slice(&payload[offset..offset + copy_len]);
            offset += copy_len;
        }
        shards.push(buf);
    }

    (shard_len, shards)
}

/// Reassembles $k$ decoded data shards of length `shard_len` and truncates them to the exact
/// original `payload_length` bytes of the stripe.
pub fn join_data_shards_into_payload(
    data_shards: &[Vec<u8>],
    expected_k: usize,
    expected_shard_len: usize,
    payload_length: usize,
) -> Result<Vec<u8>, CodecError> {
    if data_shards.len() != expected_k {
        return Err(CodecError::WrongDataShardCount {
            expected: expected_k,
            actual: data_shards.len(),
        });
    }
    for s in data_shards {
        if s.len() != expected_shard_len {
            return Err(CodecError::InvalidShardLength {
                expected: expected_shard_len,
                actual: s.len(),
            });
        }
    }

    let capacity = expected_k * expected_shard_len;
    if payload_length > capacity {
        return Err(CodecError::InvalidShardLength {
            expected: capacity,
            actual: payload_length,
        });
    }

    let mut out = Vec::with_capacity(payload_length);
    let mut remaining = payload_length;
    for s in data_shards {
        if remaining == 0 {
            break;
        }
        let take = remaining.min(s.len());
        out.extend_from_slice(&s[..take]);
        remaining -= take;
    }

    Ok(out)
}
