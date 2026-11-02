# FSP Phase 4.2: Security Review & Threat Analysis

## 1. Threat Vectors Audited

### 1. Split-Brain & Stale Coordinator Writes
- **Vulnerability:** A partitioned coordinator attempts to write shards or commit manifests while a new leader has been elected.
- **Defense:** Monotonic leadership epoch fencing. Storage nodes check `highest_seen_epoch` on all mutating RPCs (`PutShardFenced`, `DeleteShardFenced`) and reject stale epochs with `RpcError::Fenced`. Verified in `test_p4_2_04`, `test_p4_2_05`, and `test_p4_2_13`.

### 2. Quorum Hijacking & Rogue Leadership
- **Vulnerability:** A rogue coordinator attempts to claim leadership without consensus.
- **Defense:** Strict 3-member quorum consensus requiring $\ge 2$ votes. Epoch advancement requires fsync on quorum members before leadership is granted.

### 3. Manifest Rollback & Replay
- **Vulnerability:** An attacker replays an older validly signed manifest to revert data.
- **Defense:** Per-object monotonic generation watermarking on disk (`<coord_root>/generations/<object_id>.gen`). Stale generations are rejected. Verified in `test_p4_1_08` and `test_p4_2_08`.

### 4. Wire Eavesdropping & Tampering
- **Vulnerability:** Man-in-the-middle inspection or modification of RPC payloads.
- **Defense:** Mandatory TLS 1.3 with mutual certificate authentication (`mTLS`). Shard bodies carry independent SHA-256 digests validated prior to disk commit.

### 5. Storage Node Daemon Capacity Starvation
- **Vulnerability:** Malicious PUT floods exhaust disk space.
- **Defense:** Node daemons enforce pre-allocation admission checks against `capacity_limit_bytes` and return `RpcError::QuotaExceeded`.
