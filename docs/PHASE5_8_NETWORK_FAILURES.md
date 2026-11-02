# FSP Phase 5.8: Network Failure Handling, Connection Resets & Idempotency

## 1. Network Failure Fault Model

Distributed storage systems operating over real LAN networks experience transient TCP drops, connection resets (`ECONNRESET`), packet delays, and intermittent network partitions.

FSP handles network faults via three foundational layers:
1. **Isolated Daemon State on Connection Reset:**
   - When a client or coordinator connection drops abruptly mid-stream, the daemon traps the socket disconnection and tears down the connection context.
   - Any uncommitted staging files for the interrupted transaction are discarded without polluting active storage.
2. **Client Retry Backoff with Jitter:**
   - Reconnections and retried RPCs follow bounded exponential backoff with full randomized jitter to prevent cluster-wide retry storms (thundering herds).
3. **Durable Idempotency Store:**
   - Every mutating request carries a client-assigned unique transaction ID (`tx_id`).
   - If a network failure occurs after a shard was durably committed but before the ACK reached the coordinator, the client retries the exact same `tx_id`.
   - The daemon checks its durable idempotency WAL, identifies the transaction as already committed, and returns the cached success status without re-writing or corrupting data.

---

## 2. Linux Traffic Control (Netem) Fault Injection Framework

To facilitate reproducible LAN network fault injection when physical secondary hosts become available, FSP provides `deploy/phase5_8/fault_injection/netem_partition.sh`:
- **Latency & Jitter Injection:** Simulates inter-datacenter or congested switch delays (`tc qdisc add dev <iface> root netem delay 25ms 5ms`).
- **Packet Loss Simulation:** Introduces controlled random packet loss (`netem loss 5%`).
- **Corruption Injection:** Injects bit flips into network frames (`netem corrupt 0.1%`) to test CRC32C and TLS MAC rejection.
- **Clean Teardown:** Completely removes traffic shaping qdiscs upon test completion (`tc qdisc del dev <iface> root`).

---

## 3. Test Verification

Verified by automated integration tests:
- `test_p5_8_12_network_connection_reset_and_isolation` (Passed)
- `test_p5_8_13_client_timeout_and_retry_backoff` (Passed)
- `test_p5_8_14_idempotent_put_transaction_replay` (Passed)
