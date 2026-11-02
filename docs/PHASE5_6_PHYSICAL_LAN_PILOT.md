# FSP Phase 5.6: Physical LAN Pilot & Two-Host Deployment Audit

## 1. Executive Status: NOT EXECUTED ON INDEPENDENT HARDWARE

In accordance with strict empirical engineering principles:
- **Physical Multi-Host Deployment Status:** `NOT EXECUTED`
- **Physical Multi-Host Production Readiness:** `NOT ESTABLISHED`
- **Host Testbed Reality:** Single physical Linux workstation (`Linux 6.14.0-37-generic x86_64, 24 cores, 62 GiB RAM`).
- **Authorization Boundary:** No external physical bare-metal hardware was provisioned or authorized for live execution. In adherence to system instructions, unauthorized scanning, network probing, or remote deployment was strictly avoided.

---

## 2. Two-Host Physical Deployment Plan

Tooling, preflight audits, and configurations have been developed and placed in [`deploy/phase5_6/two_host/hosts.yaml`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_6/two_host/hosts.yaml):

```yaml
cluster:
  name: fsp-lan-pilot-2host
  scheme: RS(20,12)
  mtls: true

host_a:
  role: coordinator_and_storage
  listen_ip: "192.168.1.10" # Placeholder for authorized staging IP
  coordinator_port: 9500
  storage_nodes: 10
  storage_ports_range: [9501, 9510]

host_b:
  role: storage_only
  listen_ip: "192.168.1.11" # Placeholder for authorized staging IP
  storage_nodes: 10
  storage_ports_range: [9511, 9520]
```

---

## 3. Critical Theoretical Limitation of Two-Host Deployments

A fundamental distributed systems invariant applies to a two-host cluster running RS(20,12):

1. **Shard Distribution:** 20 unique shard indices partitioned between 2 hosts yields 10 shards per host.
2. **Failure Analysis:** If Host A or Host B experiences a catastrophic failure (power loss, motherboard failure, kernel panic), exactly 10 shards are lost simultaneously.
3. **Surviving Shards:** $20 - 10 = 10$ surviving shards.
4. **Reconstruction Threshold:** $K = 12$.
5. **Conclusion:** Because $10 < K = 12$, **a two-host cluster cannot tolerate the loss of either physical host**.

The two-host pilot is designed exclusively to validate **cross-network physical socket communication, MTU fragmentation, and remote TLS identity verification**. It does **not** provide physical machine fault tolerance. That guarantee requires a minimum of 3 physical hosts (tolerates 1 host loss) or 5 physical hosts (tolerates 2 host losses).

---

## 4. Loopback vs. Physical Network Discrepancy Matrix

| Dimension | Loopback Environment (Tested) | Physical LAN (Projected / Untested) |
|:---|:---|:---|
| **Latency** | 0.02 – 0.05 ms | 0.3 – 1.8 ms (switched 1GbE/10GbE) |
| **Packet Loss** | Deterministic 0.00% | 0.01% – 0.1% under link saturation |
| **MTU** | 65,536 bytes | 1,500 bytes (standard) or 9,000 bytes (jumbo) |
| **Bandwidth** | ~40 Gbps internal bus | 1 Gbps or 10 Gbps physical link speed |
| **Failure Domain** | Shared CPU, RAM, power supply | Independent power supplies, UPS, switch ports |
