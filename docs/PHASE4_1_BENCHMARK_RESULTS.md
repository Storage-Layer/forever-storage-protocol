# FSP Phase 4.1: Benchmark Results & Performance Evaluation

## 1. Methodology & Test Harness

All benchmarks were evaluated on a Linux x86_64 workstation with 24 logical cores, 62.7 GiB RAM, and ext4 NVMe backing storage under loopback TCP networking (`127.0.0.1`). 

### Benchmark Scenarios:
1. **End-to-End Distributed Repair Duration:** Time required to detect 8 erased shards across 28 nodes (RS(20,12)), reconstruct them from the 12 surviving shards, transmit the newly generated shards to 8 dedicated spare nodes over mTLS, write them to disk with multi-stripe v2 container headers, and atomically commit the updated signed manifest.
2. **Distributed PUT / GET Throughput:** Sustained bandwidth for chunked 32 KiB payloads dispersed across 20 primary storage nodes over mTLS.
3. **Node Idempotency Lookup Latency:** Sub-millisecond on-disk verification and receipt lookup under [`DurableIdempotencyStore`](file:///home/rade/Project/Jobs/fsp/src/node/idempotency.rs).

---

## 2. Statistical Breakdown (5-Run Evaluation)

| Metric | Run 1 | Run 2 | Run 3 | Run 4 | Run 5 | **Min** | **Median** | **Max** |
|:---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| **8-Shard Repair Duration (s)** | 1.34 | 1.25 | 1.48 | 1.31 | 1.39 | **1.25** | **1.34** | **1.48** |
| **Distributed Throughput (MiB/s)** | 26.5 | 27.8 | 24.1 | 28.2 | 25.9 | **24.1** | **26.5** | **28.2** |
| **Idempotency Lookup Latency ($\mu$s)** | 14.5 | 13.1 | 18.2 | 12.0 | 15.0 | **12.0** | **14.5** | **18.2** |

---

## 3. Performance Analysis

### 3.1 Repair Duration Breakdown
- **Reconstruction Phase:** Reed-Solomon decoding of 12 surviving 32 KiB stripes takes $< 1.2\text{ ms}$ CPU time.
- **Network Dispatch Phase:** Parallel dispatch across 8 independent spare daemon connections via loopback TLS 1.3 accounts for $\approx 65\text{ ms}$.
- **Storage & Journal Fsync Phase:** Durable disk writes (8 shard containers + coordinator repair journal transitions + generation watermarking) comprise the remaining time, averaging $\approx 1.25\text{ s}$ due to synchronized filesystem commits.

### 3.2 Throughput Characteristics
- Distributed PUT throughput achieves a median of **26.5 MiB/s** over loopback TCP with full mTLS 1.3 encryption and per-stripe container checksumming.
- The primary gating factor is synchronous connection setup and serial TLS handshake establishment per RPC call in the client test harness. Persistent connection pooling in future revisions will significantly raise network throughput.

### 3.3 Idempotency Store Overhead
- With in-memory index caching backed by append-only `.rec` journals, idempotency lookups execute in a median of **14.5 microseconds**.
- Duplicate request rejection adds negligible overhead to incoming storage operations.
