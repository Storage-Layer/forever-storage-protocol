# FSP Phase 5.7: Physical LAN Pilot & Multi-Host Staging Audit

**Milestone:** Phase 5.7  
**Artifact Path:** `docs/PHASE5_7_PHYSICAL_LAN_PILOT.md`  
**Execution Status:** **NOT EXECUTED**  
**Staging Artifacts:** [`deploy/phase5_7/`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/)  
**Preflight Data:** [`results/phase5_7/physical_preflight.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/physical_preflight.json)  
**LAN Result Evidence:** [`results/phase5_7/physical_lan_put_get.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_7/physical_lan_put_get.json)  

---

## 1. Physical Pilot Execution Status: NOT EXECUTED

In strict accordance with Phase 5.7 directives, this audit states unambiguously:
**The two-host physical LAN pilot was NOT EXECUTED on independent bare-metal hardware.**

### Evidence & Rationale:
1. **Single-Host Environment:** The active testbed is a single Linux host (`Linux 6.14.0-37-generic x86_64`). No secondary physical host is available or authorized on the local subnet.
2. **Automated Ping & Route Verification:** The preflight check [`deploy/phase5_7/scripts/deploy_two_host.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/scripts/deploy_two_host.sh) probed `192.168.1.10` (Host A) and `192.168.1.11` (Host B). Both returned `Network unreachable` / `0 packets received`.
3. **Zero False Claims:** We strictly refuse to fabricate ping latencies, MTU captures, or physical network traces.

---

## 2. Complete Staging Package Created in `deploy/phase5_7/`

All required deployment artifacts, configurations, scripts, and automation templates have been implemented and validated in dry-run mode:

1. **Topology & Host Profiles:**
   - [`deploy/phase5_7/inventory/hosts.ini`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/inventory/hosts.ini): Inventory mapping Host A (`192.168.1.10`, nodes 0..9) and Host B (`192.168.1.11`, nodes 10..19).
   - [`deploy/phase5_7/two_host/hosts.yaml`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/two_host/hosts.yaml): 2-host profile with coordinator and zone allocations.
   - [`deploy/phase5_7/three_host/hosts.yaml`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/three_host/hosts.yaml): 3-host profile (7+7+6 nodes) providing single-host fault tolerance.
   - [`deploy/phase5_7/five_host/hosts.yaml`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/five_host/hosts.yaml): 5-host production profile (4 nodes per host) tolerating 2 full machine crashes.
2. **Hardened Systemd Templates:**
   - [`deploy/phase5_7/systemd/fsp-node@.service`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/systemd/fsp-node@.service): Hardened instance unit with `LimitNOFILE=65536`, `LimitMEMLOCK=infinity`, `TasksMax=infinity`, `KillMode=control-group`, `Restart=always`, `RestartSec=2s`.
   - [`deploy/phase5_7/systemd/fsp-coordinator.service`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/systemd/fsp-coordinator.service): Distributed coordinator unit.
3. **PKI & TLS Automation:**
   - [`deploy/phase5_7/certificates/generate_pki.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/certificates/generate_pki.sh): Generates root CA and 21 node/coordinator certificates with physical IP and DNS Subject Alternative Names (SANs).
4. **Operations & Verification Scripts:**
   - [`deploy/phase5_7/scripts/preflight.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/scripts/preflight.sh): Validates CPU, RAM, NIC MTU, open file limits, and ext4 mount barriers (supports `--dry-run`).
   - [`deploy/phase5_7/scripts/deploy_two_host.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/scripts/deploy_two_host.sh): Dry-run and execution orchestrator.
   - [`deploy/phase5_7/scripts/health_check.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/scripts/health_check.sh): Probes coordinator and node endpoints.
   - [`deploy/phase5_7/scripts/durability_audit.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/scripts/durability_audit.sh): Scans and audits on-disk container files.
   - [`deploy/phase5_7/supervision/orphan_sweeper.sh`](file:///home/rade/Project/Jobs/fsp/deploy/phase5_7/supervision/orphan_sweeper.sh): Safe orphan daemon sweeper.

---

## 3. Resilience Bounds of a Two-Host Deployment

An essential theoretical insight verified during Phase 5.7 is the mathematical resilience limitation of 2 physical machines under RS(20,12):

| Metric | Two-Host Staging | Three-Host Staging | Five-Host Production |
| :--- | :---: | :---: | :---: |
| **Total Physical Hosts** | 2 | 3 | 5 |
| **Shards per Host** | 10 on Host A, 10 on Host B | 7 on H1, 7 on H2, 6 on H3 | 4 on each host |
| **Loss of 1 Host: Shards Lost** | 10 | 7 | 4 |
| **Surviving Shards** | **10** | **13** | **16** |
| **Reconstruction Threshold ($K=12$)** | **FAIL ($10 < 12$)** | **PASS ($13 \ge 12$)** | **PASS ($16 \ge 12$)** |
| **Loss of 2 Hosts: Surviving Shards** | 0 | 6 (FAIL) | **12 (PASS: $12 == K$)** |

### Critical Architectural Finding:
A two-host topology **cannot provide full fault tolerance against the loss of one physical machine**. If Host A catches fire or loses power, 10 shards are lost simultaneously. The remaining 10 shards on Host B are insufficient to reconstruct the object via RS(20,12), which mathematically demands at least $K=12$ surviving shards.

Therefore, 2-host staging is strictly intended for validating remote physical network routing, mTLS handshakes across interfaces, and multi-process persistence. For true physical host crash survivability, a minimum of **3 physical machines** (or 5 machines for dual-machine fault tolerance) is mandatory.
