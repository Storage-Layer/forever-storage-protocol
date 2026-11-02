# Phase 5.2 Known Limitations & Boundaries

## 1. Explicit Scope & Boundary Acknowledgements
To maintain rigorous scientific and engineering integrity, the following limitations are documented explicitly:

### A. Production Readiness Status
- **Status:** **NOT ESTABLISHED**.
- **Justification:** FSP is an academic and research distributed storage preservation protocol. While hardened with 250 passing automated tests, formal model checkers, and cryptographic integrity validation, it has not undergone external third-party penetration testing, production bare-metal validation across multi-region datacenters, or multi-year operational soak.

### B. Physical Bare-Metal Multi-Host Deployment
- **Status:** **NOT EXECUTED**.
- **Justification:** All distributed network and failure tests were executed using isolated local multi-process configurations. Bare-metal multi-machine staging requires physical cluster provisioning outside the current environment scope.

### C. Continuous Long-Duration Soak Tests (24h & 72h)
- **Status:** **NOT EXECUTED**.
- **Justification:** Multi-day continuous soak testing was omitted in favor of bounded stress tests and static leak audits to remain within conversational runtime limits.

### D. Byzantine Fault Tolerance (BFT)
- FSP operates under a crash-fault-tolerant (CFT) consensus model with mutual TLS 1.3 cryptographic authentication and Ed25519 digital signatures. It does not implement BFT consensus (e.g. PBFT or Raft-BFT) for arbitrary malicious coordinator Byzantine behaviors.

### E. Blockchains and Tokenomics
- FSP deliberately excludes blockchain tokens, cryptocurrency mining, and speculative consensus mechanisms, focusing purely on deterministic, long-term distributed storage durability.
