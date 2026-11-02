# FSP Phase 5.1 — Operational Runbook & Incident Response Manual

## Incident Procedures

### Procedure 1: Single Storage Node Daemon Failure / Restart
1. Check service status: `systemctl status fsp-node@<id>`.
2. Inspect logs: `journalctl -u fsp-node@<id> -n 100`.
3. Verify filesystem mount: `df -h /var/lib/fsp/node-<id>`.
4. Restart daemon: `systemctl restart fsp-node@<id>`.
5. Verify node reports healthy in cluster status: `fsp cluster status`.

### Procedure 2: Multi-Node Disk Drive Failure on a Host
1. Identify failed disk via kernel logs: `dmesg -T | grep -E 'sd|blk'`.
2. Unmount corrupted volume and replace physical disk.
3. Re-initialize storage root: `fsp storage init --root /mnt/new_disk/node-X`.
4. Adaptive repair scheduler will automatically reconstruct missing shards from parity.

### Procedure 3: Full Physical Host Failure (4 Nodes Lost)
1. Verify surviving hosts (16 nodes surviving): cluster remains in `DegradedButRecoverable`.
2. Online PUT and GET continue uninterrupted.
3. Provision replacement server or trigger repair to available spare nodes.

### Procedure 4: Correlated Two-Host Failure (8 Nodes Lost)
1. 8 shards lost per stripe; 12 shards surviving across remaining 3 hosts.
2. Degraded reads continue with 100% data fidelity.
3. Throttle non-critical workloads to reserve repair network bandwidth.
4. Replace hardware and monitor repair journal: `fsp storage status`.

### Procedure 5: Background Scrubber Bitrot Detection and Healing
1. Scrubber logs bitrot event and increments `fsp_reliability_bitrot_detected_total`.
2. Corrupted shard container is flagged and excluded from read paths.
3. Repair scheduler issues Cauchy RS(20,12) reconstruction task to write fresh shard.

### Procedure 6: Coordinator Process Failure and Automatic Raft Failover
1. Standby voter detects missing leader heartbeat within 2 seconds.
2. Quorum election triggers: new leader acquires majority `LeadershipCertificate`.
3. Storage nodes update epoch watermark. RTO $< 500\text{ ms}$.

### Procedure 7: Coordinator Split-Brain Network Partition Resolution
1. Partitioned minority leader fails to acquire majority permits.
2. Storage nodes fenced by new epoch reject minority leader mutating RPCs.
3. When network heals, partitioned coordinator steps down to follower.

### Procedure 8: Metadata Quorum Loss in 3-Voter Cluster
1. If 2 voters fail in a 3-voter cluster, control plane safely fails closed.
2. Do not attempt uncoordinated quorum reductions.
3. Issue `fsp secure-emergency-read` for critical reads, then restore metadata snapshot.

### Procedure 9: Stale Metadata Backup Restoration Prevention
1. If an operator attempts to restore a stale backup, restore script aborts: `RollbackRejected`.
2. Inspect existing `snapshot.rec` commit index: `COMMIT_INDEX`.
3. Only use `--force` if deliberate point-in-time recovery is formally authorized.

### Procedure 10: Authorized Offline Emergency Data Recovery
1. Generate capability token using tenant key: `fsp token issue --id <obj> --ns <ns>`.
2. Verify token against trust store: `fsp token verify`.
3. Execute secure offline read: `fsp secure-emergency-read --id <obj> --token <tok> --output <path>`.

### Procedure 11: Expired / Forged Recovery Capability Token Handling
1. Audit log records security rejection: `SEC-P51-01`.
2. Investigate source IP and principal ID.
3. Re-issue fresh token from authoritative tenant keypair if genuine expiration.

### Procedure 12: Catastrophic Coordinator Cluster Disaster Restore
1. Stop any remaining coordinator processes: `systemctl stop fsp-coordinator`.
2. Obtain latest verified bundle from `/var/backups/fsp/metadata/`.
3. Execute: `/opt/fsp/deploy/recovery/disaster_recovery_restore.sh <bundle> /var/lib/fsp/coordinator`.
4. Verify ReplicatedLogManager starts cleanly and reports healthy commit index.

### Procedure 13: Storage Node Epoch Fencing Violation Resolution
1. Storage node returns `RpcError::Fenced` when an RPC carries a stale epoch.
2. Coordinator must refresh leadership lease or step down.
3. Client retries against authoritative leader returned in error header.

### Procedure 14: Storage Capacity Exhaustion Repair Blocking Procedure
1. When cluster storage utilization exceeds 95%, placement planner sets `ObjectHealthState::RepairBlockedByCapacity`.
2. Alerts trigger `FspRepairBlocked`.
3. Add physical disk volumes and execute `fsp storage resume-repairs`.

### Procedure 15: Scheduled Rolling Upgrade of Cluster Binaries
1. Upgrade storage nodes one host at a time (`host-01` through `host-05`).
2. Verify node health before advancing to next host: `fsp node health --node <id>`.
3. Upgrade standby coordinators, then trigger graceful failover of primary leader.
