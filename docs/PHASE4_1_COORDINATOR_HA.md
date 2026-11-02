# FSP Phase 4.1 — Active/Passive Coordinator High Availability Architecture

## 1. Overview & Single Point of Failure Remediation

The coordinator manages manifest signatures, transaction coordination, and repair scheduling. To eliminate the coordinator as a single point of failure (SPOF):

## 2. Active/Passive Architecture

```mermaid
flowchart LR
    subgraph Storage Network
        N1[Storage Node 1]
        N2[Storage Node 2]
        NN[Storage Node N]
    end

    subgraph Coordinator HA Pair
        Primary[Active Coordinator] -->|Heartbeats & Leases| SharedStore[(Shared Lease / Raft State)]
        Standby[Passive Standby Coordinator] -->|Monitors Lease| SharedStore
        Primary -->|Async Journal Replicate| Standby
    end

    Primary -->|mTLS RPC| N1
    Primary -->|mTLS RPC| N2
    Primary -->|mTLS RPC| NN
```

1. **Lease Fencing:** The Active Coordinator acquires an epoch-based lease with monotonic fencing tokens.
2. **Standby Failover:** If the active lease expires without renewal, the passive coordinator promotes itself, acquires the lease, replays the durable repair journal and generation watermarks, and assumes traffic.
3. **Storage Node Lease Enforcement:** Storage nodes reject requests carrying stale fencing tokens.
