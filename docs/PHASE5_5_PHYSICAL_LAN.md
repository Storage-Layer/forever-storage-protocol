# FSP Phase 5.5: Physical LAN Validation & Deployment Audit

## Executive Status: NOT EXECUTED ON INDEPENDENT BARE-METAL

In accordance with strict empirical engineering standards, **physical multi-host network deployment has NOT been executed on independent physical machines**. The host testbed is a single physical workstation (`Linux 6.14.0-37-generic x86_64, 24 cores, 62 GiB RAM`). External independent bare-metal hardware was neither provisioned nor authorized for live remote execution.

Consequently:
- **Physical Multi-Host Status:** `NOT EXECUTED`
- **Physical Multi-Host Production Readiness:** `NOT ESTABLISHED`
- **Network Scope Tested:** Linux multi-process OS subprocesses over TCP/TLS 1.3 loopback interfaces (`127.0.0.1:[9500..9520]`).

---

## 1. Multi-Host Deployment Specifications

Deploy configurations and deployment topologies have been prepared and checked into `deploy/phase5_5/`:
- `deploy/phase5_5/two_host/hosts.yaml`: 2 physical host cluster plan (Host A: Coordinator + 10 nodes; Host B: 10 nodes).
- `deploy/phase5_5/three_host/hosts.yaml`: 3 physical host cluster plan (Host A: Coordinator + 7 nodes; Host B: 7 nodes; Host C: 6 nodes).
- `deploy/phase5_5/five_host/hosts.yaml`: 5 physical host cluster plan (Host 1..5: 4 nodes per host corresponding directly to failure zones A, B, C, D, E).
- `deploy/phase5_5/inventory/hosts.ini`: Ansible/systemd inventory for automated staging.
- `deploy/phase5_5/systemd/`: Production systemd service unit files (`fsp-node@.service` and `fsp-coordinator.service`).

---

## 2. Loopback vs. Real Physical LAN Discrepancy Matrix

| Dimension | Loopback Environment (Tested) | Physical LAN (Projected / Untested) | Durability / Safety Impact |
|:---|:---|:---|:---|
| **Packet Latency** | ~0.02 ms – 0.05 ms | ~0.2 ms – 2.0 ms (1GbE / 10GbE switched) | TCP heartbeat / lease renewal timeouts must account for jitter. |
| **Packet Loss / Reordering** | 0% (deterministic kernel ring buffer) | 0.01% – 1.0% (switch buffer drops, NIC resets) | Requires TLS retransmission & TCP window sizing under high load. |
| **MTU / Fragmentation** | 65,536 bytes (loopback MTU) | 1,500 bytes (standard) or 9,000 bytes (jumbo) | IP packet fragmentation on large shard transfers (>1 MiB stripes). |
| **Bandwidth Saturation** | ~40 Gbps inter-core memory bus | 1 Gbps – 10 Gbps physical link speed | Concurrent stripe reconstruction will saturate physical uplinks. |
| **NIC / Cable Failures** | None | Physical link drops, PHY renegotiation | Triggers transient node unavailability and health check flap. |
| **Failure Domain Correlation** | All nodes share single host kernel & power supply | Independent power supplies, switch racks, UPS units | Host kernel crash or power drop kills all 20 nodes simultaneously on single machine. |

---

## 3. Pre-Flight Verification Script

For future staging on authentic hardware, [`deploy/phase5_5/scripts/preflight.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_5/scripts/preflight.sh) enforces required system prerequisites:
1. Verifies kernel version $\ge 5.15$ with support for `PR_SET_PDEATHSIG` and `O_SYNC` / `fdatasync`.
2. Validates filesystem mount options: rejects mounts with `nobarrier` or `data=writeback` that violate durability semantics.
3. Tests TCP socket bind capability across configured ports without permission denial.
4. Audits entropy pool availability for cryptographic key generation and Ed25519 signing.
5. Verifies memory and swap thresholds to prevent OOM killer invocations during 100 MiB erasure stripe coding.
