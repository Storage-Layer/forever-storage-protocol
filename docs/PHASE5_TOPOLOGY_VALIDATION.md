# Phase 5 Topology Validation & Preflight Audit
**Forever Storage Protocol (FSP)**
**Date:** 2026-11-01  
**Author:** Principal Storage Reliability Engineer & Site Reliability Engineer  
**Source CLI:** [`src/cli/mod.rs`](file:///home/rade/Project/Jobs/fsp/src/cli/mod.rs), [`src/config/validation.rs`](file:///home/rade/Project/Jobs/fsp/src/config/validation.rs)

---

## 1. Overview & Purpose

Before commissioning physical storage hardware or starting distributed daemons, storage administrators must verify that physical deployment topology satisfies the erasure coding durability invariants.

The FSP Preflight CLI provides non-invasive, zero-side-effect validation of:
1. Physical host distribution and failure domain independence.
2. Erasure code placement durability (e.g. RS(20,12) 2-host loss tolerance).
3. Network endpoints, IP addresses, and port conflict detection.
4. PKI certificates, private keys, and authority trust chains.

---

## 2. CLI Commands & Usage

### 1. `fsp preflight --config <path>`
Executes comprehensive preflight checks against a cluster deployment specification.

```bash
cargo run --release -- preflight --config deploy/phase5/cluster_5host.toml
# Output:
PREFLIGHT OK: 20 nodes across 5 hosts (max 4/host)
```

### 2. `fsp topology validate --config <path>`
Performs deep failure domain combinatorial analysis, calculating the worst-case shard loss under any $k$ simultaneous host or rack failures.

```bash
cargo run --release -- topology validate --config deploy/phase5/cluster_5host.toml
# Output:
TOPOLOGY VALID: 20 nodes, 5 hosts, min surviving shards: 12
```

### 3. `fsp cluster status --config <path>`
Summarizes node distribution and host allocations across the cluster.

```bash
cargo run --release -- cluster status --config deploy/phase5/cluster_5host.toml
# Output:
CLUSTER STATUS | coordinator_id=coordinator-1
Nodes configured: 20
  host host-alpha: 4 nodes
  host host-bravo: 4 nodes
  host host-charlie: 4 nodes
  host host-delta: 4 nodes
  host host-echo: 4 nodes
```

---

## 3. Preflight Failure Domain Rules

To ensure mathematical durability under RS(20,12) ($n=20, k=12, m=8$):

1. **Distinct Physical Hosts:** The cluster MUST contain at least 5 distinct physical host domains ($\ge 5$).
2. **Maximum Nodes Per Host:** No single physical host may run more than 4 storage daemons ($\le 4$). If a host ran 5 daemons, losing that host and any other 4-node host would lose $5 + 4 = 9$ shards, leaving only $11 < 12$ surviving shards, causing permanent data loss!
3. **Worst-Case 2-Host Loss Invariant:**
   $$\text{Max Two Host Loss} = \max_{h_i \neq h_j} (\text{count}(h_i) + \text{count}(h_j)) \le n - k = 8$$
   $$\text{Min Surviving Shards} = n - \text{Max Two Host Loss} \ge k = 12$$

If any of these conditions are violated, preflight validation immediately aborts with an actionable error (verified in [`test_p5_07_preflight_cli_detects_co_located_daemons_on_same_host`](file:///home/rade/Project/Jobs/fsp/tests/phase5_validation.rs)).

---

## 4. Audited Production Topology: `deploy/phase5/cluster_5host.toml`

| Host Domain | IP Address | Rack Domain | Region | Nodes Allocated | Max Shards Placed |
|---|---|---|---|---|---|
| `host-alpha` | `10.0.1.11` | `rack-1` | `region-eu-1` | Nodes 1, 2, 3, 4 | 4 |
| `host-bravo` | `10.0.1.12` | `rack-1` | `region-eu-1` | Nodes 5, 6, 7, 8 | 4 |
| `host-charlie` | `10.0.1.13` | `rack-2` | `region-eu-1` | Nodes 9, 10, 11, 12 | 4 |
| `host-delta` | `10.0.1.14` | `rack-2` | `region-eu-1` | Nodes 13, 14, 15, 16 | 4 |
| `host-echo` | `10.0.1.15` | `rack-3` | `region-eu-1` | Nodes 17, 18, 19, 20 | 4 |

### Combinatorial Durability Audit:
- Total combinations of 2 hosts failing: $\binom{5}{2} = 10$ pairs.
- For every pair $\{h_i, h_j\}$, shards lost = $4 + 4 = 8$.
- Surviving shards = $20 - 8 = 12 = k$.
- Durability probability under arbitrary 2-host loss = **100.0%**.
