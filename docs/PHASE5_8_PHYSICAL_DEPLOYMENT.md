# FSP Phase 5.8: Physical Multi-Host Deployment and Topology Architecture

## 1. Physical Host Topology Overview

FSP is designed to distribute Reed-Solomon coded erasure stripes across independent failure domains. In Phase 5.8, deployment configurations are standardized across three topology scales:

1. **Two-Host Staging Pilot (`deploy/phase5_8/two_host/`):**
   - **Host A (Primary):** IP `10.77.77.2` (Alt `192.168.1.103`), Coordinator Primary + Storage Nodes 0..9 (10 instances).
   - **Host B (Secondary Target):** IP `10.77.77.3` (Alt `192.168.1.104`), Coordinator Standby + Storage Nodes 10..19 (10 instances).
   - **Durability Characteristic:** 0 host losses tolerated under $\text{RS}(20, 12)$. Losing either host removes 10 shards ($10 < K=12$). This topology evaluates network transport and cross-node coordination, not physical fault survival.

2. **Three-Host Fault-Tolerant Staging (`deploy/phase5_8/three_host/`):**
   - Host 1: 7 Shards (Nodes 0..6)
   - Host 2: 7 Shards (Nodes 7..13)
   - Host 3: 6 Shards (Nodes 14..19)
   - **Durability Characteristic:** 1 host loss tolerated. Maximum shards lost upon failure of any single host is 7, leaving $13 \ge 12$ surviving shards.

3. **Five-Host Production Topology (`deploy/phase5_8/five_host/`):**
   - 4 Shards per host across 5 independent racks/hosts.
   - **Durability Characteristic:** 2 full host/rack losses tolerated ($8$ shards lost, exactly $12$ surviving shards $\equiv K$). 3 host losses drop surviving shards to $8 < 12$, safely triggering fail-closed rejection.

---

## 2. Real Host Hardware Inventory

The primary host was audited via `/proc` and Linux system calls:
- **Host Identity:** `host-a.fsp.internal`
- **CPU:** Dual-socket Intel(R) Xeon(R) CPU X5650 @ 2.67GHz (24 logical threads)
- **RAM:** 62 GiB DDR3 ECC Registered
- **Filesystem:** `ext4` on `/dev/sdb3`
- **Interfaces:** `enp2s0` (`10.77.77.2/24`), `wlx7c8bca05e8a9` (`192.168.1.103/24`), `lo` (`127.0.0.1/8`)

---

## 3. Remote Physical Execution Status

```
+-------------------------------------------------------------------------------+
| STATUS: PHASE 5.8 LOCAL ENGINEERING COMPLETE — PHYSICAL PILOT NOT EXECUTED     |
| Reason: BLOCKED: No authorized independent physical host available             |
+-------------------------------------------------------------------------------+
```

Per strict instructions:
1. No unauthenticated scanning or unauthorized machine compromise was performed.
2. The user did not designate or authorize remote SSH credentials for an independent secondary physical host.
3. All local orchestration scripts, non-loopback bindings, and transport logic are verified and packaged under `deploy/phase5_8/`.
