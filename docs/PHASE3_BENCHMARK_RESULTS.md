# FSP Phase 3 — Real Storage Engine & Erasure Coding Benchmark Results

## 1. Benchmark Environment & Methodology

Executed via `/usr/bin/time -v ./target/release/fsp-sim --phase3` ([`run_phase3_benchmarks_and_validation`](file:///home/rade/Project/Jobs/fsp/src/main.rs#L1285-L1765)):
- **OS & Kernel:** Linux (`ext4` local filesystem)
- **Hardware:** 24 logical CPU cores, 62 GiB system RAM
- **Compiler:** `rustc 1.99.1` (`--release` profile, optimized)
- **Peak Resident Set Size (RSS) Across All 100 MiB Benchmarks:** **`11,016 KB` (`10.76 MiB`)**
- **Machine-Readable Artifacts:**
  - [results/phase3/benchmark_config.json](file:///home/rade/Project/Jobs/fsp/results/phase3/benchmark_config.json)
  - [results/phase3/benchmark_results.json](file:///home/rade/Project/Jobs/fsp/results/phase3/benchmark_results.json)
  - [results/phase3/recovery_results.json](file:///home/rade/Project/Jobs/fsp/results/phase3/recovery_results.json)
  - [results/phase3/test_summary.json](file:///home/rade/Project/Jobs/fsp/results/phase3/test_summary.json)

> [!IMPORTANT]
> **Single-Host Filesystem Benchmark Boundary:**
> All measurements below reflect real CPU erasure coding, SHA-256 hashing, and local POSIX filesystem I/O (`write`, `read`, `fsync`, `rename`) on a single host. They must **not** be interpreted as distributed multi-host WAN/LAN network transfer throughput.

---

## 2. Pure In-Memory Erasure Codec Throughput (`100 MiB`, 100 Stripes $\times 1\text{ MiB}$)

Measured on single-threaded in-memory buffers isolating $\text{GF}(2^8)$ matrix multiplication and Gauss–Jordan inversion from disk I/O:

| Coding Scheme | Nominal Overhead ($\Omega = n/k$) | Encoding Throughput ($\text{MiB/s}$) | Degraded Decoding Throughput ($\text{MiB/s}$) | Single-Shard Repair Throughput ($\text{MiB/s}$) | Source Shards Read per Single-Shard Repair |
|---|---|---|---|---|---|
| `Replication_3x` $(3, 1)$ | $3.0000\times$ | `257.49 MiB/s` | `397.93 MiB/s` (2 erasures) | `779.65 MiB/s` | `1` shard |
| $\text{RS}(20, 12)$ | $1.6667\times$ | `99.22 MiB/s` | `111.13 MiB/s` (8 erasures) | `77.41 MiB/s` | `12` shards |
| $\text{LRC}(20, 12, 4)$ | $1.6667\times$ | **`165.33 MiB/s`** (`+66.6%`) | `149.24 MiB/s` (6 erasures) | **`2,601.11 MiB/s`** (`33.6x`) | **`4` shards** |
| $\text{LRC}(22, 12, 4)$ | $1.8333\times$ | **`124.08 MiB/s`** (`+25.1%`) | `101.05 MiB/s` (8 erasures) | **`2,327.92 MiB/s`** (`30.1x`) | **`4` shards** |

**Key Codec Findings:**
- $\text{LRC}(20, 12, 4)$ encodes **66.6% faster** (`165.33 MiB/s` vs `99.22 MiB/s`) and $\text{LRC}(22, 12, 4)$ encodes **25.1% faster** (`124.08 MiB/s`) than $\text{RS}(20, 12)$ because the 3 local group parity shards are computed via 64-bit word-aligned bitwise XOR (`xor_slice_in_place`) rather than $\text{GF}(2^8)$ Cauchy table lookups.
- For single-shard repair, $\text{LRC}(20, 12, 4)$ (`2,601.11 MiB/s`) and $\text{LRC}(22, 12, 4)$ (`2,327.92 MiB/s`) achieve a **$30\times$–$33.6\times$ CPU throughput speedup** over $\text{RS}(20, 12)$ (`77.41 MiB/s`) because local group repair XORs 4 peer shards directly without inverting a $12 \times 12$ $\text{GF}(2^8)$ matrix.

---

## 3. End-to-End Persistent Filesystem Storage Benchmarks

Every `StorageEngine` `PUT` includes:
1. Streaming stripe read + object SHA-256 calculation,
2. Erasure encoding into $n$ shards + per-shard SHA-256 calculation,
3. Writing $n$ `.tmp` shard files to staging + `File::sync_all()` on every shard file,
4. Re-reading all $n$ staged shard files from disk in `Verifying` and recomputing SHA-256,
5. Atomic `rename` of $n$ shard files + directory `sync_all()` + manifest `sync_all()` and atomic `rename`.

| Scheme | Object Size | `PUT` Latency ($\text{ms}$) | `PUT` Throughput ($\text{MiB/s}$) | Degraded `GET` Latency ($\text{ms}$) | Degraded `GET` Throughput ($\text{MiB/s}$) | Single-Shard `REPAIR` Latency ($\text{ms}$) | `PUT` Disk Bytes Written | `REPAIR` Disk Bytes Read | `REPAIR` Disk Bytes Written | Logical Repair Amplification |
|---|---|---|---|---|---|---|---|---|---|---|
| `RawFilesystemCopy` | `1 MiB` | `8.89` | `112.54` | `9.14` | `109.43` | — | `1,048,576` | — | — | `1.00x` |
| `RawFilesystemCopy` | `100 MiB` | `629.87` | `158.76` | `935.51` | `106.89` | — | `104,857,600` | — | — | `1.00x` |
| `Replication_3x` | `0 B` | `25.35` | `0.00` | `2.83` | `0.00` | `3.44` | `192` | `64` | `64` | `1.00x` |
| `Replication_3x` | `1 B` | `25.50` | `0.00` | `3.10` | `0.00` | `3.59` | `192` | `64` | `64` | `1.00x` |
| `Replication_3x` | `4 KiB` | `25.94` | `0.15` | `3.24` | `1.20` | `4.36` | `12,288` | `4,096` | `4,096` | `1.00x` |
| `Replication_3x` | `1 MiB` | `126.33` | `7.92` | `30.90` | `32.37` | `94.35` | `3,145,728` | `1,048,576` | `1,048,576` | `1.00x` |
| $\text{RS}(20, 12)$ | `0 B` | `70.13` | `0.00` | `3.86` | `0.00` | `5.51` | `1,280` | `768` | `64` | `12.00x` |
| $\text{RS}(20, 12)$ | `1 B` | `71.60` | `0.00` | `3.96` | `0.00` | `5.79` | `1,280` | `768` | `64` | `12.00x` |
| $\text{RS}(20, 12)$ | `4 KiB` | `75.01` | `0.05` | `5.14` | `0.76` | `6.15` | `6,880` | `4,128` | `344` | `12.00x` |
| $\text{RS}(20, 12)$ | `1 MiB` | `139.74` | `7.16` | `44.71` | `22.37` | `50.52` | `1,747,680` | `1,048,608` | `87,384` | `12.00x` |
| **$\text{RS}(20, 12)$** | **`100 MiB`** | **`13,206.38`** | **`7.57`** | **`3,370.48`** | **`29.67`** | **`4,371.25`** | **`174,768,000`** | **`104,860,800`** | **`8,738,400`** | **`12.00x`** |
| $\text{LRC}(20, 12, 4)$ | `0 B` | `75.99` | `0.00` | `6.40` | `0.00` | `9.59` | `1,280` | `256` | `64` | **`4.00x`** |
| $\text{LRC}(20, 12, 4)$ | `1 B` | `75.46` | `0.00` | `7.26` | `0.00` | `8.59` | `1,280` | `256` | `64` | **`4.00x`** |
| $\text{LRC}(20, 12, 4)$ | `4 KiB` | `76.76` | `0.05` | `5.78` | `0.68` | `9.50` | `6,880` | `1,376` | `344` | **`4.00x`** |
| $\text{LRC}(20, 12, 4)$ | `1 MiB` | `153.54` | `6.51` | `43.27` | `23.11` | `62.10` | `1,747,680` | **`349,536`** | `87,384` | **`4.00x`** |
| $\text{LRC}(22, 12, 4)$ | `0 B` | `78.35` | `0.00` | `5.60` | `0.00` | `8.20` | `1,408` | `256` | `64` | **`4.00x`** |
| $\text{LRC}(22, 12, 4)$ | `1 B` | `79.18` | `0.00` | `5.49` | `0.00` | `8.19` | `1,408` | `256` | `64` | **`4.00x`** |
| $\text{LRC}(22, 12, 4)$ | `4 KiB` | `80.08` | `0.05` | `5.67` | `0.69` | `8.60` | `7,568` | `1,376` | `344` | **`4.00x`** |
| $\text{LRC}(22, 12, 4)$ | `1 MiB` | `170.80` | `5.85` | `43.81` | `22.83` | `67.35` | `1,922,448` | **`349,536`** | `87,384` | **`4.00x`** |
| **$\text{LRC}(22, 12, 4)$** | **`100 MiB`** | **`15,057.33`** | **`6.64`** | **`4,963.04`** | **`20.15`** | **`4,552.96`** | **`192,244,800`** | **`34,953,600`** | **`8,738,400`** | **`4.00x`** |

---

## 4. Physical I/O Amplification & Memory Analysis

1. **Physical Write Amplification During Crash-Safe `PUT`:**
   - For a `100 MiB` (`104,857,600` B) object under $\text{RS}(20, 12)$, nominal coded shard bytes are $174,768,000\text{ B}$ ($1.6667\times$). Because `WriteTransaction` (`src/storage/transaction.rs`) enforces per-shard `File::sync_all()` (`2,000` `fsync` barriers) plus a full pre-commit verification read pass (`174.77 MB` read back in `Verifying`), the `PUT` latency (`13.21 s`, `7.57 MiB/s`) is dominated by **filesystem journal & `fsync` barrier latency** (`~6.6 ms` per stripe of 20 synced shard files), not CPU encoding (`99.22 MiB/s`, which takes only `1.01 s` for `100 MiB`).
2. **Physical Repair Read Reduction Under $\text{LRC}(22, 12, 4)$ (`-66.7%`):**
   - Repairing 1 missing shard per stripe across a `100 MiB` object (`8,738,400` B of missing shard data across 100 stripes) reads **`104,860,800` B** under $\text{RS}(20, 12)$ (`12.00x` read amplification), versus only **`34,953,600` B** under $\text{LRC}(22, 12, 4)$ (**`4.00x` read amplification — a `66.7%` reduction in repair read I/O**).
3. **Bounded Streaming Memory (`10.76 MiB` Peak RSS for `100 MiB` Objects):**
   - Because `StorageEngine::put_reader` and `reconstruct_object_to_writer` process one `1 MiB` stripe at a time, Peak RSS for the entire `100 MiB` benchmark and 12-step lifecycle validation was **`11,016 KB` (`10.76 MiB`)** — roughly $10\%$ of the `100 MiB` object size.
