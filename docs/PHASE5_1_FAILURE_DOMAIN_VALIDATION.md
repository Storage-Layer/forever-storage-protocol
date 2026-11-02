# FSP Phase 5.1 — Failure Domain Topology Validation

## 1. Overview
Storage systems must prevent co-location vulnerabilities where too many erasure-coded shards reside on the same physical server, power rail, or rack switch.
Phase 5.1 enforces failure domain anti-affinity at configuration load time and placement planning time.

## 2. Preflight Topology Rules Enforced
Implemented in [src/config/validation.rs](file:///home/rade/Project/Jobs/fsp/src/config/validation.rs) and [src/coordinator/placement.rs](file:///home/rade/Project/Jobs/fsp/src/coordinator/placement.rs):
1. **Host Anti-Affinity:** $\le 4$ shards per host for RS(20,12).
2. **Rack Anti-Affinity:** $\le 8$ shards per rack.
3. **Sufficient Failure Domains:** The cluster must configure at least 5 distinct physical hosts. If fewer than 5 hosts are present, preflight validation rejects the topology with `ConfigValidationError::InvalidValue`.
4. **Co-location Detection:** If multiple nodes share the same IP/listen port or exceed host domain ceilings, preflight fails closed.

## 3. Mathematical Proof of 2-Host Survival
Let $n = 20$ be the total coded shards, $k = 12$ be the minimum required shards for decoding, and $m = 8$ be the maximum tolerable erasures.
Let $H = 5$ be the number of independent physical host domains, and $c_H = 4$ be the maximum shards placed per host domain.
For any two host failures $H_a, H_b$:
$$\text{Shards Lost} \le c_{H_a} + c_{H_b} = 4 + 4 = 8$$
$$\text{Surviving Shards} = n - \text{Shards Lost} \ge 20 - 8 = 12$$
Since $\text{Surviving Shards} \ge k = 12$, any arbitrary 2-host concurrent hardware failure is 100% recoverable without data loss.

## 4. Test Verification
- [tests/phase5_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_validation.rs) (`test_p5_07`): Rejects 4-host topology violating failure domain constraints.
- [tests/phase5_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_validation.rs) (`test_p5_08`): Approves valid 5-host 20-node topology.
- [tests/phase5_validation.rs](file:///home/rade/Project/Jobs/fsp/tests/phase5_validation.rs) (`test_p5_12`): Physically deletes all 8 shards on host-01 and host-02 and verifies 100% Cauchy RS(20,12) degraded reconstruction.
