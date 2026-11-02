# FSP Phase 5.6: Explicit System Limitations & Non-Claims

## 1. Executive Disclosure: Production Readiness NOT ESTABLISHED

Forever Storage Protocol (FSP) has made substantial engineering progress through Phase 5.6, establishing verified crash consistency, Linux process supervision, atomic directory barriers, and durable acknowledgement semantics.

However, **production readiness is strictly NOT ESTABLISHED**. FSP must not be deployed into production or mission-critical environments until the explicit engineering limitations documented below are resolved.

---

## 2. Comprehensive Limitations & Boundaries

### 1. Single Physical Machine Environment
- All network operations executed during Phase 5.6 were conducted over Linux loopback (`127.0.0.1`).
- Loopback interfaces exhibit 0% packet loss, ~0.02 ms latency, and 65,536-byte MTUs. Real Ethernet links introduce jitter, packet drops, fragmentation (1,500-byte MTU), and switch congestion.
- In the single-machine testbed, all 20 nodes share a single operating-system kernel, memory controller, and power supply. A physical power failure or kernel panic halts 100% of the cluster simultaneously.

### 2. Multi-Host Bare-Metal Deployment Status
- **Status: `NOT EXECUTED`.**
- Multi-host topology plans, service definitions, and inventory scripts (`deploy/phase5_6/`) are fully prepared, but independent physical machines were not provisioned or authorized. Physical LAN multi-host validation remains to be conducted.

### 3. Power-Loss Durability (Class B and C) Status
- **Status: `NOT EXECUTED`.**
- Class A process failure (`SIGKILL`) was rigorously validated.
- Class B (OS/VM crash) and Class C (Physical power loss) require dedicated hardware and separate authorization, and remain unexecuted.

### 4. Multi-Day Endurance Testing Status
- **Status: `PARTIAL`.**
- Bounded 5-minute and 15-minute soak tests passed with zero leaks (stable RSS ~16.8 MiB, 18 FDs, 0 zombie processes).
- Extended 24-hour and 72-hour longevity soaks remain `NOT EXECUTED`.

### 5. Architectural Non-Goals
- **No Blockchain Tokenomics:** FSP deliberately contains no tokens, proof-of-work, cryptocurrency mining, or financial incentives.
- **No Public Permissionless Network:** FSP is an enterprise/infrastructure distributed storage engine designed for controlled, authenticated failure domains using mutual TLS 1.3.
- **No Byzantine Fault Tolerance (CFT Only):** Storage nodes and coordinators are assumed to be non-Byzantine.
