//! Finite field $\text{GF}(2^8)$ arithmetic and matrix algebra over primitive polynomial
//! $p(x) = x^8 + x^4 + x^3 + x^2 + 1$ (`0x11d`).

/// Primitive polynomial $p(x) = x^8 + x^4 + x^3 + x^2 + 1 = \mathtt{0x11d}$.
pub const GF256_POLY: u16 = 0x11d;

struct GfTables {
    exp: [u8; 512],
    log: [u8; 256],
    inv: [u8; 256],
    mul: [[u8; 256]; 256],
}

const fn build_gf_tables() -> GfTables {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut inv = [0u8; 256];
    let mut mul = [[0u8; 256]; 256];

    let mut x: u16 = 1;
    let mut i = 0usize;
    while i < 255 {
        exp[i] = x as u8;
        exp[i + 255] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if (x & 0x100) != 0 {
            x ^= GF256_POLY;
        }
        i += 1;
    }
    exp[510] = exp[0];
    exp[511] = exp[1];

    let mut a = 1usize;
    while a < 256 {
        let log_a = log[a] as usize;
        let inv_log = if log_a == 0 { 0 } else { 255 - log_a };
        inv[a] = exp[inv_log];

        let mut b = 1usize;
        while b < 256 {
            let log_b = log[b] as usize;
            mul[a][b] = exp[log_a + log_b];
            b += 1;
        }
        a += 1;
    }

    GfTables { exp, log, inv, mul }
}

static TABLES: GfTables = build_gf_tables();

/// Addition / subtraction in $\text{GF}(2^8)$ (bitwise XOR).
#[inline(always)]
pub const fn gf_add(a: u8, b: u8) -> u8 {
    a ^ b
}

/// Multiplication in $\text{GF}(2^8)$.
#[inline(always)]
pub fn gf_mul(a: u8, b: u8) -> u8 {
    TABLES.mul[a as usize][b as usize]
}

/// Multiplicative inverse $a^{-1}$ in $\text{GF}(2^8)$. Panics if `a == 0`.
#[inline(always)]
pub fn gf_inv(a: u8) -> u8 {
    assert!(a != 0, "Cannot invert 0 in GF(2^8)");
    TABLES.inv[a as usize]
}

/// Division $a / b$ in $\text{GF}(2^8)$. Panics if `b == 0`.
#[inline(always)]
pub fn gf_div(a: u8, b: u8) -> u8 {
    assert!(b != 0, "Division by 0 in GF(2^8)");
    if a == 0 {
        0
    } else {
        let log_a = TABLES.log[a as usize] as usize;
        let log_b = TABLES.log[b as usize] as usize;
        TABLES.exp[log_a + 255 - log_b]
    }
}

/// Generator power $\alpha^i$ in $\text{GF}(2^8)$.
#[inline(always)]
pub fn gf_exp(power: usize) -> u8 {
    TABLES.exp[power % 255]
}

/// Computes `dst[i] ^= coeff * src[i]` across equal-length slices `dst` and `src`.
#[inline]
pub fn gf_mul_add_slice(dst: &mut [u8], src: &[u8], coeff: u8) {
    debug_assert_eq!(dst.len(), src.len());
    if coeff == 0 {
        return;
    }
    if coeff == 1 {
        xor_slice_in_place(dst, src);
        return;
    }
    let mul_row = &TABLES.mul[coeff as usize];
    for (d, &s) in dst.iter_mut().zip(src.iter()) {
        *d ^= mul_row[s as usize];
    }
}

/// Fast 64-bit chunked XOR `dst[i] ^= src[i]`.
#[inline]
pub fn xor_slice_in_place(dst: &mut [u8], src: &[u8]) {
    debug_assert_eq!(dst.len(), src.len());
    let len = dst.len();
    let chunks = len / 8;
    for c in 0..chunks {
        let offset = c * 8;
        let d_word = u64::from_ne_bytes(dst[offset..offset + 8].try_into().unwrap());
        let s_word = u64::from_ne_bytes(src[offset..offset + 8].try_into().unwrap());
        dst[offset..offset + 8].copy_from_slice(&(d_word ^ s_word).to_ne_bytes());
    }
    for i in (chunks * 8)..len {
        dst[i] ^= src[i];
    }
}

/// Dense matrix over $\text{GF}(2^8)$ stored in row-major order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GfMatrix {
    pub rows: usize,
    pub cols: usize,
    pub data: Vec<u8>,
}

impl GfMatrix {
    /// Creates a zero-initialized `rows x cols` matrix over $\text{GF}(2^8)$.
    pub fn zeros(rows: usize, cols: usize) -> Self {
        Self {
            rows,
            cols,
            data: vec![0u8; rows * cols],
        }
    }

    /// Creates a `k x k` identity matrix over $\text{GF}(2^8)$.
    pub fn identity(k: usize) -> Self {
        let mut mat = Self::zeros(k, k);
        for i in 0..k {
            mat.set(i, i, 1);
        }
        mat
    }

