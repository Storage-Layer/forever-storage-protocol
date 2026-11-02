# FSP Phase 4.1 — Self-Healing Protocol Specification

## 1. Lifecycle

The end-to-end distributed self-healing protocol guarantees full redundancy restoration without human intervention:

1. **Detection:** Detected via background scrubbing or failed read transactions.
2. **Admission & Queueing:** Persisted to durable repair journal with deduplication.
3. **Quorum Fetching:** Surviving shards ($k$) fetched from remote nodes and verified against placement SHA-256 digests.
4. **Reconstruction:** Erasure decoding reconstitutes missing shard bytes.
5. **Deployment:** Reconstructed shard uploaded to target spare node via `PutShard`.
6. **Independent Verification:** Coordinator executes streaming verification of the written shard.
7. **Monotonic Generation Advance:** A new signed manifest is published with updated placement topology and incremented generation.
