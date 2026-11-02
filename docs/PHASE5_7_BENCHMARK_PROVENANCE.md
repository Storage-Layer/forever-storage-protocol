# Phase 5.7 Benchmark Provenance & Wire Throughput Accounting
## Architectural Trace, Boundary Classification & Mathematical Analysis

**Repository:** `/home/rade/Project/Jobs/fsp`  
**Milestone:** Phase 5.7 Hotfix v2  
**Physical LAN Pilot Status:** `NOT EXECUTED`  
**Production Readiness:** `NOT ESTABLISHED`  

---

## 1. Executive Summary

This provenance audit formally defines the execution environment, boundary conditions, and accounting metrics of the FSP benchmark suite (`fsp-sim --phase5-bench`).

The historical benchmark reported **18.72 MiB/s payload throughput** and **31.21 MiB/s wire throughput** for a 100 MiB distributed PUT. This audit clarifies that while disk persistence is authentic (persisting to NVMe containers with hardware `sync_all` barriers), the transport layer utilized in-process Tokio channels. Consequently, **31.21 MiB/s represents the theoretical nominal erasure-coded rate**, and physical network interface bytes are explicitly classified as:

$$\mathbf{ACTUAL\_WIRE\_BYTES: NOT\ MEASURED}$$

---

## 2. Benchmark Architecture & Boundary Classification

| Dimension | Measured Implementation | Boundary Classification |
| :--- | :--- | :--- |
| **Command Invocation** | `cargo run --release --bin fsp-sim -- --phase5-bench` | Single OS process harness |
| **Node Architecture** | 20 logical storage nodes (`SimulatedStorageNode`) | In-process concurrent Tokio actors |
| **Storage Backend** | Append-only container files on local filesystem | Real NVMe disk writes with `fdatasync()` barriers |
| **Inter-Node Transport** | Tokio async memory channels (`mpsc::channel`) | In-memory message passing; bypasses Linux socket stack |
| **Network Interfaces** | None traversed | Zero physical Ethernet/Wi-Fi NIC packets generated |
| **Erasure Coding Scheme**| Cauchy Reed-Solomon $\text{RS}(20,12)$ | $K=12$ data shards, $M=8$ parity shards ($1.6667\times$ overhead) |
| **Payload Throughput** | **18.72 MiB/s** median (100 MiB payload in 5.34 s) | Genuine measured client write rate to durable storage |
| **Nominal Wire Rate** | **31.21 MiB/s** ($18.72 \times \frac{20}{12}$) | Mathematical calculation of erasure-coded ingress volume |
| **Physical Wire Bytes** | **`ACTUAL_WIRE_BYTES: NOT MEASURED`** | No hardware NIC counters or socket buffers captured |

---

## 3. Recomputed Workload Statistics Across 5 Trials

All trials enforced binary mebibytes ($1\text{ MiB} = 1,048,576\text{ bytes}$). Raw per-trial samples recorded in [`results/phase5_7/verification_hotfix_v2/run_01/benchmark_reconciliation.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/verification_hotfix_v2/run_01/benchmark_reconciliation.json):

| Workload | Execution Scope | Payload | Min (s) | Median (s) | Max (s) | Mean (s) | StdDev (s) | Median Payload Throughput | Calculated Nominal Wire Rate | Actual Physical Wire Bytes |
| :--- | :--- | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: | :---: |
| **10 MiB Buffered Write** | Host NVMe (Page cache) | 10 MiB | 0.0077 | 0.0078 | 0.0118 | 0.0087 | 0.0016 | **1278.29 MiB/s** | N/A | N/A |
| **10 MiB Synchronized Write** | Host NVMe (`sync_all` barrier) | 10 MiB | 0.0600 | 0.0667 | 0.0970 | 0.0719 | 0.0134 | **150.01 MiB/s** | N/A | N/A |
| **100 MiB RS(20,12) Encoding** | In-Memory CPU Galois field | 100 MiB | 0.9860 | 1.1282 | 1.1530 | 1.0981 | 0.0594 | **88.64 MiB/s** | N/A | N/A |
| **100 MiB Degraded Reconstruct**| In-Memory CPU (8 missing shards) | 100 MiB | 0.9091 | 0.9254 | 0.9582 | 0.9287 | 0.0175 | **108.06 MiB/s** | N/A | N/A |
| **100 MiB Distributed PUT** | In-process 20-node cluster on NVMe | 100 MiB | 5.2582 | 5.3410 | 5.5336 | 5.3657 | 0.1066 | **18.72 MiB/s** | **31.21 MiB/s** (nominal calculated) | **ACTUAL_WIRE_BYTES: NOT MEASURED** |
| **100 MiB Distributed GET** | In-process 20-node cluster on NVMe | 100 MiB | 8.9911 | 10.1291 | 10.2472 | 9.8434 | 0.4764 | **9.87 MiB/s** | **9.87 MiB/s** (nominal calculated) | **ACTUAL_WIRE_BYTES: NOT MEASURED** |

---

## 4. Erasure-Coding Amplification vs Physical Wire Bytes

Under Reed-Solomon $\text{RS}(20,12)$, every 12 bytes of payload data produce 8 bytes of parity data, yielding an expansion factor:

$$\text{Expansion Factor} = \frac{N}{K} = \frac{20}{12} \approx 1.666667$$

For a 100 MiB write ($104,857,600\text{ bytes}$), the coordinator divides the payload into stripes and writes 20 shards per stripe. The aggregate volume dispatched across all 20 storage nodes is:

$$\text{Nominal Encoded Volume} = 100\text{ MiB} \times \frac{20}{12} = 166.67\text{ MiB}\quad (174,762,667\text{ bytes})$$

Dividing by the median PUT duration of **5.3410 seconds** yields:

$$\text{Nominal Rate} = \frac{166.6667\text{ MiB}}{5.3410\text{ s}} \approx \mathbf{31.21\text{ MiB/s}}$$

However, because the storage nodes communicated with the coordinator via in-process Tokio async channels rather than physical Ethernet NICs, this figure represents an internal algorithmic throughput, **not physical wire throughput**. Physical network measurements must capture Linux `/sys/class/net/<iface>/statistics/tx_bytes` or socket send/recv counters.

---

## 5. Audit Declarations

1. **Physical LAN Testing:** **`NOT EXECUTED`**. Inter-node RPCs across independent physical hosts were not measured in this benchmark.
2. **Production Readiness:** **`NOT ESTABLISHED`**. Benchmark figures reflect single-node in-process performance.
3. **Wire Throughput Classification:** **`ACTUAL_WIRE_BYTES: NOT MEASURED`**.
