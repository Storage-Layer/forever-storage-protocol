# FSP Phase 5.8: Consensus Availability, Coordinator Failover & Epoch Fencing

## 1. Consensus Quorum Architecture

FSP control-plane operations are coordinated across a 3-member voting ensemble:
1. `coordinator-primary` (Voter 1)
2. `coordinator-standby` (Voter 2)
3. `witness-01` (Voter 3, lightweight raft-only quorum participant)

Under standard consensus rules:
- **Total Voters ($N$):** 3
- **Quorum Requirement ($Q$):** $\lfloor N/2 \rfloor + 1 = 2$ votes.

---

## 2. Failover Scenarios and Safety Analysis

### Scenario A: Primary Coordinator Failure
- Surviving voters: `coordinator-standby` + `witness-01` = 2 voters.
- $2 \ge 2$: Majority consensus maintained.
- `coordinator-standby` steps up to active Leader, increments the leadership epoch ($e_{t+1} > e_t$), issues signed quorum certificates, and resumes storage operations.

### Scenario B: Simultaneous Loss of Two Voters
- Surviving voters: 1 voter.
- $1 < 2$: Majority consensus lost.
- **Fail-Closed Durability:** The remaining coordinator strictly refuses to accept write or delete permits. Read operations targeting existing committed manifests remain accessible if quorum of storage nodes is online.

---

## 3. Stale Leadership Permit Fencing

To prevent "split-brain" dual-primary hazards (e.g. during asymmetric network partitions):
- Every write or stage permit issued by a coordinator embeds the monotonically increasing leadership epoch ($e$).
- Each storage node maintains an atomic and durable `highest_seen_epoch` watermark.
- If a demoted or partitioned former primary attempts to commit writes with an outdated epoch ($e_{\text{old}} < e_{\text{current}}$), storage nodes reject the permit with `StaleEpochFencingError`.

---

## 4. Test Verification

Verified by automated integration tests:
- `test_p5_8_15_coordinator_failover_under_three_member_quorum` (Passed)
- `test_p5_8_16_stale_leadership_permit_fencing` (Passed)