    #[inline(always)]
    pub fn get(&self, r: usize, c: usize) -> u8 {
        self.data[r * self.cols + c]
    }

    #[inline(always)]
    pub fn set(&mut self, r: usize, c: usize, val: u8) {
        self.data[r * self.cols + c] = val;
    }

    #[inline(always)]
    pub fn row(&self, r: usize) -> &[u8] {
        &self.data[r * self.cols..(r + 1) * self.cols]
    }

    /// Extracts the submatrix formed by `row_indices`.
    pub fn submatrix_rows(&self, row_indices: &[usize]) -> Self {
        let mut sub = Self::zeros(row_indices.len(), self.cols);
        for (dst_r, &src_r) in row_indices.iter().enumerate() {
            let src_slice = self.row(src_r);
            sub.data[dst_r * self.cols..(dst_r + 1) * self.cols].copy_from_slice(src_slice);
        }
        sub
    }

    /// Matrix multiplication `self * rhs` over $\text{GF}(2^8)$.
    pub fn multiply(&self, rhs: &Self) -> Self {
        assert_eq!(self.cols, rhs.rows);
        let mut out = Self::zeros(self.rows, rhs.cols);
        for i in 0..self.rows {
            for k in 0..self.cols {
                let a = self.get(i, k);
                if a != 0 {
                    for j in 0..rhs.cols {
                        let prod = gf_mul(a, rhs.get(k, j));
                        let cur = out.get(i, j);
                        out.set(i, j, cur ^ prod);
                    }
                }
            }
        }
        out
    }

    /// Computes the rank of this matrix over $\text{GF}(2^8)$ via Gaussian elimination.
    pub fn rank(&self) -> usize {
        let mut work = self.clone();
        let mut pivot_row = 0usize;
        for col in 0..work.cols {
            if pivot_row >= work.rows {
                break;
            }
            let mut found = None;
            for r in pivot_row..work.rows {
                if work.get(r, col) != 0 {
                    found = Some(r);
                    break;
                }
            }
            let Some(swap_r) = found else {
                continue;
            };
            if swap_r != pivot_row {
                for c in 0..work.cols {
                    let tmp = work.get(pivot_row, c);
                    work.set(pivot_row, c, work.get(swap_r, c));
                    work.set(swap_r, c, tmp);
                }
            }
            let inv_pivot = gf_inv(work.get(pivot_row, col));
            for c in col..work.cols {
                work.set(pivot_row, c, gf_mul(work.get(pivot_row, c), inv_pivot));
            }
            for r in (pivot_row + 1)..work.rows {
                let factor = work.get(r, col);
                if factor != 0 {
                    for c in col..work.cols {
                        let p_val = work.get(pivot_row, c);
                        let cur = work.get(r, c);
                        work.set(r, c, cur ^ gf_mul(factor, p_val));
                    }
                }
            }
            pivot_row += 1;
        }
        pivot_row
    }

    /// Inverts a square $k \times k$ matrix over $\text{GF}(2^8)$ via Gauss–Jordan elimination.
    /// Returns `None` iff the matrix is singular ($\text{rank} < k$).
    pub fn invert(&self) -> Option<Self> {
        if self.rows != self.cols || self.rows == 0 {
            return None;
        }
        let k = self.rows;
        let mut work = self.clone();
        let mut inv = Self::identity(k);

        for i in 0..k {
            // Find non-zero pivot in column i at or below row i
            let mut pivot_row = None;
            for r in i..k {
                if work.get(r, i) != 0 {
                    pivot_row = Some(r);
                    break;
                }
            }
            let swap_r = pivot_row?;
            if swap_r != i {
                for c in 0..k {
                    let tmp_w = work.get(i, c);
                    work.set(i, c, work.get(swap_r, c));
                    work.set(swap_r, c, tmp_w);

                    let tmp_inv = inv.get(i, c);
                    inv.set(i, c, inv.get(swap_r, c));
                    inv.set(swap_r, c, tmp_inv);
                }
            }

            let pivot_val = work.get(i, i);
            let inv_pivot = gf_inv(pivot_val);
            if inv_pivot != 1 {
                for c in 0..k {
                    work.set(i, c, gf_mul(work.get(i, c), inv_pivot));
                    inv.set(i, c, gf_mul(inv.get(i, c), inv_pivot));
                }
            }

            // Eliminate all other rows (above and below)
            for r in 0..k {
                if r == i {
                    continue;
                }
                let factor = work.get(r, i);
                if factor != 0 {
                    for c in 0..k {
                        let w_ic = work.get(i, c);
                        let inv_ic = inv.get(i, c);
                        work.set(r, c, work.get(r, c) ^ gf_mul(factor, w_ic));
                        inv.set(r, c, inv.get(r, c) ^ gf_mul(factor, inv_ic));
                    }
                }
            }
        }

        Some(inv)
    }
}
