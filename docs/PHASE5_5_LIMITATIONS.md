# FSP Phase 5.5: Explicit System Limitations & Non-Claims

## 1. Executive Disclosure: Production Readiness NOT ESTABLISHED

Forever Storage Protocol (FSP) has reached Phase 5.5 as a high-fidelity, crash-consistent distributed storage prototype validated on a single multi-core Linux host using real operating-system subprocesses, TCP/mTLS 1.3, disk barriers, and RS(20,12) erasure coding.

However, **production readiness is strictly NOT ESTABLISHED**. FSP must not be deployed into production or mission-critical environments without addressing the explicit engineering limitations documented below.

---

## 2. Comprehensive Limitations Matrix

### 1. Single Physical Machine Environment
- **Loopback Networking:** All network communication in tested benchmarks occurred over Linux loopback (`127.0.0.1`). Loopback interfaces feature 0% packet loss, ~0.02 ms latency, and large MTU (65,536 bytes). Real physical Ethernet links introduce variable packet delay, packet loss, TCP retransmissions, MTU fragmentation (1,500 bytes), and physical link flap.
- **Correlated Host Kernel / Power Failure:** In the current testbed, all 20 nodes share a single operating system kernel, memory bus, and power supply. A physical power interruption or Linux kernel panic halts 100% of the cluster simultaneously. Surviving multi-zone outages assumes physically separate power and network hardware.

### 2. Multi-Host Bare-Metal Deployment Status
- **Status: NOT EXECUTED.**
- Deployment configurations and service definitions (`deploy/phase5_5/`) are fully prepared, but independent physical machines were not provisioned or authorized. Multi-host physical validation must be independently executed on a physical LAN before asserting multi-host production readiness.

### 3. Multi-Day Longevity & Soak Testing Status
- **Status: PARTIAL.**
- Bounded 5-minute and 15-minute soak tests passed with zero leaks (memory RSS stable at ~16.8 MiB, constant 18 FDs, zero zombie processes).
- Extended 24-hour and 72-hour endurance soaks remain **NOT EXECUTED** due to session wall-clock constraints.

### 4. Non-Byzantine Fault Model (CFT Only)
- FSP is designed under the **Crash Fault Tolerant (CFT)** model.
- Storage nodes and coordinators are assumed to be non-Byzantine. While shard payloads and manifests are cryptographically verified via SHA-256 and Ed25519 signatures, the system does not protect against coordinated Byzantine quorum subversion or malicious coordinator collusion.

### 5. Architectural Non-Goals
- **No Blockchain Tokenomics:** FSP deliberately contains no tokens, proof-of-work, cryptocurrency mining, or financial incentives.
- **No Public Permissionless Network:** FSP is an enterprise/infrastructure distributed storage engine designed for controlled, authenticated failure domains using mutual TLS 1.3.
- **No Unbounded Durability Claims:** No storage system can guarantee infinite survival without active physical hardware maintenance, disk replacement, and continuous scrubbing.
