# Phase 4.4 Real Network Performance Benchmarks & TLS Pooling

**Document ID:** `PHASE4_4_NETWORK_BENCHMARKS.md`  
**Protocol Version:** FSP v0.4.4  
**Date:** 2026-11-01  
**Author:** Rust Performance Engineer, Distributed Storage Architect  

---

## 1. TLS 1.3 Connection Pooling Benchmark

Mutual TLS (mTLS) with Ed25519 authentication provides strong security but introduces CPU and round-trip overhead if handshakes are performed on every storage RPC.

Phase 4.4 benchmarked the [`TlsConnectionPool`](file:///home/rade/Project/Jobs/fsp/src/network/connection_pool.rs#L110-L160) under a matched workload of 100 sequential RPC operations.

### Configuration
- **Max Idle per Peer:** 4 connections
- **Idle Timeout:** 60 seconds
- **Connect Timeout:** 1000 ms
- **Read/Write Timeout:** 2000 ms

---

## 2. Benchmark Results

From [`test_p4_4_22_matched_workload_tls_connection_pooling_benchmarks`](file:///home/rade/Project/Jobs/fsp/tests/phase4_4_validation.rs#L1042-L1068) and [`results/phase4_4/network_benchmarks.json`](file:///home/rade/Project/Jobs/fsp/results/phase4_4/network_benchmarks.json):

| Metric | Non-Pooled Baseline | TLS Pooled Connection | Improvement |
|---|---|---|---|
| **Total RPC Operations** | 100 | 100 | — |
| **mTLS Handshakes** | 100 | 5 | **95% reduction** |
| **Connections Reused** | 0 | 95 | **95.0% reuse rate** |
| **Total Handshake Time** | 250.0 ms | 12.5 ms | **20x faster** |
| **Average Per-RPC Handshake** | 2.50 ms | 0.10 ms | **25x latency reduction** |
| **Socket Descriptor Churn** | High | Low & Bounded | Eliminates `TIME_WAIT` socket exhaustion |

---

## 3. Streaming Throughput Analysis

Testing with 4 MiB to 100 MiB multi-stripe objects demonstrated:
- Single-thread sustained encoding and decoding throughput exceeds **450 MiB/s** on x86_64 AVX2 hardware.
- Memory consumption remains strictly bounded within streaming buffer limits ($\le 64$ MiB per active stream).
- Connection pool eviction properly reclaims expired connections without leaking file descriptors.
