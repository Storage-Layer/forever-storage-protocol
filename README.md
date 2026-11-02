# Forever Storage Protocol (FSP)

[![Rust](https://img.shields.io/badge/rust-2021%20edition-orange.svg)](https://www.rust-lang.org)
[![Tests](https://img.shields.io/badge/tests-370%2F370%20passing-brightgreen.svg)]()
[![Suites](https://img.shields.io/badge/suites-19%20active-blue.svg)]()
[![Erasure Coding](https://img.shields.io/badge/FEC-RS(20%2C12)%20Cauchy-informational.svg)]()
[![Security](https://img.shields.io/badge/mTLS-1.3%20Ed25519-green.svg)]()
[![Status](https://img.shields.io/badge/phase-5.8%20virtual%20network-blueviolet.svg)]()

**Forever Storage Protocol (FSP)** is a high-assurance, fault-tolerant distributed storage engine written in Rust. It provides durable object storage across distributed nodes utilizing systematic Reed–Solomon RS(20,12) Cauchy matrix erasure coding, multi-voter coordinator quorum consensus, strict Mutual TLS 1.3 cryptographic identity binding, and format-hardened V2 shard containers.

---

## Technical Architecture

```
                                    +------------------------------+
                                    |     Client / Ingestion       |
                                    +------------------------------+
                                                   |
                                         (Authenticated mTLS 1.3)
                                                   v
                       +-------------------------------------------------------+
                       |      Coordinator Consensus Tier (fsp-c01..c03)         |
                       |       3-Voter Quorum, Lease Tracking, Epoch Fencing   |
                       +-------------------------------------------------------+
                                                   |
                         +-------------------------+-------------------------+
                         | (RS(20,12) Cauchy Parity Slices over Kernel mTLS) |
                         v                                                   v
+-------------------------------------------------+ +-------------------------------------------------+
| Zone A (fsp-n01..n04) | Zone B (fsp-n05..n08)   | | Zone C (fsp-n09..n12) | Zone D (fsp-n13..n16)   |
| Storage Nodes 00 - 07                           | | Storage Nodes 08 - 15                           |
| NodeDaemon, V2 Containers, WAL, CRC32 Validation| | NodeDaemon, V2 Containers, WAL, CRC32 Validation|
+-------------------------------------------------+ +-------------------------------------------------+
                                                  \ /
                                   +-------------------------------+
                                   | Zone E (fsp-n17..n20)         |
                                   | Storage Nodes 16 - 19         |
                                   | NodeDaemon, V2 Containers     |
                                   +-------------------------------+
```

### Core Primitives

- **Systematic Cauchy Erasure Coding (RS(20,12)):**  
  Every object stripe is divided into $k = 12$ data shards and $m = 8$ Cauchy parity shards ($n = 20$). The system tolerates any arbitrary combination of up to 8 simultaneous node failures with byte-exact recovery.
- **Hierarchical Failure Domains:**  
  The cluster enforces 5 isolated logical failure zones (`Zone A` through `Zone E`, 4 nodes each). Loss of 2 entire zones ($4 \times 2 = 8$ nodes) guarantees full reconstruction from the surviving 3 zones ($12 \ge 12$).
- **Strict Mutual TLS 1.3 (`rustls` & Ed25519):**  
  Zero plaintext transport. Peer authorization requires CA-signed X.509 certificates with mandatory IP Subject Alternative Name (SAN) binding and cryptographic fingerprint registration in the coordinator's `TrustPolicy`.
- **Durable V2 Shard Containers (`FSPC`):**  
  Format-hardened, self-describing container files with independent header CRC32, payload CRC32, atomic filesystem synchronization (`fsync` + parent directory `fsync`), and write-ahead log (WAL) replay for idempotency.
- **Fail-Closed Consensus Control Plane:**  
  Coordinator leadership epochs enforce monotonic fencing ($E_{k+1} > E_k$). Split-brain conditions or quorum loss ($\le 1$ voter of 3) immediately trigger fail-closed mutation rejection to guarantee zero silent corruption.

---

## Phase 5.8 Virtual Distributed Network Validation

FSP was evaluated in a realistic kernel-networked distributed laboratory on Linux using 23 isolated network namespaces (`fsp-c01..c03`, `fsp-n01..n20`), connected via virtual Ethernet (`veth`) pairs and a dedicated Linux bridge (`fsp-br0` at `10.240.58.1/24`).

### Failure & Resilience Scenarios

| Scenario | Injected Condition | Threshold | Observed Result | Status |
|---|---|---|---|:---:|
| **100 MiB PUT & GET** | Baseline full cluster commit | 20 / 20 nodes | Committed in 4.12s (~24.8 MB/s), retrieved in 25.3s | **VERIFIED** |
| **8-Node Catastrophic Loss** | Simultaneous `SIGKILL` to 8 nodes (0..7) | $12 \ge 12$ ($k=12$) | Decoded via Cauchy matrix; exact SHA-256 match | **VERIFIED** |
| **9-Node Catastrophic Loss** | Simultaneous `SIGKILL` to 9 nodes (0..8) | $11 < 12$ ($k=12$) | Safely failed closed: `InsufficientValidShards` | **VERIFIED** |
| **2-Zone Outage** | Loss of Zones A & B (8 nodes down) | 12 nodes surviving | Reconstructed from Zones C, D, and E | **VERIFIED** |
| **3-Zone Outage** | Loss of Zones A, B, & C (12 nodes down) | 8 nodes surviving | Safely failed closed: `InsufficientValidShards` | **VERIFIED** |
| **Coordinator Leader Crash** | `SIGKILL` on primary leader | 3-member quorum | Standby promoted to Epoch $E+1$; stale leader fenced | **VERIFIED** |
| **Consensus Quorum Loss** | 2 of 3 voter coordinators offline | $\lfloor 3/2 \rfloor + 1 = 2$ | Mutations rejected; atomic fail-closed behavior | **VERIFIED** |
| **Network Partition & Heal** | 12-node vs. 8-node cluster partition | $12 \ge 12$ read quorum | Read quorum served; cluster re-converged on heal | **VERIFIED** |
| **Shard Bit-Rot Corruption** | Overwrote byte offsets with `0xFF` | Shard 00 on Node 00 | CRC32 mismatch detected; transparently reconstructed | **VERIFIED** |
| **Process Crash Durability** | Simultaneous `SIGKILL` on 5 nodes | 1 node per zone | Cold restart validated WAL, repaired staging, 0 loss | **VERIFIED** |

### Network Emulation Benchmark (`tc netem`)

Benchmarked over 5 independent trials per profile with 100 MiB RS(20,12) objects transferred over authenticated mTLS 1.3:

| Network Profile | Simulated Parameters | Avg PUT (ms) | Avg GET (ms) | Avg GET Throughput |
|---|---|:---:|:---:|:---:|
| **Baseline** | 0.0 ms delay, 0.0% packet loss | 382.09 ms | 531.84 ms | **1.88 MB/s** |
| **LAN** | 0.5 ms delay, 0.0% packet loss | 375.46 ms | 941.73 ms | **1.06 MB/s** |
| **WAN** | 20.0 ms delay, 0.0% packet loss | 528.18 ms | 8,500.61 ms | **0.12 MB/s** |
| **Degraded WAN** | 35.0 ms delay, 5ms jitter, 0.5% loss | 1,138.40 ms | 15,231.29 ms | **0.06 MB/s** |

All cryptographic digests matched the canonical source digest (`bf17539774a7fab5802d0e4a5fccc4df33736ea0d48d101592ad88589818fea9`) with 100% byte-exact fidelity.

---

## Governance & Boundary Disclaimers

To maintain strict scientific evidence integrity and engineering transparency:

```
REAL_PHYSICAL_MULTI_HOST: NOT EXECUTED
PHYSICAL_POWER_LOSS:      NOT EXECUTED
PHYSICAL_NIC_THROUGHPUT:  NOT MEASURED
PRODUCTION_READINESS:     NOT ESTABLISHED
```

- **Virtual Execution:** Experiments were conducted inside Linux network namespaces on a single physical host (`Linux Mint 22.3, kernel 6.14.0-37-generic x86_64`) using authentic kernel TCP/IP sockets and `tc netem`, not separate physical machine motherboards or external Ethernet switches.
- **Power Loss:** Durability under process crashes was validated via `SIGKILL` and process restarts. Hardware-level physical power interruptions (Class C) requiring switched ePDUs were not executed.
- **Production Readiness:** FSP is an active distributed systems research implementation; production readiness is not claimed.

---

## Repository Structure

```
├── deploy/
│   └── phase5_8/
│       ├── netlab/            # Automated netlab lifecycle and fault injection scripts
│       │   ├── setup.sh       # Bridge fsp-br0, 23 namespaces, and mTLS PKI setup
│       │   ├── start.sh       # Launches 20 storage nodes and coordinator
│       │   ├── status.sh      # Health and link inspection probe
│       │   ├── inject_fault.sh# Traffic control, link toggle, and SIGKILL injection
│       │   ├── cleanup.sh     # Clean teardown of processes and network devices
│       │   └── run_netlab_experiments.py  # End-to-end automated experiment runner
│       └── physical_pilot/    # Physical 2-host LAN deployment configurations & runbooks
├── docs/                      # Architectural specifications and milestone verification audits
│   ├── PHASE5_8_VIRTUAL_NETWORK_VALIDATION.md  # Comprehensive 20-point Phase 5.8 report
│   └── PHASE5_8_PHYSICAL_PILOT_RESULT.md       # Physical pilot audit & hardware evaluation
├── results/
│   └── phase5_8/netlab/       # 21 authoritative JSON evidence files from automated trials
├── src/
│   ├── bin/                   # Executables: fsp_node and fsp_coordinator
│   ├── coordinator/           # Consensus, epoch fencing, placement planner, replication
│   ├── network/               # rustls mTLS 1.3 transport, RPC framing, connection pools
│   ├── node/                  # StorageNodeDaemon, V2 container engine, idempotency WAL
│   ├── recovery/              # RS(20,12) Cauchy erasure reconstruction and repair scheduler
│   └── storage/               # FSPC container format, atomic filesystem sync, CRC32
├── tests/                     # 19 integration test suites (370 passing tests)
└── tools/                     # Evidence consistency validators and manifest generators
```

---

## Building and Verification

### Prerequisites

- Rust 1.80+ (stable toolchain)
- Linux (Ubuntu 22.04+ / Linux Mint 21+ / Debian 12+)
- `iproute2` and `iptables` for virtual network namespaces

### Running the Test Suite

```bash
# Execute the full 370-test integration suite
cargo test --workspace --all-targets -- --test-threads=1

# Run clippy and format checks
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

### Running the Virtual Network Experiment Suite

```bash
# 1. Set up the 23 network namespaces, bridge, and PKI certificates
./deploy/phase5_8/netlab/setup.sh

# 2. Run the automated 10-experiment suite and benchmark trials
python3 deploy/phase5_8/netlab/run_netlab_experiments.py

# 3. Clean up virtual network devices and processes
./deploy/phase5_8/netlab/cleanup.sh
```

---

## License

This project is licensed under the MIT License or Apache-2.0 License at your discretion.
