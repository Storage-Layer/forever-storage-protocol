# FSP Phase 3 — Real Byte-Level Erasure Coding Specification

## 1. Finite Field $\text{GF}(2^8)$ Construction

All byte-level erasure coding operations in FSP Phase 3 operate over the Galois field $\text{GF}(2^8) \cong \mathbb{F}_2[x] / \langle p(x) \rangle$ with primitive polynomial:
$$p(x) = x^8 + x^4 + x^3 + x^2 + 1 \quad (\text{hexadecimal } \mathtt{0x11d}, \text{ decimal } 285)$$
and primitive generator element $\alpha = x$ ($\mathtt{0x02}$).

- **Addition & Subtraction:** Bitwise XOR ($a \oplus b$).
- **Multiplication & Division:** Computed via compile-time verified logarithm ($\log_\alpha$) and anti-logarithm ($\alpha^i$) tables and a static $256 \times 256$ lookup table (`MUL_TABLE[a][b]` in [`src/coding/gf256.rs`](file:///home/rade/Project/Jobs/fsp/src/coding/gf256.rs)).
- **Vectorized Inner-Product Slice Accumulation (`gf_mul_add_slice`):**
  For a scalar $c \in \text{GF}(2^8)$ and equal-length slices $\mathbf{y}, \mathbf{x} \in \text{GF}(2^8)^L$:
  $$\mathbf{y}[b] \leftarrow \mathbf{y}[b] \oplus (c \cdot \mathbf{x}[b]) \quad \forall b \in \{0, \dots, L-1\}$$
  - When $c = 0$: no-op.
  - When $c = 1$: 64-bit word-aligned XOR (`u64` chunks).
  - When $c \ge 2$: tight loop over `MUL_TABLE[c]` auto-vectorized / unrolled by LLVM.

---

## 2. Systematic Cauchy Reed–Solomon $\text{RS}(n, k)$

### 2.1 Generator Matrix $G_{\text{RS}} \in \text{GF}(2^8)^{n \times k}$
For $1 \le k \le n \le 255$ (default FSP configuration: $n = 20, k = 12, m = n - k = 8$):
$$G_{\text{RS}} = \begin{bmatrix} I_k \\ C_{m \times k} \end{bmatrix}$$
where:
- Top $k \times k$ block is the identity matrix $I_k$ ($G_{i, j} = \delta_{i, j}$ for $0 \le i, j < k$), ensuring systematic encoding where shards $0 \dots k-1$ store unmodified original data slices.
- Bottom $m \times k$ block ($m = n - k$) is a **Cauchy matrix** over $\text{GF}(2^8)$:
  $$C_{p, j} = \frac{1}{x_p \oplus y_j} \quad \text{for } p \in \{0, \dots, m-1\},\; j \in \{0, \dots, k-1\}$$
  with $x_p = p$ and $y_j = m + j$. Because $\{x_0, \dots, x_{m-1}, y_0, \dots, y_{k-1}\} = \{0, 1, \dots, n-1\}$ are pairwise distinct elements of $\text{GF}(2^8)$ whenever $n \le 256$, every square submatrix of $C$ is nonsingular (the **super-regularity** theorem for Cauchy matrices), and therefore **every $k \times k$ submatrix formed by any $k$ distinct rows of $G_{\text{RS}}$ is strictly invertible** (Maximum Distance Separable, $d_{\min} = n - k + 1 = 9$).

### 2.2 Decoding from Any $k$ Surviving Shards
Given any subset of $k$ distinct valid surviving shards $S = \{i_0, i_1, \dots, i_{k-1}\} \subset \{0, \dots, n-1\}$:
1. Extract the $k \times k$ submatrix $G_S$ whose $r$-th row is row $i_r$ of $G_{\text{RS}}$.
2. Invert $G_S$ over $\text{GF}(2^8)$ via Gauss–Jordan elimination with partial pivoting (`invert_square_matrix`), yielding $G_S^{-1} \in \text{GF}(2^8)^{k \times k}$.
3. For any missing data shard $j \notin S$ ($0 \le j < k$), row $j$ of $G_S^{-1}$ gives the exact linear combination coefficients over the $k$ surviving shards:
   $$D_j = \bigoplus_{r=0}^{k-1} \left(G_S^{-1}\right)_{j, r} \cdot Y_{i_r}$$
4. For any missing parity shard $k + p \notin S$, compute $P_p = \bigoplus_{j=0}^{k-1} C_{p, j} \cdot D_j$ (or directly via the single-pass repair matrix $R_{\text{target}} = G_{\text{target}} \cdot G_S^{-1}$).

---

## 3. Real Byte-Level Local Reconstruction Codes: $\text{LRC}(22, 12, 4)$ and $\text{LRC}(20, 12, 4)$

### 3.1 Code Structure & Generator Matrix $G_{\text{LRC}} \in \text{GF}(2^8)^{n \times k}$
For $\text{LRC}(n, k, l)$ with $k = 12$, locality $l = 4$, $g = k / l = 3$ local groups, and $r_g = n - k - g$ global parities ($r_g = 7$ for $n = 22$; $r_g = 5$ for $n = 20$):
- **Rows $0 \dots 11$ (Systematic Data Shards $D_0 \dots D_{11}$):** $12 \times 12$ identity matrix $I_{12}$, partitioned into 3 disjoint local groups:
  - Group 0: $D_0, D_1, D_2, D_3$
  - Group 1: $D_4, D_5, D_6, D_7$
  - Group 2: $D_8, D_9, D_{10}, D_{11}$
- **Rows $12 \dots 14$ (Local Group XOR Parities $L_0, L_1, L_2$):**
  $$L_r = \bigoplus_{j = 4r}^{4r + 3} D_j \quad (r \in \{0, 1, 2\})$$
  Row $12 + r$ has $1$ in columns $4r \dots 4r + 3$ and $0$ elsewhere.
- **Rows $15 \dots n-1$ (Global Parities $G_0 \dots G_{r_g - 1}$):**
  To ensure that global parity rows remain linearly independent of the local parity equations $L_r = \bigoplus_{j=4r}^{4r+3} D_j$ under every Maximally Recoverable (MR) erasure pattern, the $(p, j)$ entry of the global parity block ($p \in \{0, \dots, r_g - 1\}$, $j \in \{0, \dots, k - 1\}$) is constructed via Cauchy evaluation points where the last column of each local group has zero-sum avoidance:
  $$G_{15 + p, j} = \frac{1}{x_p \oplus y_j}, \quad x_p = 100 + p, \quad y_j = 1 + 5 \cdot (j / 4) + (j \bmod 4)$$
  and verified by rank check over $\text{GF}(2^8)$.

### 3.2 Two-Tier Repair & Decoding Algorithm
1. **Fast Local Single-Erasure Repair ($O(l) = 4$ shards read):**
   Whenever a single shard $s \in \text{Group}_r = \{4r, 4r+1, 4r+2, 4r+3, 12+r\}$ is missing and its $l = 4$ peers in $\text{Group}_r \setminus \{s\}$ are available, $s$ is reconstructed purely by XORing those 4 peers:
   $$\text{Shard}_s = \bigoplus_{u \in \text{Group}_r \setminus \{s\}} \text{Shard}_u$$
   This reads **$4$ shards instead of $12$** (`-66.7%` read I/O, `-61.5%` total repair traffic).
2. **Global MR Decoding (Up to $r_g + g = 10$ erasures in $\text{LRC}(22, 12, 4)$):**
   Because the $l + 1 = 5$ shards of each local group satisfy $\bigoplus_{u \in \text{Group}_r} \text{Shard}_u = \mathbf{0}$, any local group with 0 erasures has 5 surviving shards spanning a 4-dimensional subspace. Therefore, our LRC decoder (`LrcCodec::select_independent_rows` in [`src/coding/lrc.rs`](file:///home/rade/Project/Jobs/fsp/src/coding/lrc.rs)):
   - Selects at most $l = 4$ surviving shards from each local group $r$ (yielding $12 - \sum_{r=0}^2 \max(0, e_r - 1)$ mutually independent rows), and
   - Selects the remaining $\sum_{r=0}^2 \max(0, e_r - 1)$ linearly independent rows from the surviving global parity shards, then performs Gauss–Jordan inversion over $\text{GF}(2^8)$.

---

## 4. Stripe Framing & Padding Specification

Given an object of arbitrary length $L \ge 0$ bytes and configured maximum stripe data size $S_{\text{max}}$ (default $1,048,576$ bytes = $1\text{ MiB}$):
1. If $L = 0$, a single canonical stripe ($s = 0$) with `payload_length = 0` and `shard_length = 64` bytes (all zero bytes) is encoded into $n$ shards.
2. If $L > 0$, the object is split into $N_{\text{stripes}} = \lceil L / S_{\text{max}} \rceil$ stripes. Each stripe $s$ has `payload_length` $L_s \in \{1, \dots, S_{\text{max}}\}$ such that $\sum_s L_s = L$.
3. Within stripe $s$, the per-shard byte length is:
   $$\text{shard\_length}_s = \max\left(64,\; \left\lceil \frac{L_s}{8 \cdot k} \right\rceil \cdot 8\right)$$
4. The $L_s$ payload bytes are copied into a zero-initialized buffer of size $k \cdot \text{shard\_length}_s$, split into $k$ contiguous slices of length $\text{shard\_length}_s$, and encoded into $n$ shards.
5. During reconstruction of stripe $s$, the $k$ decoded data shards ($k \cdot \text{shard\_length}_s$ bytes) are truncated to exactly $L_s$ bytes (`payload_length`), guaranteeing **exact byte-for-byte preservation** for empty files (`0 B`), `1 B`, odd-sized files, and multi-hundred-megabyte files.
