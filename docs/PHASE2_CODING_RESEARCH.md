# FSP Phase 2: Repair Amplification & Local Reconstruction Codes (LRC) Research

## 1. The Repair Amplification Problem in Reed–Solomon $\text{RS}(20, 12)$

Under a maximum distance separable (MDS) Reed–Solomon code $\text{RS}(n = 20, k = 12)$ over $\mathbb{F}_{2^w}$:
- **Storage Overhead:** $\Omega_{\text{RS}} = \frac{20}{12} = 1.6667\times$.
- **Minimum Distance & Fault Tolerance:** By the Singleton bound, $d_{\min} = n - k + 1 = 9$. The code can recover from **any** $n - k = 8$ simultaneous shard erasures.
- **Single-Shard Repair Amplification:** Reconstructing **one** erased shard of size $S_{\text{shard}} = S_{\text{obj}} / 12$ requires reading $k = 12$ surviving shards ($12 \cdot S_{\text{shard}} = S_{\text{obj}}$ bytes read) and writing $1 \cdot S_{\text{shard}}$ bytes, yielding a **12:1 read amplification** per repaired shard (`total_transfer = 13 * S_shard`).

---

## 2. Information-Theoretic Analysis of Locally Repairable Codes (LRC) with $k = 12, l = 4$

Suppose we partition the $k = 12$ data shards into $g = k / l = 3$ disjoint local groups of size $l = 4$:
- **Group 0:** Data shards $D_0, D_1, D_2, D_3$ + Local Parity $L_0 = D_0 \oplus D_1 \oplus D_2 \oplus D_3$ ($5$ shards).
- **Group 1:** Data shards $D_4, D_5, D_6, D_7$ + Local Parity $L_1 = D_4 \oplus D_5 \oplus D_6 \oplus D_7$ ($5$ shards).
- **Group 2:** Data shards $D_8, D_9, D_{10}, D_{11}$ + Local Parity $L_2 = D_8 \oplus D_9 \oplus D_{10} \oplus D_{11}$ ($5$ shards).
- **Global Parity Shards:** $r_g$ global parity shards $G_0, \dots, G_{r_g - 1}$ computed across all $k = 12$ data shards (as in Pyramid / Azure Huang et al. LRC or Tamo–Barg optimal LRC).

### 2.1 Gopalan–op-Singleton Bound on Minimum Distance $d_{\min}$
For any $(n, k, l)$ code where every symbol belongs to a local parity group of locality $l$ (or where the $k$ information symbols are partitioned into $g = \lceil k / l \rceil$ local groups each with 1 local parity, plus $r_g = n - k - g$ global parities), the minimum distance $d_{\min}$ is bounded above by the Gopalan et al. (2012) bound:
$$d_{\min} \le n - k - \left\lceil \frac{k}{l} \right\rceil + 2 = r_g + 2$$

### 2.2 Proof That $\text{LRC}(n = 20, k = 12, l = 4)$ Cannot Match $\text{RS}(20, 12)$ Arbitrary Erasure Tolerance
For $n = 20, k = 12, l = 4$:
- Number of local groups: $g = 12 / 4 = 3$ (consuming $3$ local parity shards $L_0, L_1, L_2$).
- Remaining budget for global parity shards: $r_g = 20 - 12 - 3 = 5$ global parities ($G_0, \dots, G_4$).
- By the Gopalan bound:
  $$d_{\min}\!\left(\text{LRC}(20, 12, 4)\right) \le 20 - 12 - 3 + 2 = 7$$
