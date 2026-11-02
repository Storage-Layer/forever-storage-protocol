#!/usr/bin/env python3
"""FSP Phase 5.7 Micro-Benchmark Runner and Statistical Analyzer.

Enforces binary mebibytes (1 MiB = 1,048,576 B; MiB/s).
Orchestrates cargo release execution of the benchmark suite or parses and validates
existing benchmark trial outputs in results/phase5_7/benchmark_trials.json.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
RESULTS_DIR = REPO_ROOT / "results" / "phase5_7"

def run_benchmarks():
    print("=" * 80)
    print("Executing FSP Phase 5.7 Multi-Trial Micro-Benchmarks")
    print("=" * 80)
    cmd = ["cargo", "run", "--release", "--", "--phase5-bench"]
    res = subprocess.run(cmd, cwd=REPO_ROOT)
    if res.returncode != 0:
        print(f"Benchmark execution failed with code {res.returncode}", file=sys.stderr)
        sys.exit(res.returncode)

def audit_results():
    trials_path = RESULTS_DIR / "benchmark_trials.json"
    prov_path = RESULTS_DIR / "benchmark_provenance.json"

    if not trials_path.exists():
        print(f"Error: {trials_path} does not exist. Run benchmarks first.", file=sys.stderr)
        sys.exit(1)

    with open(trials_path) as f:
        trials_data = json.load(f)

    with open(prov_path) as f:
        prov_data = json.load(f)

    print("\nVerified Phase 5.7 Benchmark Results:")
    print(f"Units enforced: {trials_data.get('units_enforced')}")
    print(f"Trials per workload: {trials_data.get('trials_per_workload')}")
    print("-" * 80)
    print(f"{'Workload':<45} | {'Median Dur':<12} | {'Median MiB/s':<14} | {'StdDev MiB/s'}")
    print("-" * 80)
    for s in prov_data.get("summary", []):
        name = s.get("workload")[:43]
        dur = f"{s.get('median_duration_s'):.4f} s"
        mib_s = f"{s.get('median_mib_s'):.2f} MiB/s"
        std = f"±{s.get('stddev_mib_s'):.2f}"
        print(f"{name:<45} | {dur:<12} | {mib_s:<14} | {std}")
    print("-" * 80)

if __name__ == "__main__":
    if "--audit-only" in sys.argv:
        audit_results()
    else:
        run_benchmarks()
        audit_results()
