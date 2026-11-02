# Phase 4 Benchmark Results
Single run (n=1, no variance estimate), one host, loopback, RS(20,12), 1 MiB stripes, debug-free release build, `./target/release/fsp-sim --phase4`. Raw: [benchmark_results.json](file:///home/rade/Project/Jobs/fsp/results/phase4/benchmark_results.json).

| Mode | Size | PUT MiB/s | GET MiB/s | Files |
|---|---|---|---|---|
| local v1 | 100 MiB | 6.71 | 27.28 | 2000 |
| local v2 | 100 MiB | 6.50 | 33.71 | 20 |
| local v1 | 1 MiB | 4.80 | 27.20 | 20 |
| local v2 | 1 MiB | 4.14 | 20.17 | 20 |
| 20-node mTLS | 1 MiB | 10.42 | 12.77 | - |
| 20-node mTLS | 100 MiB | 14.96 | 11.65 | - |

Reading: v2 cuts file count 100x for large objects with similar PUT speed; GET gain at 100 MiB (+24%) is a single noisy sample and the 1 MiB GET was slower, so no speed claim is made. Distributed numbers are loopback only and exclude real network latency; they are not comparable to the local numbers (different code path, parallel PutShard). 1 KiB figures are dominated by fixed costs. Network bytes for 100 MiB: 174,877,120 (~1.67x, matching n/k plus headers). Degraded GET with 8 nodes down (1 MiB): 637 ms, correct.
