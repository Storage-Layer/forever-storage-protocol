# FSP Phase 4.3: Adversarial Split-Brain & Partition Test Suite

This document records the 8 adversarial experiments (Experiments A–H) designed to test the limits of FSP's consensus and fencing guarantees under hostile network partitions and delayed message delivery.

---

## 1. Adversarial Scenarios & Experimental Outcomes

### Experiment A: Isolated Minority Leader Mutation Attempt
- **Setup:** A 3-node cluster ($C_1, C_2, C_3$) is partitioned into $\{C_1\}$ (minority) and $\{C_2, C_3\}$ (majority). $C_1$ was the leader for Term 1.
- **Action:** A client submits a manifest mutation to $C_1$.
- **Observed Behavior:** $C_1$ appends the proposal to its local WAL but only receives 1 ACK (itself). Since $1 < \text{threshold}(2)$, $C_1$ cannot advance its `commit_index` and blocks the commit.
- **Outcome:** **PASSED.** No uncommitted state leaked to clients.

### Experiment B: Majority Partition Leadership Acquisition & Storage Advance
- **Setup:** Same partition $\{C_1\}$ vs $\{C_2, C_3\}$.
- **Action:** $C_2$ starts an election for Term 2.
- **Observed Behavior:** $C_2$ collects authentic Ed25519 `QuorumVoteGrant`s from $C_2$ and $C_3$, forming a valid `LeadershipCertificate` for Term 2. $C_2$ presents this certificate to storage nodes, advancing their watermark to epoch 2.
- **Outcome:** **PASSED.** New leader successfully authorized.

### Experiment C: Stale Leader Mutation Post-Failover
- **Setup:** $C_1$ attempts to write a shard to storage nodes using its old Term 1 fencing token.
- **Action:** Storage nodes evaluate the incoming token against their durably recorded epoch 2 watermark.
- **Observed Behavior:** Storage nodes immediately reject the request with `RpcError::Fenced { current_epoch: 2, presented_epoch: 1 }`.
- **Outcome:** **PASSED.** Stale leader mutation strictly blocked.

### Experiment D: Delayed In-Flight RPC Replay
- **Setup:** An in-flight `PutShard` RPC issued by $C_1$ in Term 1 is delayed by 500 ms in the network queue and arrives after $C_2$ has certified Term 2 on all nodes.
- **Observed Behavior:** The storage node validates the presented token against its durable `epoch.rec` (now 2) and rejects the replayed RPC.
- **Outcome:** **PASSED.** Replay attack prevented.

### Experiment E: Concurrent Competing Elections
- **Setup:** Both $C_1$ and $C_2$ propose epoch 1 simultaneously to voter $C_3$.
- **Observed Behavior:** $C_3$ grants a vote to the first candidate observed ($C_1$). When $C_2$'s request arrives, $C_3$ checks its durable `voted_for_in_epoch` record and rejects $C_2$. Only $C_1$ reaches quorum majority.
- **Outcome:** **PASSED.** At most one leader elected per term.

### Experiment F: Rapid Successive Failovers (Term 1 $\to$ 2 $\to$ 3)
- **Setup:** Successive failovers triggered in rapid succession without complete replication between intermediate leaders.
- **Observed Behavior:** Intermediate uncommitted entries are truncated; log prefix matching is strictly maintained; only entries committed by majority quorum survive.
- **Outcome:** **PASSED.** Log matching invariant maintained.

### Experiment G: Crash-Partition Interleaved Recovery
- **Setup:** $C_2$ is SIGKILL-terminated; $C_1$ is partitioned; surviving node $C_3$ initiates recovery with newly joined node.
- **Observed Behavior:** The cluster safely blocks mutations until a majority quorum is re-established.
- **Outcome:** **PASSED.** Fail-closed behavior maintained under multi-failure conditions.

### Experiment H: Network Packet Drop & Asymmetric Link Degradation
- **Setup:** 35% random packet drop injected on quorum replication links.
- **Observed Behavior:** Bounded retry policies with exponential jitter ensure safe progress without false commit acknowledgments.
- **Outcome:** **PASSED.** 0 corrupted records, 0 split-brain incidents.
