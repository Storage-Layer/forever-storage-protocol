# Phase 5 Operational Runbook & Incident Response
**Forever Storage Protocol (FSP)**
**Date:** 2026-11-01  
**Author:** Principal Site Reliability Engineer & Operations Lead  
**Target Cluster:** 5-Host Physical / Staging Topology

---

## 1. Routine Cluster Operations

### Starting the Cluster
1. **Verify Network & NTP:**
   ```bash
   chronyc tracking
   ping -c 3 10.0.1.11
   ```
2. **Execute Preflight Checks:**
   ```bash
   fsp preflight --config deploy/phase5/cluster_5host.toml
   fsp topology validate --config deploy/phase5/cluster_5host.toml
   ```
3. **Start Storage Node Daemons (on each host):**
   ```bash
   sudo systemctl start fsp-node@{1..4}.service
   ```
4. **Start Coordinator Daemons (on voter hosts):**
   ```bash
   sudo systemctl start fsp-coordinator.service
   ```
5. **Verify Cluster Health:**
   ```bash
   fsp cluster status --config deploy/phase5/cluster_5host.toml
   ```

---

## 2. Alert Response & Incident Workflows

### Alert: `FspStorageDegraded` (1 to 8 Shards Missing)
- **Severity:** WARNING
- **Impact:** Data is 100% durable and accessible. Redundancy margin is reduced.
- **Workflow:**
  1. Inspect the degraded object:
     ```bash
     fsp storage inspect <object-id> --root /var/lib/fsp/coordinator
     ```
  2. Check if hosting storage node is unreachable or if disk died:
     ```bash
     systemctl status fsp-node@<node_id>.service
     ```
  3. If node is permanently offline, trigger manual or adaptive self-healing:
     ```bash
     fsp storage repair <object-id> --root /var/lib/fsp/coordinator
     ```

### Alert: `FspRepairBlockedByCapacity`
- **Severity:** HIGH
- **Impact:** Shards cannot be rebuilt because no surviving host has $< 4$ shards.
- **Workflow:**
  1. Provision a new physical storage server or replace failed drives on the offline machine.
  2. Start the new storage node daemon with its new `host_domain`.
  3. Re-run `fsp topology validate` to ensure new host satisfies placement constraints.
  4. Adaptive repair scheduler will automatically resume and reconstruct redundancy back to `FullyProtected`.

### Alert: `FspQuorumLost` (Control Plane Offline)
- **Severity:** CRITICAL
- **Impact:** Metadata quorum lost ($< 3$ voters active). Online mutations and online reads fail closed. Data durability is NOT lost if surviving shards $\ge 12$.
- **Workflow:**
  1. **DO NOT PANIC.** The data plane is safe.
  2. If an urgent read is required while reviving hosts, invoke emergency offline recovery:
     ```bash
     fsp emergency-read --id <object-id> --config deploy/phase5/cluster_5host.toml --output /tmp/restored.bin
     ```
  3. Revive at least 1 failed coordinator host to re-establish $3$ of 5 voter quorum.
  4. Once 3 voters communicate, coordinator election succeeds, WAL replays, and normal online service resumes.
