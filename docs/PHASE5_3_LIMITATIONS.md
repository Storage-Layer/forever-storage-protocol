# FSP Phase 5.3: Protocol Limitations, Assumptions & Unproven Claims

## 1. Explicit Protocol Assumptions
The Forever Storage Protocol in Phase 5.3 relies upon the following formal operational assumptions:
1. **Partially Synchronous Network:** Bounded message delivery delay $\Delta$ holds during normal consensus operation. If $\Delta$ is unbounded indefinitely, consensus progress stalls (fail-closed).
2. **Crash-Recovery with Non-Volatile Storage:** Nodes may experience crash-restart (SIGKILL, power loss), provided that the underlying filesystem honors `fsync` barriers for `.rec` and `.fspc` writes.
3. **Cryptographic Primitives Security:** Ed25519 signatures and SHA-256 collision resistance are assumed unbreakable.

## 2. Hard Protocol Limitations
1. **Offline Revocation Blindness:**
   A completely air-gapped or offline storage node cannot detect that a recovery principal or signing key has been revoked unless an updated Trust Store / CRL is physically delivered.
2. **Crash During Unlink (Filesystem Non-Atomicity):**
   While container creation and snapshot writes are atomic via `write_and_sync_file` (write-tmp then rename), physical file unlinking (`fs::remove_file`) on POSIX filesystems is not atomic across multiple files without a journaled transaction log.
3. **No Byzantine Fault Tolerance (BFT):**
   FSP is a Crash-Fault-Tolerant (CFT) system with cryptographic authentication. It does **not** protect against arbitrary malicious Byzantine behavior inside the coordinator quorum where a majority of coordinators collude to sign invalid permits.

## 3. Disclaimed & Unproven Claims
- **Indefinite Durability:** FSP does not guarantee indefinite or eternal durability. Physical storage media degrade, bitrot occurs, and hardware must be physically replaced.
- **Production Readiness:** Formally marked **NOT ESTABLISHED** until real physical multi-host hardware staging and multi-day soak tests are completed.
- **Continuous Soak Testing:** 24-hour and 72-hour continuous soak tests are marked **NOT EXECUTED**.
- **Physical Multi-Host Staging:** Bare-metal multi-machine execution is marked **NOT EXECUTED**.
