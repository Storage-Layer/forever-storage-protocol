# FSP Phase 4.1 — Durable Node Idempotency Store

## 1. Design & Crash Resilience

Node-level transaction idempotency is implemented in [`DurableIdempotencyStore`](file:///home/rade/Project/Jobs/fsp/src/node/idempotency.rs):
- Location: `<node_root>/idempotency/<tx_id>_<shard_id>.rec`
- Covers mutations: `PutShard`, `RepairShard`, and `DeleteShard`.
- Each record binds `tx_id`, `object_id`, `shard_id`, `version`, and `payload_sha256`.

## 2. Invariants

- **Exact Duplicate:** Returns previously cached receipt with `idempotent_replay: true`.
- **Payload Conflict:** If a duplicate `tx_id` arrives with a differing payload digest, the node rejects the request with `RpcError::IntegrityFailure`.
- **Durability:** Every record is fsynced to disk before transaction acknowledgment is dispatched.
