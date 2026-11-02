# Phase 5.4 Authority Coexistence & Incarnation Fencing

## 1. Problem Statement: Split-Brain In A Distributed Storage Cluster

In a distributed cluster with 3 coordinators and network partitions:
- An isolated coordinator (Coordinator 0) may believe it is still the leader.
- A new leader (Coordinator 1) may be elected by a majority quorum (Coordinators 1 & 2) and advance its incarnation epoch from 1 to 2.
- If Coordinator 0 attempts to issue destructive mutations (e.g. shard relocation, garbage collection tombstones, or metadata updates) to storage nodes, data corruption could occur unless storage nodes strictly enforce fencing tokens.

## 2. Monotonic Incarnation Architecture

FSP enforces node-side incarnation fencing:
1. Every mutation permit issued by a coordinator includes:
   - `coordinator_id`: ID of the issuing coordinator
   - `incarnation`: Monotonically increasing 64-bit epoch
   - `timestamp_ms`: Issuance timestamp
   - `lease_expiry_ms`: Bounded lease duration (default 15,000ms)
   - `action`: Specific permitted operation
   - `signature`: Ed25519 signature over permit fields
2. Each storage node maintains `highest_seen_incarnation` in its persistent manifest DB.
3. Upon receiving any mutating RPC, the node compares `permit.incarnation` against `highest_seen_incarnation`:
   - If `permit.incarnation < highest_seen_incarnation`: The RPC is immediately rejected with `FspError::StaleIncarnation`.
   - If `permit.incarnation > highest_seen_incarnation`: The node updates its persistent manifest with the new incarnation and processes the RPC.
   - If `permit.timestamp_ms + permit.lease_expiry_ms < now`: The RPC is rejected with `FspError::PermitExpired`.

## 3. Test Verification: `test_p5_4_11_authority_coexistence_and_stale_permit_rejection`

In Phase 5.4 validation:
1. Coordinator 0 issued Permit A at Incarnation 1.
2. A leader election occurred; Coordinator 1 advanced the cluster incarnation to 2.
3. Storage nodes received Permit B with Incarnation 2 and committed it.
4. A delayed / replayed Permit A with Incarnation 1 was sent to the storage nodes.
5. The storage nodes rejected Permit A, returning `StaleIncarnation`. No mutations occurred.
