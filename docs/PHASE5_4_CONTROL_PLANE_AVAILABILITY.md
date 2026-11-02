# Phase 5.4 Control Plane Availability & Quorum Failover

## 1. 3-Member Quorum Architecture

FSP metadata is maintained by a 3-member coordinator quorum operating on consensus principles:
- **Total Members:** 3 ($C_0, C_1, C_2$)
- **Quorum Requirement:** $Q = \lfloor 3/2 \rfloor + 1 = 2$ members
- **Fault Tolerance:** Can withstand the failure of any 1 coordinator without interrupting read or write availability.

```
       +---------------------------------------------+
       |             Coordinator Quorum              |
       |                                             |
       |  [ Coord 0 ] <======> [ Coord 1 ] (Leader)  |
       |        \                  /                 |
       |         \                /                  |
       |          v              v                   |
       |            [ Coord 2 ]                      |
       +---------------------------------------------+
```

## 2. Failover Latency & Timing

| Metric | Target | Measured |
|---|---|---|
| Heartbeat Interval | 100 ms | 100 ms |
| Leader Election Timeout | 250 - 400 ms (jittered) | 280 ms |
| Total Failover Time (RTO) | < 1000 ms | ~350 ms |
| Metadata Loss (RPO) | 0 records | 0 records (Synchronous WAL replication) |

## 3. Network Partition & Asymmetric Split Recovery

### Test Case: `test_p5_4_13_metadata_quorum_partition_and_reconnection`
- **Scenario:** Coordinator 2 is isolated by a simulated firewall partition.
- **Observations:**
  - Majority partition ($C_0, C_1$) retains $2/3$ quorum and continues servicing client write requests.
  - Minority partition ($C_2$) cannot achieve a quorum of 2, so all write attempts on $C_2$ fail closed with `FspError::NoQuorumAvailable`.
  - Upon partition healing, $C_2$ synchronizes missing log entries via the Write-Ahead Log (WAL) catchup protocol and resumes active follower duties.
