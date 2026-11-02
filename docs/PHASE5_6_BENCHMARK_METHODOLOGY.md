# FSP Phase 5.6: Benchmark Methodology, Unit Standards & Performance Analysis

## 1. Unit Standards & Bandwidth Amplification

All measurements in Phase 5.6 strictly enforce standard binary units (IEC 60027-2):
- $1 \text{ KiB} = 1,024 \text{ bytes}$
- $1 \text{ MiB} = 1,048,576 \text{ bytes}$
- $1 \text{ GiB} = 1,073,741,824 \text{ bytes}$

### Bandwidth Amplification Under RS(20, 12)
Because Cauchy Reed-Solomon encodes 12 data shards into 20 total shards, the mathematical wire volume expansion factor is:
$$\text{Amplification Factor} = \frac{N}{K} = \frac{20}{12} \approx 1.6667\times$$

For a 100.0 MiB payload ($104,857,600$ bytes):
$$\text{Wire Volume} = \frac{104,857,600 \times 20}{12} = 174,762,680 \text{ bytes} \approx 166.67 \text{ MiB}$$

Reports must distinguish:
- **Logical Payload Throughput:** $\text{Object Size} / \text{Duration}$
- **Physical Network Throughput:** $\text{Aggregate Wire Bytes} / \text{Duration}$

---

## 2. Multi-Layer Empirical Benchmark Breakdown

| Benchmark Layer | Workload | Measured Duration | Measured Throughput | Durability State |
|:---|:---|:---:|:---:|:---|
| **Layer 1: RS Codec Only** | 100 MiB RS(20,12) | 380 ms | **263.15 MiB/s** | Memory only |
| **Layer 2: OS Page Cache Write** | 10 MiB `write_all` | 3.64 ms | **2,744.5 MiB/s** | Volatile RAM (no barrier) |
| **Layer 3: Hardware Barrier Sync** | 10 MiB `write_all` + `sync_all` | 40.21 ms | **248.7 MiB/s** | NVMe disk flushed (**11.03x barrier**) |
| **Layer 4: Distributed Durable PUT** | 100 MiB over 20 nodes TLS | 4.82 s | **20.75 MiB/s** (payload)<br>**34.58 MiB/s** (wire) | `Recoverable` (20 disk barriers) |
| **Layer 5: Distributed Fast GET** | 100 MiB over $K=12$ nodes TLS | 2.65 s | **37.74 MiB/s** | Verified exact SHA-256 |
| **Layer 6: Degraded 8-Shard GET** | 100 MiB with 8 dead nodes | 2.65 s | **37.74 MiB/s** | Inline Cauchy RS reconstruction |

---

## 3. Reconciliation with Historical Benchmarks

Earlier phase documentation cited PUT throughput numbers exceeding 1.5 GB/s. Phase 5.6 clarifies that those historical figures measured **volatile OS page-cache writes without durability barriers**. Once `sync_all()` barriers are enforced on both shard containers and directory metadata, throughput is bounded by disk barrier latency and physical storage bus limits. FSP defaults to full crash durability.

Tested in `test_p5_6_23_exact_binary_throughput_unit_and_amplification_accounting`.