- Therefore:
  - **Arbitrary Erasure Tolerance:** $\text{LRC}(20, 12, 4)$ is guaranteed to recover from **any** $d_{\min} - 1 = 6$ arbitrary erasures (compared to $8$ arbitrary erasures for $\text{RS}(20, 12)$).
  - **Information-Theoretic Recoverability Condition for $7$ or $8$ Erasures in $\text{LRC}(20, 12, 4)$:**
    Let $e_j \in \{0, \dots, 5\}$ be the number of erased shards in local group $j \in \{0, 1, 2\}$ (out of its $4$ data + $1$ local parity shards), and let $e_{\text{global}} \in \{0, \dots, 5\}$ be the number of erased global parity shards.
    - In each local group $j$, if $e_j \ge 1$, the single local parity equation repairs 1 erasure (or absorbs 1 degree of freedom), leaving $\max(0, e_j - 1)$ **unresolved data erasures** that must be solved by the surviving global parities!
    - The number of surviving global parities is $5 - e_{\text{global}}$.
    - Consequently, an erasure pattern in maximally recoverable (MR) $\text{LRC}(20, 12, 4)$ is **decodable if and only if**:
      $$\sum_{j=0}^{2} \max(0, e_j - 1) \le 5 - e_{\text{global}} \iff \sum_{j=0}^{2} \max(0, e_j - 1) + e_{\text{global}} \le 5$$
  - **Explicit Unrecoverable 7-Erasure Pattern in $\text{LRC}(20, 12, 4)$:**
    Suppose all $5$ shards of Group 0 ($D_0, D_1, D_2, D_3, L_0$, so $e_0 = 5$) and $2$ global parities ($G_0, G_1$, so $e_{\text{global}} = 2$) are erased ($7$ total erasures, $13 \ge 12$ shards still survive!).
    Then $\max(0, 5 - 1) + 2 = 4 + 2 = 6 > 5$. Even though $13$ shards survive, **two of those surviving shards ($L_1$ and $L_2$) are linearly dependent on already-known intact groups 1 and 2**, leaving only $13 - 2 = 11 < 12$ linearly independent equations! Thus the object is **irrecoverably lost** at $7$ erasures!

---

## 3. Valid LRC Configurations Implemented in `src/coding/`

We implement exact parity-group tracking and exact MR-LRC decodability verification in `src/coding/mod.rs` for two explicit LRC constructions alongside $\text{RS}(n, k)$:

| Coding Scheme | $(n, k, l)$ | Local Parities ($g$) | Global Parities ($r_g$) | Storage Overhead ($\Omega$) | Arbitrary Fault Tolerance ($d_{\min}-1$) | Max Conditionally Recoverable Erasures | Single-Shard Local Repair Read Cost | Global Fallback Repair Read Cost |
|---|---|---|---|---|---|---|---|---|
| `RS(20, 12)` | $(20, 12, -)$ | $0$ | $8$ | $1.6667\times$ | **$8$ (any 8)** | $8$ | N/A ($12$ shards) | $12$ shards |
| `LRC(20, 12, 4)` | $(20, 12, 4)$ | $3$ | $5$ | $1.6667\times$ | **$6$ (any 6)** | $8$ (iff $\ge 1$ per group) | **$4$ shards** (`-61.5%` total transfer) | $12$ shards |
| `LRC(22, 12, 4)` | $(22, 12, 4)$ | $3$ | $7$ | $1.8333\times$ | **$8$ (any 8)** | $10$ (iff $\ge 1$ per group) | **$4$ shards** (`-61.5%` total transfer) | $12$ shards |

### 3.1 Local vs. Global Repair Traffic Accounting
1. **Local Repair (Fast Path — $e_j = 1$ in Local Group $j$):**
   - If a data shard $D_x$ or local parity shard $L_j$ in group $j$ is lost and the other $l = 4$ shards in group $j$ are currently `Online`:
   - Repair reads only $l = 4$ shards ($4 \cdot S_{\text{shard}}$) and writes $1$ shard ($1 \cdot S_{\text{shard}}$), transferring **$5 \cdot S_{\text{shard}}$ bytes** instead of $13 \cdot S_{\text{shard}}$ bytes (**`61.54%` repair bandwidth reduction per local repair**).
2. **Global Reconstruction Fallback ($e_j \ge 2$ in Local Group $j$ or Global Parity $G_m$ Lost):**
   - If $\ge 2$ shards in local group $j$ are simultaneously offline/lost, or if a global parity shard $G_m$ is being repaired, local XOR parity is insufficient.
   - Repair falls back to global reconstruction, reading $k = 12$ linearly independent surviving shards ($12 \cdot S_{\text{shard}}$ read + $1 \cdot S_{\text{shard}}$ write per repaired shard).
