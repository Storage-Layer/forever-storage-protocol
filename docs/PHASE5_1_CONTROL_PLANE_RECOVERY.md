# FSP Phase 5.1 — Control Plane Recovery & Quorum Availability

## 1. 3-Voter vs 5-Voter Quorum Availability Analysis
A critical reliability research question investigated in Phase 5 and Phase 5.1 is the trade-off between 3-voter and 5-voter metadata quorums under correlated host failures:

| Metric | 3-Voter Control Plane | 5-Voter Control Plane |
|---|---|---|
| Quorum Size ($N$) | 3 | 5 |
| Majority Threshold ($\lfloor N/2 \rfloor + 1$) | 2 | 3 |
| Concurrent Voter Failures Tolerated ($F$) | 1 ($3 - 2 = 1$) | **2** ($5 - 3 = 2$) |
| Behavior Under 2 Host Loss | **Control plane loses quorum (blocked)** | **Control plane remains 100% available** |
| Data Durability Under 2 Host Loss | 100% Intact (RS 20,12) | 100% Intact (RS 20,12) |
| Read Path When Quorum Lost | Secure Emergency Read Required | Normal Online Coordinator GET |

## 2. Experimental Verification
Validated in [tests/phase5_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_validation.rs):
- `test_p5_04`: In a 5-voter quorum with 2 voters terminated, the remaining 3 voters elect a leader, issue permits, and successfully commit distributed mutations.
- `test_p5_05`: In a 3-voter quorum with 2 voters terminated, the control plane safely fails closed (refusing mutations), while underlying RS(20,12) data durability is 100% preserved.

## 3. Control Plane Recovery Procedures
1. **Dynamic Quorum Reconfiguration:** If 2 voters in a 3-voter cluster are permanently lost, the cluster must not attempt ad-hoc reconfiguration that risks split-brain. Instead, the surviving node operates in read-only maintenance mode.
2. **Disaster Recovery Restore:** Restore metadata via [deploy/phase5_1/recovery/disaster_recovery_restore.sh](file:///home/rade/Project/Jobs/fsp/deploy/phase5_1/recovery/disaster_recovery_restore.sh) to establish a fresh 5-voter quorum.
3. **Secure Emergency Offline Read:** If the control plane cannot be restored immediately, tenants utilize [src/security/recovery_auth.rs](file:///home/rade/Project/Jobs/fsp/src/security/recovery_auth.rs) to reconstruct objects offline.
