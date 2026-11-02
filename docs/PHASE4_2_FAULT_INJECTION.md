# FSP Phase 4.2: Fault Injection Framework & Adversarial Scenarios

## 1. Overview

The fault injection framework ([`src/testing/fault_injection.rs`](file:///home/rade/Project/Jobs/fsp/src/testing/fault_injection.rs)) provides deterministic, programmatically controlled failure injection to validate distributed storage resilience under network degradation and process crashes.

---

## 2. Injected Fault Rules

| Fault Rule | Behavior | Validated In |
|:---|:---|:---:|
| `FaultRule::PacketLoss` | Drops packets to targeted nodes with defined probability. | `test_p4_2_24` |
| `FaultRule::Latency` | Injects sleep delay before network or disk operations. | Unit & benchmark suites |
| `FaultRule::ConnectionReset` | Simulates abrupt TCP `RST` from storage peers. | `test_p4_2_19` |
| `FaultRule::Partition` | Simulates symmetric/asymmetric network partitions. | `test_p4_2_02`, `test_p4_2_13` |
| `FaultRule::StaleEpoch` | Injects outdated leadership epochs. | `test_p4_2_04`, `test_p4_2_05` |
| `FaultRule::DiskFull` | Returns `QuotaExceeded` without process panics. | `test_p4_2_24` |
| **Kernel SIGKILL** | Abruptly terminates child process via OS `SIGKILL`. | `test_p4_2_12` |
