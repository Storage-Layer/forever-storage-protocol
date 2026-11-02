//! Cryptographic content integrity using NIST FIPS PUB 180-4 SHA-256.
//!
//! Trust Boundary:
//! SHA-256 cryptographic digests detect bit-rot, torn writes, truncation, silent storage corruption,
//! and accidental shard substitution. Authenticity against an active adversary capable of modifying
//! both shard files and manifest files requires verifying `object_digest` against a trusted
//! content-addressed identifier or signing the manifest root out-of-band.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Standard NIST FIPS 180-4 SHA-256 initial hash state $H^{(0)}$.
const SHA256_INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Standard NIST FIPS 180-4 SHA-256 round constants $K_0 \dots K_{63}$.
const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// A 256-bit (32-byte) SHA-256 cryptographic digest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    /// Formats the 32-byte digest as a 64-character lowercase hexadecimal string.
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(64);
        for &b in &self.0 {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }

    /// Parses and validates a 64-character hexadecimal string into a [`Sha256Digest`].
    pub fn from_hex(hex: &str) -> Result<Self, String> {
        if hex.len() != 64 {
            return Err(format!(
                "Invalid SHA-256 hex length: expected 64 chars, got {}",
                hex.len()
            ));
        }
        let bytes = hex.as_bytes();
        let mut out = [0u8; 32];
        for i in 0..32 {
            let hi = decode_hex_nibble(bytes[2 * i])?;
            let lo = decode_hex_nibble(bytes[2 * i + 1])?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

fn decode_hex_nibble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("Invalid hex character: 0x{:02x}", b)),
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256({})", self.to_hex())
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// Streaming NIST FIPS 180-4 SHA-256 hasher.
#[derive(Clone)]
pub struct Sha256Hasher {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_bytes: u64,
}

impl Default for Sha256Hasher {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256Hasher {
    /// Creates a new initialized SHA-256 hasher.
    pub const fn new() -> Self {
        Self {
            state: SHA256_INIT,
            buffer: [0u8; 64],
            buffer_len: 0,
            total_bytes: 0,
        }
    }

    /// Feeds arbitrary bytes into the streaming SHA-256 state.
    pub fn update(&mut self, mut data: &[u8]) {
        self.total_bytes = self.total_bytes.wrapping_add(data.len() as u64);

        if self.buffer_len > 0 {
            let needed = 64 - self.buffer_len;
            if data.len() < needed {
                self.buffer[self.buffer_len..self.buffer_len + data.len()].copy_from_slice(data);
                self.buffer_len += data.len();
                return;
            }
            self.buffer[self.buffer_len..64].copy_from_slice(&data[..needed]);
            let block = self.buffer;
            compress_block(&mut self.state, &block);
            self.buffer_len = 0;
            data = &data[needed..];
        }

        while data.len() >= 64 {
            let block: &[u8; 64] = data[..64].try_into().unwrap();
            compress_block(&mut self.state, block);
            data = &data[64..];
        }

        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffer_len = data.len();
        }
    }

    /// Finalizes the SHA-256 padding and returns the 32-byte [`Sha256Digest`].
    pub fn finalize(mut self) -> Sha256Digest {
        let bit_len = self.total_bytes.wrapping_mul(8);

        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;

        if self.buffer_len > 56 {
            for b in &mut self.buffer[self.buffer_len..64] {
                *b = 0;
            }
            let block = self.buffer;
            compress_block(&mut self.state, &block);
            self.buffer_len = 0;
        }

        for b in &mut self.buffer[self.buffer_len..56] {
            *b = 0;
        }
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let final_block = self.buffer;
        compress_block(&mut self.state, &final_block);

        let mut out = [0u8; 32];
        for (i, &word) in self.state.iter().enumerate() {
            out[4 * i..4 * i + 4].copy_from_slice(&word.to_be_bytes());
        }
        Sha256Digest(out)
    }
}

#[inline]
fn compress_block(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes([
            block[4 * i],
            block[4 * i + 1],
            block[4 * i + 2],
            block[4 * i + 3],
        ]);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let mut a = state[0];
    let mut b = state[1];
    let mut c = state[2];
    let mut d = state[3];
    let mut e = state[4];
    let mut f = state[5];
    let mut g = state[6];
    let mut h = state[7];

    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let temp1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(SHA256_K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temp1);
        d = c;
        c = b;
        b = a;
        a = temp1.wrapping_add(temp2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

/// Computes the SHA-256 digest of an in-memory byte slice.
pub fn sha256_bytes(data: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

/// Streams a file from disk in bounded 64 KiB chunks and returns `(byte_length, sha256_digest)`.
pub fn sha256_file(path: &Path) -> io::Result<(u64, Sha256Digest)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256Hasher::new();
    let mut buf = [0u8; 65_536];
    let mut total_len = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total_len += n as u64;
    }
    Ok((total_len, hasher.finalize()))
}
