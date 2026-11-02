# FSP Phase 5.1 — Known Limitations & Non-Production Scope

## 1. Non-Production Notice
> [!WARNING]
> FSP remains an advanced research prototype and engineering implementation. **PRODUCTION READINESS IS NOT ESTABLISHED.**

## 2. Identified Limitations
1. **Physical Bare-Metal Validation:** Physical multi-host tests across real optical network switches and physical SAS/NVMe controllers have **NOT BEEN EXECUTED**. Testing has been performed on Linux Mint 22.3 / Ubuntu 24.04 localhost multi-process simulations.
2. **Extended Soak Testing:** Continuous 24-hour and 72-hour endurance testing has **NOT BEEN EXECUTED**. Bounded stress tests (60 seconds) confirm zero leaks, but long-term memory fragmentation and kernel buffer behavior remain uncharacterized.
3. **Dynamic Quorum Reconfiguration:** Reconfiguring the voter set of an active control plane without restart is not fully automated; surviving nodes currently operate under safe fail-closed invariants until administrative disaster restore is performed.
4. **Bandwidth Throttling Granularity:** Per-tenant network egress bandwidth shaping is coarse-grained at the node TCP level and does not yet support token-bucket rate limiting per tenant namespace.
5. **No Blockchain / Tokenomics:** FSP is strictly a distributed storage preservation protocol and consensus engine; it does not implement blockchain tokenomics or proof-of-work/stake consensus.
