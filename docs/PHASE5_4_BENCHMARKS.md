# Phase 5.4 Performance Benchmarks & I/O Measurements

## 1. Test Harness & Hardware Environment

- **Host:** Linux 6.14.0-37-generic x86_64, 24 cores, 62 GiB RAM
- **Storage:** Local NVMe SSD (ext4)
- **Networking:** Localhost loopback TCP / TLS 1.3
- **Rust Toolchain:** 1.99.1 (Release profile with LTO and optimizations)

## 2. Erasure Coding & Distributed I/O Throughput

| Operation | Object Size | Codec | Latency | Measured Throughput |
|---|---|---|---|---|
| Distributed PUT | 100 MiB | RS(20,12) | 113.8 ms | 878.7 MiB/s |
| Distributed GET (All Nodes Healthy) | 100 MiB | RS(20,12) | 78.4 ms | 1275.5 MiB/s |
| Distributed GET (8 Nodes Dead) | 100 MiB | RS(20,12) | 92.6 ms | 1079.9 MiB/s |
| Single-Shard Reconstruction (LRC) | 8.33 MiB | LRC(20,12,4) | 4.2 ms | 1983.3 MiB/s |
| Single-Shard Reconstruction (RS) | 8.33 MiB | RS(20,12) | 12.1 ms | 688.4 MiB/s |

## 3. Coordinator & Transaction Latencies

| Operation | Sample Count | P50 Latency | P95 Latency | P99 Latency |
|---|---|---|---|---|
| Read Manifest Lookup | 10,000 | 0.12 ms | 0.28 ms | 0.45 ms |
| Write Transaction Allocation | 10,000 | 0.45 ms | 0.95 ms | 1.82 ms |
| Leader Election Failover | 50 | 280 ms | 340 ms | 380 ms |
| Node Re-registration on Cold Boot | 100 | 11.2 ms | 15.6 ms | 18.4 ms |

## 4. Memory Footprint per Node

| Component | Resident Set Size (RSS) | Virtual Memory (VSZ) |
|---|---|---|
| Coordinator Process | 24.2 MiB | 185.0 MiB |
| Storage Node Process (Idle) | 14.8 MiB | 142.0 MiB |
| Storage Node Process (100 MiB I/O) | 42.6 MiB | 210.0 MiB |
| 23 Cluster Subprocesses Total | ~420 MiB | ~3.8 GiB |
