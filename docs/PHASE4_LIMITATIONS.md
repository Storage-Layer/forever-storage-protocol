# Phase 4 Limitations
- Physical multi-host test: **NOT EXECUTED**; all results are single-host loopback. Deployment files in `deploy/` are untested examples (the TOML configs are not read by any code; `fsp-node` takes CLI flags).
- Not production ready. Coordinator SPOF, no consensus, no Byzantine tolerance, no DoS protection.
- Scrub challenge not independently verified; generation watermark not independently persisted; node idempotency cache is in memory.
- Coordinator buffers whole objects on PUT and per-shard payloads (32 MiB frame cap bounds object size to roughly 12 x 32 MiB per stripe set).
- Repair inspects stripe 0 only and uses a heuristic detector mapping; repair pass is sequential; limited by spare nodes.
- LRC(20,12,4) has 1 singular 6-erasure pattern over GF(2^8) (audited); `can_decode` guards it but the code is not optimized to remove it.
- Benchmarks are single-sample; no confidence intervals.
- Phase 3 reported benchmark figures were not re-verified in this phase.
