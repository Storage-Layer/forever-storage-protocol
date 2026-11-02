# FSP Phase 4.2: Physical Failure-Domain Placement and Durability Mathematics

## 1. Mathematical Ground Truth

In Reed-Solomon coding $\text{RS}(n, k)$, an object is partitioned into $k$ data shards and expanded to $n$ total shards with $m = n - k$ parity shards. Reconstruction requires any $k$ surviving shards. Up to $m$ arbitrary shard losses can be tolerated.

### 1.1 Single Physical Host Loss Tolerance ($F = 1$)
To survive the total destruction or isolation of any single physical server, no physical server $h$ may hold more than $m$ shards of any stripe:
$$\max_{h} \text{shards}(h) \le m = n - k$$

**Theorem (Minimum Host Bound):**
The minimum number of independent physical hosts required to survive $F = 1$ host failure is:
$$H_{\min} = \left\lceil \frac{n}{m} \right\rceil$$

For the default $\text{RS}(20, 12)$ scheme ($n=20, k=12, m=8$):
$$H_{\min} = \left\lceil \frac{20}{8} \right\rceil = 3 \text{ hosts}$$

A valid 3-host distribution is:
- Host A: 7 shards
- Host B: 7 shards
- Host C: 6 shards
Loss of Host A or B leaves $20 - 7 = 13 \ge 12 = k$ shards. Loss of Host C leaves $20 - 6 = 14 \ge 12$ shards.

> [!CAUTION]
> **Mathematical Impossibility with 2 Hosts:**
> Distributing 20 shards across 2 hosts guarantees by the Pigeonhole Principle that at least one host holds $\ge 10$ shards. Because $10 > m = 8$, the loss of that host leaves $\le 10 < 12$ shards, causing permanent data loss. Two hosts cannot provide single-host failure tolerance under RS(20,12).

### 1.2 Double Physical Host Loss Tolerance ($F = 2$)
To survive two simultaneous physical host failures:
$$\forall h_1 \ne h_2: \quad \text{shards}(h_1) + \text{shards}(h_2) \le m$$
Under equal placement, each host holds at most $\lfloor 8 / 2 \rfloor = 4$ shards.
Minimum host count:
$$H_{\min}(F=2) = \left\lceil \frac{20}{4} \right\rceil = 5 \text{ hosts}$$

---

## 2. Hierarchical Failure Domains

The placement engine [`src/placement/failure_domains.rs`](file:///home/rade/Project/Jobs/fsp/src/placement/failure_domains.rs) tracks and verifies hierarchical failure domains:
1. `Host`: Physical hardware chassis.
2. `StorageDevice`: Specific disk or NVMe serial.
3. `Rack`: Top-of-rack power and networking domain.
4. `AvailabilityZone`: Datacenter facility.
5. `Region`: Geographic region.
6. `Operator`: Autonomous cloud / infrastructure provider.

Multiple local daemons on the same physical host sharing a `host_domain` are recognized as a single physical failure domain and strictly prevented from being counted as independent hosts.
