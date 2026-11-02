# FSP Phase 4.2: Distributed Fencing and Write Authorization

## 1. Problem Definition: Split-Brain Prevention

In a distributed storage system, an active leader may experience a temporary network freeze, garbage collection pause, or network partition. If a standby coordinator promotes itself and begins serving writes while the old leader is still running, both coordinators might attempt conflicting mutations on storage nodes.

To eliminate split-brain write hazards without distributed transaction locks, FSP enforces **Monotonic Epoch Fencing**.

---

## 2. Storage Node-Side Epoch Fencing

Every storage node maintains a persistent, crash-safe fencing tracker: [`NodeEpochFencingTracker`](file:///home/rade/Project/Jobs/fsp/src/coordinator/fencing.rs) stored at `<node_root>/epoch.rec`.

### Invariant Rules:
1. **Durable Watermark:** The node stores `highest_seen_epoch: AtomicU64`.
2. **Mutating RPC Binding:** All mutating RPCs (`PutShardFenced`, `DeleteShardFenced`) include `epoch: u64`.
3. **Rejection of Stale Leaders:**
   $$\text{If } E_{\text{presented}} < E_{\text{node\_seen}} \implies \text{REJECT with } \texttt{RpcError::Fenced}$$
4. **Advancement of Watermark:**
   $$\text{If } E_{\text{presented}} > E_{\text{node\_seen}} \implies \text{advance } E_{\text{node\_seen}} = E_{\text{presented}} \text{ and } \texttt{fsync(epoch.rec)}$$

```text
Stale Coordinator (Epoch 10)       Storage Node (Epoch 12)       Authoritative Leader (Epoch 12)
          |                                  |                                  |
          |--- PutShardFenced(epoch=10) ---->|                                  |
          |                                  |                                  |
          |<-- RpcError::Fenced(12, 10) -----|                                  |
          |    [STALE WRITE REJECTED]        |                                  |
          |                                  |<-- PutShardFenced(epoch=12) -----|
          |                                  |                                  |
          |                                  |--- PutShardAck(OK) ------------->|
```

---

## 3. Crash Recovery and SIGKILL Durability

Because the node writes and synchronizes `<node_root>/epoch.rec` prior to acknowledging an epoch advance:
- If a storage node daemon process is abruptly terminated with `SIGKILL`, the kernel immediately terminates all open sockets.
- Upon respawn, [`StorageNodeDaemon::init_or_open`](file:///home/rade/Project/Jobs/fsp/src/node/daemon.rs) reloads `highest_seen_epoch` from disk.
- Replayed requests from superseded coordinators continue to be rejected after process restart.
