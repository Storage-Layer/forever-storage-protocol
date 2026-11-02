# FSP Phase 5.1 — Physical Deployment Architecture & Staging Guide

## 1. Physical Deployment Status
> [!IMPORTANT]
> **PHYSICAL BARE-METAL TEST EXECUTION: NOT EXECUTED**  
> All physical deployment validation in Phase 5.1 was conducted via localhost multi-process staging simulation and preflight configuration auditing. Tests on bare-metal physical hardware were not executed in this environment.  
> **PRODUCTION READINESS IS NOT ESTABLISHED** until physical multi-host deployment with independent network interfaces and real disk controllers is independently validated.

## 2. 5-Host Physical Architecture Target
The target physical staging topology consists of 5 physical bare-metal servers hosting 20 storage node daemons (4 nodes per host) and a 5-voter Raft control plane:

| Host Domain | IP Address | Storage Nodes | Rack Domain | Control Plane Role |
|---|---|---|---|---|
| `host-01` | `10.0.1.11` | `node-01`, `node-02`, `node-03`, `node-04` | `rack-01` | Voter 1 (Leader Candidate) |
| `host-02` | `10.0.1.12` | `node-05`, `node-06`, `node-07`, `node-08` | `rack-01` | Voter 2 |
| `host-03` | `10.0.1.13` | `node-09`, `node-10`, `node-11`, `node-12` | `rack-02` | Voter 3 |
| `host-04` | `10.0.1.14` | `node-13`, `node-14`, `node-15`, `node-16` | `rack-02` | Voter 4 |
| `host-05` | `10.0.1.15` | `node-17`, `node-18`, `node-19`, `node-20` | `rack-03` | Voter 5 |

## 3. Placement & Durability Constraints
- **Coding Scheme:** Systematic Cauchy Reed-Solomon RS(20,12) with 12 data shards and 8 parity shards.
- **Maximum Shards Per Host:** Strict ceiling of 4 shards per physical host.
- **Host Failure Invariant:** The simultaneous catastrophic loss of any 2 full physical hosts loses at most $2 \times 4 = 8$ shards, leaving exactly $12$ surviving shards across the remaining 3 hosts. Because $12 \ge k=12$, data reconstructibility is 100% guaranteed.

## 4. Staging Configuration Artifacts
- Cluster Configuration: [deploy/phase5_1/staging/cluster_staging.toml](file:///home/rade/Project/Jobs/fsp/deploy/phase5_1/staging/cluster_staging.toml)
- Preflight Validation Script: [deploy/phase5_1/preflight/preflight_check.sh](file:///home/rade/Project/Jobs/fsp/deploy/phase5_1/preflight/preflight_check.sh)
- Systemd Units: [deploy/phase5_1/systemd/fsp-coordinator.service](file:///home/rade/Project/Jobs/fsp/deploy/phase5_1/systemd/fsp-coordinator.service), [deploy/phase5_1/systemd/fsp-node.service](file:///home/rade/Project/Jobs/fsp/deploy/phase5_1/systemd/fsp-node.service)
