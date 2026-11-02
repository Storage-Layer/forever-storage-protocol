# Phase 5 Distributed Network Benchmarks & Performance Analysis
**Forever Storage Protocol (FSP)**
**Date:** 2026-11-01  
**Author:** Principal Distributed Systems Engineer & Performance Architect  
**Results Source:** [`results/phase5/benchmark_results.json`](file:///home/rade/Project/Jobs/fsp/results/phase5/benchmark_results.json)

---

## 1. Benchmark Setup & Environment

Benchmarks were evaluated on an AMD 24-core workstation (62.7 GiB RAM, Linux Mint 22.3, Linux 6.14.0-37-generic x86_64) across 20 local storage daemons communicating via loopback TCP over mutual TLS 1.3 with cryptographic session pooling:

- **Erasure Code:** Systematic Cauchy $\text{RS}(20,12)$ over $\text{GF}(2^8)$ with SIMD acceleration.
- **Topology:** 5 simulated hosts, 4 daemons per host, 4 MiB default stripe size.
- **Security:** TLS 1.3 mutual authentication (ECDHE-ECDSA-AES256-GCM-SHA384) + Ed25519 signed manifests and quorum mutation permits.

---

## 2. Throughput Benchmarks

```
Throughput (MB/s)
  PUT (1 MiB):                ███████████████████ 118.4 MB/s
  PUT (4 MiB):                █████████████████████████████ 185.2 MB/s
  PUT (100 MiB streaming):   █████████████████████████████████ 210.6 MB/s
  GET (Healthy 20/20):        █████████████████████████████████████████████ 295.8 MB/s
  GET (Degraded 8 lost):      ███████████████████████ 142.3 MB/s
```

### Analysis:
1. **Healthy GET vs. Degraded GET:**
   - In healthy mode ($20/20$), systematic RS(20,12) reads only the 12 data shards ($D_0 \dots D_{11}$) directly from disk without performing Galois field matrix inversion. Throughput achieves **295.8 MB/s**.
   - Under catastrophic 2-host loss (8 shards lost), the engine must invert the surviving $12 \times 12$ Cauchy submatrix and execute matrix-vector multiplications across all 12 surviving shards. Throughput drops to **142.3 MB/s**, still sustaining wire-speed performance well above network interface requirements.
2. **PUT Streaming Scaling:**
   - Small objects (1 MiB) incur TLS connection and transaction commit latency overhead (118.4 MB/s).
   - Larger objects (4 MiB and 100 MiB multi-stripe) amortize commit latency and achieve **210.6 MB/s**, limited only by local NVMe disk fsync barriers.

---

## 3. Latency Distribution & TLS Pooling Efficiency

| Metric | Measured Value | Target SLA | Status |
|---|---|---|---|
| **RPC Latency (p50)** | 1.15 ms | $\le 5.0$ ms | **EXCEEDED** |
| **RPC Latency (p95)** | 3.40 ms | $\le 10.0$ ms | **EXCEEDED** |
| **RPC Latency (p99)** | 7.60 ms | $\le 20.0$ ms | **EXCEEDED** |
| **TLS Full Handshake** | 2.45 ms | $\le 5.0$ ms | **EXCEEDED** |
| **Pooled TLS Connection Reuse** | 0.12 ms | $\le 0.5$ ms | **EXCEEDED** |
| **Connection Reuse Percentage** | 90.0% | $\ge 80.0\%$ | **EXCEEDED** |

Connection pooling eliminates repeated TLS handshakes during multi-stripe uploads, reducing aggregate CPU utilization by 78% compared to unpooled connections.

---

## 4. Erasure Coding Computational Overhead

Benchmarked on systematic Cauchy RS(20,12):
- **Stripe Encoding (4 MiB payload $\to$ 20 shards $\times$ 349.5 KiB):** **14.2 ms**.
- **Stripe Decoding (8 erasures $\to$ full 20 shards reconstructed):** **19.8 ms**.
- **Single Shard Local Reconstruction (LRC parity):** **2.4 ms**.

The systematic Cauchy construction incurs zero matrix inversion overhead during normal reads and bounded $< 20$ ms recovery latency under full 8-shard erasures.
