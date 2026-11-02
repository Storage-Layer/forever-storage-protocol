# FSP Phase 4.2: Metadata Replication and Recovery Architecture

## 1. Overview

To guarantee that standby coordinators can take over immediately upon leader crash without losing committed objects or violating manifest monotonicity, Phase 4.2 implements an append-only Write-Ahead Log: [`MetadataReplicationWal`](file:///home/rade/Project/Jobs/fsp/src/coordinator/replication.rs) located at `<coord_root>/metadata_replication.wal`.

---

## 2. Replicated Record Types

The replication stream captures all state transitions required to resume operations safely:
1. `ManifestCommitted`: Full manifest text, generation number, object ID, and SHA-256 digest.
2. `GenerationWatermarkAdvanced`: Object ID and new high-water mark generation.
3. `RepairJobStateChanged`: Job ID, repair state (`Pending`, `Scheduled`, `Repairing`, `Completed`), target node, and retry timestamps.
4. `NodeRegistered`: Storage node ID, listen address, and physical failure domain topology.

---

## 3. Standby Replay and Rollback Protection

When a standby coordinator reads the WAL during normal operation or upon failover promotion:
- **Generation Rollback Defense:**
  - $\text{Generation} < \text{recorded}$: Explicitly rejected.
  - $\text{Generation} = \text{recorded}$ and $\text{digest} = \text{recorded}$: Allowed as idempotent read/verification.
  - $\text{Generation} = \text{recorded}$ and $\text{digest} \ne \text{recorded}$: Rejected as conflicting split-brain manifest!
  - $\text{Generation} > \text{recorded}$: Accepted and written to `<coord_root>/generations/<object_id>.gen`.
- **Private Key Isolation:**
  - The replication WAL replicates **public signing references** and **signed manifest envelopes**, never the Ed25519 private signing keys.
  - Standby coordinators maintain their own independent cryptographic credentials.
