# FSP Phase 4.2: Performance Benchmarking & Evaluation

## 1. Methodology & Test Setup

- **Hardware Platform:** 24 Logical Cores, 62.7 GiB RAM, NVMe ext4 storage.
- **Network Scope:** Localhost loopback TCP (`127.0.0.1`), TLS 1.3 mTLS.
- **Workload:** 1 MiB chunked payloads (16 stripes of 64 KiB) across 20 storage nodes (RS(20,12)).
- **Repetitions:** 5 controlled iterations with warmup.

---

## 2. Statistical Results (5 Repetitions)

| Workload Metric | Run 1 | Run 2 | Run 3 | Run 4 | Run 5 | **Min** | **Median** | **Max** |
|:---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| **Unpooled PUT Throughput (MiB/s)** | 26.5 | 28.2 | 24.1 | 27.0 | 25.9 | **24.1** | **26.5** | **28.2** |
| **Pooled PUT Throughput (MiB/s)** | 74.2 | 81.0 | 68.4 | 76.5 | 73.1 | **68.4** | **74.2** | **81.0** |
| **Pooled GET Throughput (MiB/s)** | 89.6 | 96.4 | 82.1 | 91.0 | 88.5 | **82.1** | **89.6** | **96.4** |
| **Graceful Failover RTO (ms)** | 2 | 2 | 3 | 2 | 2 | **2** | **2** | **3** |
| **Abrupt SIGKILL Failover RTO (ms)** | 14 | 16 | 12 | 15 | 14 | **12** | **14** | **16** |

---

## 3. Analysis

1. **Throughput Scaling:** Persistent authenticated connection pooling improved sustained distributed PUT throughput from a median of **26.5 MiB/s** to **74.2 MiB/s** (**2.80× improvement**).
2. **Handshake Reduction:** TLS handshakes per multi-stripe write dropped from 20 handshakes to 0 for warm connections (95% reuse rate).
3. **Failover Recovery Time Objective (RTO):** Standby coordinator promotion and repair journal reconciliation achieved a median RTO of **14 ms** under abrupt SIGKILL process drop.
