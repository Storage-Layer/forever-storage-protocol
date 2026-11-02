# FSP Phase 5.8: Recovery Time Objective (RTO) and Recovery Point Objective (RPO)

## 1. Durability and Availability Metrics

In distributed storage systems, disaster recovery is quantified by two core metrics:
- **Recovery Point Objective (RPO):** The maximum acceptable age of data that can be lost when an unexpected failure occurs.
- **Recovery Time Objective (RTO):** The maximum duration of time within which the system must be restored to availability after a service disruption.

---

## 2. Recovery Point Objective (RPO = 0)

FSP achieves **$\text{RPO} \equiv 0$** for all client-acknowledged transactions:
1. **Durable Barrier Invariant:** An acknowledgement is never returned to the caller until the payload has been persisted to non-volatile storage (`fsync`) and the directory link has been synchronized (`fsync(parent_dir)`).
2. **Crash Discard Policy:** Any shard write interrupted prior to the return of the client ACK is incomplete and resides in temporary staging. Upon crash recovery, uncommitted staging artifacts are discarded, leaving the storage state completely consistent with the last committed generation.
3. **Quorum Enforcement:** A write is committed only if at least $K=12$ distinct nodes return valid durable ACKs. Even if up to 8 nodes crash immediately after acknowledging, the remaining 12 nodes retain full recoverability.

---

## 3. Recovery Time Objective (RTO)

### Control Plane Failover RTO:
- **Consensus Failover Duration:** When the active coordinator experiences a hard failure, the standby coordinator detects heartbeat loss and initiates an election round across the 3-member consensus quorum.
- **Election Round Trip:** $\approx 50\text{ ms} - 250\text{ ms}$ (local / low-latency LAN).
- **WAL Reconciliation:** $\approx 10\text{ ms} - 30\text{ ms}$ for typical journal lengths.
- **Measured Control Plane RTO:** $< 300\text{ ms}$ until read/write service resumption.

### Data Plane Shard Rebuild RTO:
- In the event of a permanent storage node loss, the background scrub and repair coordinator reconstructs missing shards onto healthy nodes using Cauchy RS decoding ($R = G_S^{-1} \cdot S$).
- **Reconstruction Rate:** Controlled by the administrator-configured repair bandwidth throttle (default $50\text{ MiB/s}$ per node) to avoid saturating client I/O.
