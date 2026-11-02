# FSP Phase 5.3: Formal Model Checking & Invariant Proofs

## 1. Specification of Bounded Distributed State Machine
The formal model exploration engine in [`src/testing/formal_model_v2.rs`](file:///home/rade/Project/Jobs/fsp/src/testing/formal_model_v2.rs) models the state space:
$$S = \langle \mathcal{N}, \mathcal{C}, \mathcal{M}, \mathcal{R}, \mathcal{G}, \mathcal{I} \rangle$$
where:
- $\mathcal{N}$: Array of 20 storage nodes with local epochs, commit indices, adopted incarnations, and container state.
- $\mathcal{C}$: Set of 3 coordinators holding Raft-style terms, log indices, and leadership certificates.
- $\mathcal{M}$: Set of object manifests with generations and placement records across 5 failure domains.
- $\mathcal{R}$: Active repair reservations mapping $(O, s) \to \text{NodeId}$.
- $\mathcal{G}$: Shard deletion lifecycle state mapping $(O, s, v) \to \text{ShardDeletionState}$.
- $\mathcal{I}$: Cluster incarnation history $\langle \mathcal{I}_1, \mathcal{I}_2, \dots \rangle$.

## 2. Invariant Specifications

### Invariant 1: Incarnation Monotonicity & Authority Uniqueness
$$\forall n \in \mathcal{N}, \quad \text{permit } P \text{ accepted by } n \implies \mathcal{I}_P.\sigma = n.\mathcal{I}.\sigma \land \mathcal{I}_P.\tau = n.\mathcal{I}.\tau$$
No storage node ever executes an operation from an older or divergent incarnation.

### Invariant 2: Mutual Exclusion of Repair and Garbage Collection
$$\forall (O, s, v), \quad \mathcal{G}(O, s, v) \in \{\text{DeletionReserved}, \text{Tombstoned}, \text{PhysicallyDeleted}\} \implies (O, s) \notin \mathcal{R}$$
No shard marked for deletion or tombstoning is ever simultaneously under active repair.

### Invariant 3: No Deletion of Active Committed Generations
$$\forall (O, s, v), \quad \mathcal{G}(O, s, v) = \text{PhysicallyDeleted} \implies v < \mathcal{M}(O).\text{committed\_generation}$$
A shard belonging to the currently active generation can never transition to `PhysicallyDeleted`.

### Invariant 4: Redundancy Preservation Floor
$$\forall O \in \mathcal{M}, \quad |\{s \in \text{shards}(O) \mid \text{alive}(s) \land \neg \text{corrupted}(s)\}| \ge 12$$
At all reachable non-absorbing states, surviving healthy shards never drop below $k=12$.

## 3. Model Checking Results
- **States Explored:** 250,000 states.
- **Search Depth:** 32 transitions.
- **Counterexamples Found:** 0 violations of Invariants 1–4.
- **Report File:** [`results/phase5_3/model_check_results.json`](file:///home/rade/Project/Jobs/fsp/results/phase5_3/model_check_results.json)
