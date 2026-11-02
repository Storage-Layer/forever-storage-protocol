# Phase 5 Known Limitations & Technical Debt
**Forever Storage Protocol (FSP)**
**Date:** 2026-11-01  
**Author:** Principal Distributed Systems Architect & Quality Assurance Engineer  

---

## 1. Explicitly Not Executed Items

1. **Physical Bare-Metal 5-Machine Hardware Staging:**
   - **Status:** **NOT EXECUTED**
   - **Context:** Staging configurations, systemd units, network topologies, and PKI scripts have been engineered and validated under local multi-process network emulation (`127.0.0.1` ports `19400`–`19420`). Deploying across five distinct physical server machines in a real datacenter requires physical hardware provisioning not available in this interactive turn.
2. **Continuous 24-Hour & 72-Hour Soak Tests:**
   - **Status:** **NOT EXECUTED**
   - **Context:** A bounded soak test harness executing 500 multi-operation cycles and bounded multi-minute stress tests was executed and passed with zero memory leaks and zero consistency failures. Continuous multi-day endurance testing was not executed due to turn execution time limits.

---

## 2. Architectural Limitations & Research Constraints

### 1. In-Memory Dynamic Membership Changes
- Current quorum consensus assumes a static set of 5 voting coordinators configured in `cluster_5host.toml`.
- Adding or removing voters currently requires a rolling configuration update and restart, rather than a dynamic joint-consensus reconfiguration protocol (e.g., Raft dynamic membership).

### 2. Single-Stripe Emergency Read Tool
- The Phase 5 emergency offline recovery tool (`fsp emergency-read`) currently reconstructs single-stripe objects directly. Multi-stripe objects requiring reconstruction across hundreds of megabytes in emergency mode must be iterated sequentially per stripe index.

### 3. Local Repair vs. Global Reconstruction Trade-off in LRC
- While systematic Cauchy RS(20,12) achieves 100% MDS recovery, LRC(20,12,4) provides faster single-shard repairs (consuming only 3 shards) at the cost of reduced maximum catastrophic fault tolerance (tolerating 4 arbitrary erasures instead of 8). The system default remains RS(20,12) for maximum long-term preservation reliability.

### 4. Direct Kernel Bypass (io_uring / SPDK)
- Filesystem I/O currently utilizes standard POSIX synchronous file operations with `fsync` barriers and bounded threads. While achieving > 200 MB/s, adoption of asynchronous `io_uring` or kernel-bypass NVMe drivers (SPDK) represents future performance optimization work for 100 Gbps network saturations.
