#!/usr/bin/env python3
"""
Captures an authoritative Cargo test inventory and execution run.
Records exact environment, start/end timestamps, wall-clock durations,
exit statuses, and SHA-256 digests of all raw output logs.
"""

import datetime
import hashlib
import json
import os
import platform
import subprocess
import sys
import time
from pathlib import Path

def compute_sha256(filepath: Path) -> str:
    h = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            h.update(chunk)
    return h.hexdigest()

def main():
    repo_root = Path(__file__).resolve().parent.parent
    run_id = sys.argv[1] if len(sys.argv) > 1 else f"run_{datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%d_%H%M%S')}"
    out_dir = repo_root / "results" / "phase5_7" / "verification_hotfix_v2" / run_id
    out_dir.mkdir(parents=True, exist_ok=True)

    print(f"[*] Starting authoritative run in: {out_dir}")

    # Inspect git
    git_rev = "NOT_A_GIT_REPOSITORY"
    git_dirty = False
    try:
        git_rev = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=repo_root, stderr=subprocess.DEVNULL, text=True).strip()
        diff_out = subprocess.check_output(["git", "status", "--porcelain"], cwd=repo_root, stderr=subprocess.DEVNULL, text=True).strip()
        git_dirty = len(diff_out) > 0
    except Exception:
        pass

    rustc_ver = subprocess.check_output(["rustc", "--version"], cwd=repo_root, text=True).strip()
    cargo_ver = subprocess.check_output(["cargo", "--version"], cwd=repo_root, text=True).strip()
    kernel_ver = platform.release()

    # Step 1: Capture test inventory
    inv_log_path = out_dir / "test_inventory.log"
    inv_cmd = ["cargo", "test", "--workspace", "--all-targets", "--", "--list"]
    print(f"[*] Running inventory: {' '.join(inv_cmd)}")
    inv_start_wall = datetime.datetime.now(datetime.timezone.utc).isoformat()
    t0 = time.perf_counter()
    with open(inv_log_path, "wb") as f_out:
        p_inv = subprocess.run(inv_cmd, cwd=repo_root, stdout=f_out, stderr=subprocess.STDOUT)
    inv_dur = time.perf_counter() - t0
    inv_end_wall = datetime.datetime.now(datetime.timezone.utc).isoformat()
    inv_exit = p_inv.returncode
    inv_sha256 = compute_sha256(inv_log_path)
    print(f"[*] Inventory complete: exit={inv_exit}, duration={inv_dur:.2f}s, sha256={inv_sha256[:16]}...")

    if inv_exit != 0:
        print(f"[!] Error: Inventory command failed with exit {inv_exit}", file=sys.stderr)
        sys.exit(inv_exit)

    # Step 2: Capture full test run
    run_log_path = out_dir / "full_test_run.log"
    run_cmd = ["cargo", "test", "--workspace", "--all-targets", "--", "--test-threads=1"]
    print(f"[*] Running execution: {' '.join(run_cmd)}")
    run_start_wall = datetime.datetime.now(datetime.timezone.utc).isoformat()
    t0 = time.perf_counter()
    with open(run_log_path, "wb") as f_out:
        p_run = subprocess.run(run_cmd, cwd=repo_root, stdout=f_out, stderr=subprocess.STDOUT)
    run_dur = time.perf_counter() - t0
    run_end_wall = datetime.datetime.now(datetime.timezone.utc).isoformat()
    run_exit = p_run.returncode
    run_sha256 = compute_sha256(run_log_path)
    print(f"[*] Full run complete: exit={run_exit}, duration={run_dur:.2f}s, sha256={run_sha256[:16]}...")

    # Record environment and provenance
    env_data = {
        "run_id": run_id,
        "repo_root": str(repo_root),
        "git_revision": git_rev,
        "git_dirty": git_dirty,
        "rustc_version": rustc_ver,
        "cargo_version": cargo_ver,
        "kernel_release": kernel_ver,
        "platform": platform.platform(),
        "cpu_count": os.cpu_count(),
        "doctests_included_in_scope": False, # Cargo test --all-targets executes lib, bin, and integration tests; doctests require --doc
        "inventory": {
            "command": " ".join(inv_cmd),
            "start_time_utc": inv_start_wall,
            "end_time_utc": inv_end_wall,
            "duration_seconds": inv_dur,
            "exit_status": inv_exit,
            "log_file": str(inv_log_path.relative_to(repo_root)),
            "log_sha256": inv_sha256
        },
        "full_run": {
            "command": " ".join(run_cmd),
            "start_time_utc": run_start_wall,
            "end_time_utc": run_end_wall,
            "duration_seconds": run_dur,
            "exit_status": run_exit,
            "log_file": str(run_log_path.relative_to(repo_root)),
            "log_sha256": run_sha256
        }
    }

    env_path = out_dir / "environment.json"
    with open(env_path, "w", encoding="utf-8") as f:
        json.dump(env_data, f, indent=2)

    print(f"[*] Environment recorded in: {env_path}")
    if run_exit != 0:
        sys.exit(run_exit)

if __name__ == "__main__":
    main()
