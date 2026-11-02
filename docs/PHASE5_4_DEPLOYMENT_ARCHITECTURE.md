# Phase 5.4 Deployment Architecture & Multi-Zone Topology

## 1. Failure Domain Model

The FSP distributed storage cluster uses a 5-zone architecture designed around Reed-Solomon RS(20,12) with $N=20, K=12, M=8$.

```
+-----------------------------------------------------------------------------------+
|                            FSP 5-ZONE STORAGE CLUSTER                             |
+-----------------------------------------------------------------------------------+
|  Zone 0        |  Zone 1        |  Zone 2        |  Zone 3        |  Zone 4       |
|  Nodes 0..3    |  Nodes 4..7    |  Nodes 8..11   |  Nodes 12..15  |  Nodes 16..19 |
|  4 Shards      |  4 Shards      |  4 Shards      |  4 Shards      |  4 Shards     |
+-----------------------------------------------------------------------------------+
```

### Invariants:
1. **Single-Zone Outage (4 nodes down):**  
   16 nodes remain alive. Exceeds $K=12$ reconstruction threshold by 4 shards.
2. **Two-Zone Outage (8 nodes down):**  
   12 nodes remain alive. Exactly equals $K=12$. Reconstructs data with 0 data loss.
3. **Three-Zone Outage (12 nodes down):**  
   Only 8 nodes remain alive. Below $K=12$. Rejects read and fails closed safely (`InsufficientValidShards`).

## 2. Deployment Profiles in `deploy/phase5_4/`

- **`deploy/phase5_4/localhost/`**:  
  Runs all 20 nodes and 3 coordinators on 127.0.0.1 across distinct non-overlapping ports (`9000..9002` for coordinators, `9100..9119` for storage nodes). Used for automated integration tests and developer staging.
- **`deploy/phase5_4/two_host/`**:  
  Distributes 20 nodes across 2 physical or virtual machines (10 nodes each). Zones 0-1 on Host 1, Zones 3-4 on Host 2, Zone 2 split.
- **`deploy/phase5_4/three_host/`**:  
  Distributes 20 nodes and 3 coordinators across 3 physical servers, ensuring each coordinator resides on a separate server for quorum tolerance.
- **`deploy/phase5_4/five_host/`**:  
  Canonical 1:1 physical deployment where each physical machine represents exactly one failure zone (4 storage nodes per machine). A total physical failure of 2 full machines preserves 100% data durability.

## 3. Network Ports & Mutual TLS Architecture

| Component | Default Port Range | Protocol | Authentication |
|---|---|---|---|
| Coordinator Quorum | 9000 - 9002 | TCP / TLS 1.3 | Mutual TLS (mTLS) |
| Storage Nodes (Zone 0) | 9100 - 9103 | TCP / TLS 1.3 | Mutual TLS (mTLS) |
| Storage Nodes (Zone 1) | 9104 - 9107 | TCP / TLS 1.3 | Mutual TLS (mTLS) |
| Storage Nodes (Zone 2) | 9108 - 9111 | TCP / TLS 1.3 | Mutual TLS (mTLS) |
| Storage Nodes (Zone 3) | 9112 - 9115 | TCP / TLS 1.3 | Mutual TLS (mTLS) |
| Storage Nodes (Zone 4) | 9116 - 9119 | TCP / TLS 1.3 | Mutual TLS (mTLS) |
| Prometheus Metrics | 9200 - 9219 | HTTP | Internal / Bearer |
