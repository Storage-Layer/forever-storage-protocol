# FSP Phase 4.1 — Merkle-Tree Integrity Architecture

## 1. Merkle-Based Verification Research

To support massive objects (multi-gigabyte to terabyte scales) with minimal network transfer and fast spot verification:

1. **Stripe-Level Merkle Trees:**
   - Shard containers are split into fixed blocks (e.g. 64 KiB chunks).
   - Leaves correspond to chunk SHA-256 digests.
   - Interior nodes compute parent digests: $H_{parent} = \text{SHA-256}(H_{left} \parallel H_{right})$.
   - The Merkle root is recorded directly in the shard container header.

2. **Sub-Shard Proof of Retrievability (PoR):**
   - Coordinator challenges a node for chunk $i$ along with its audit path ($O(\log M)$ hashes).
   - Allows verification without downloading the entire shard payload over the network.
