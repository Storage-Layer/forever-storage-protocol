# FSP Phase 5.7: Operational Boundaries & Technical Limitations

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_LIMITATIONS.md`  

---

## 1. Explicit Non-Goals & Unverified Claims

To preserve scientific and engineering integrity, this document explicitly lists all unexecuted tests, physical boundaries, and operational constraints. **No claim of production readiness or indefinite durability is made.**

---

## 2. Identified Technical Limitations

### 1. Single Host Testbed Constraint
- **Current State:** All current empirical test execution, benchmarks, and fault injections were executed on a single physical host (`Linux 6.14.0-37-generic x86_64`).
- **Limitation:** While tests utilize distinct operating-system processes, network sockets, TLS 1.3 encryption, and separate filesystem directories, they do not experience physical Ethernet cabling faults, switch packet buffering, asymmetric LAN drops, or independent hardware clocks.
- **Status:** **Physical multi-host execution remains PENDING deployment on external infrastructure.**

### 2. Two-Host Physical Staging Durability Limit
- **Current State:** The staged pilot configuration in `deploy/phase5_7/two_host/` maps 10 nodes to Host A and 10 nodes to Host B.
- **Limitation:** Under RS(20,12), losing either physical machine destroys 10 shards simultaneously, leaving only 10 surviving shards ($10 < K=12$).
- **Conclusion:** A 2-host setup cannot survive physical machine crashes. A minimum of 3 physical hosts (or 5 hosts) is mathematically required for physical machine fault tolerance.

### 3. Class B & Class C Power-Loss Testing Scope
- **Current State:** Crash resilience was thoroughly verified using simulated crash points (`AfterBytesWrittenBeforeFsync`, `BeforeJournalFsync`, etc.) and process SIGKILL.
- **Limitation:** Genuine bare-metal physical power cuts (Class C) and OS kernel panics (Class B) were not executed on the developer workstation, as doing so requires specialized programmable power distribution units (ePDUs) and remote IPMI/BMC testbeds.

### 4. Extended Multi-Day Soak Testing
- **Current State:** Bounded endurance was validated for 60 seconds / 500 operations.
- **Limitation:** 24-hour and 72-hour continuous burn-in soak tests were not executed.

### 5. Production Readiness Verdict
- **Current Status:** **PRODUCTION READINESS: NOT ESTABLISHED**.
- **Requirement:** Production readiness cannot be certified until FSP is continuously operated across a minimum of 3 to 5 independent physical hosts with automated failover and multi-day soak verification under live network traffic.
