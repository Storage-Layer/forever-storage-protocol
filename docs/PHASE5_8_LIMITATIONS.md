# FSP Phase 5.8: Architectural Limitations & Operational Boundaries

## 1. Production Readiness Status

```
+─────────────────────────────────────────────────────────────+
| PRODUCTION READINESS: NOT ESTABLISHED                       |
+─────────────────────────────────────────────────────────────+
```

FSP remains a distributed storage research and engineering protocol prototype. It is not currently certified for production deployment or handling unbacked critical data.

---

## 2. Identified Operational and Environmental Limitations

### 2.1 Absence of Secondary Physical Host Credentials
- While non-loopback networking, mTLS SAN identity, and deployment assets (`deploy/phase5_8/`) are 100% complete and verified, cross-host network communication across two separate physical machines was not executed because the operator did not designate or authorize secondary machine credentials.
- Formal Status: `BLOCKED: No authorized independent physical host available` / `NOT EXECUTED`.

### 2.2 Power-Loss Durability Constraints
- **Class A (Process SIGKILL):** Executed and verified.
- **Class B (Kernel Panic) & Class C (Physical Power Cut):** Marked `NOT EXECUTED`. Testing sudden hardware power severance without ePDU or BMC management risks damaging the engineering workstation's local storage.

### 2.3 Wire Throughput Measurement
- In adherence to strict evidence integrity, wire throughput is recorded as `ACTUAL_WIRE_BYTES: NOT MEASURED`. Local loopback or in-memory transfer rates cannot substitute for real physical switch benchmarks.

---

## 3. Mandatory Prerequisites for Future Milestones

1. Provisioning of at least 2 independent bare-metal Linux hosts with dedicated non-loopback network connections and authorized SSH keys.
2. Switched ePDU or IPMI power cycling infrastructure to safely evaluate Class B and Class C power-loss durability.
3. Multi-day sustained load generation with Prometheus metric aggregation.
