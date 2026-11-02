#!/usr/bin/env python3
"""
FSP Independent Test Reconciliation Tool
Authoritative parser and validator for Cargo test execution logs and inventory.
Derives canonical results directly from raw execution evidence with zero hardcoding.
"""

import argparse
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Dict, List, Optional


class ReconciliationError(Exception):
    """Raised when test execution evidence is malformed, incomplete, or corrupted."""
    pass


class TestReconciler:
    def __init__(self):
        self.target_pattern = re.compile(r"^\s*Running\s+(.+?)\s+\((.+?)\)")
        self.result_pattern = re.compile(
            r"^test result:\s+(ok|FAILED)\.\s+(\d+)\s+passed;\s+(\d+)\s+failed;\s+(\d+)\s+ignored;\s+(\d+)\s+measured;\s+(\d+)\s+filtered out;\s+finished in\s+([\d\.]+)s"
        )
        self.test_line_pattern = re.compile(r"^([a-zA-Z0-9_:]+):\s+test")

    def parse_inventory(self, inventory_content: str) -> dict:
        """Parses the output of `cargo test -- --list` to extract all discovered tests."""
        discovered_targets = {}
        current_target = None
        current_binary = None

        for line in inventory_content.splitlines():
            t_match = self.target_pattern.match(line)
            if t_match:
                current_target = t_match.group(1).strip()
                current_binary = t_match.group(2).strip()
                if current_target in discovered_targets:
                    raise ReconciliationError(f"Duplicate target identity in inventory: {current_target}")
                discovered_targets[current_target] = {
                    "binary": current_binary,
                    "tests": []
                }
                continue

            test_match = self.test_line_pattern.match(line.strip())
            if test_match and current_target:
                discovered_targets[current_target]["tests"].append(test_match.group(1))

        total_discovered = sum(len(v["tests"]) for v in discovered_targets.values())
        return {
            "total_discovered_targets": len(discovered_targets),
            "total_discovered_tests": total_discovered,
            "discovered_targets": discovered_targets
        }

    def parse_execution_log(self, log_content: str, exit_code: int = 0, wall_clock_duration: float = None) -> dict:
        """Parses the output of `cargo test` and reconciles all execution metrics."""
        if exit_code != 0:
            raise ReconciliationError(f"Cargo test command failed with non-zero exit code: {exit_code}")

        targets = []
        seen_targets = set()
        current_target = None
        current_binary = None
        pending_target = False

        for line in log_content.splitlines():
            t_match = self.target_pattern.match(line)
            if t_match:
                if pending_target:
                    raise ReconciliationError(f"Target '{current_target}' started running but produced no summary before next target")
                current_target = t_match.group(1).strip()
                current_binary = t_match.group(2).strip()
                if current_target in seen_targets:
                    raise ReconciliationError(f"Duplicate test target executed: {current_target}")
                seen_targets.add(current_target)
                pending_target = True
                continue

            r_match = self.result_pattern.match(line.strip())
            if r_match:
                if not pending_target or not current_target:
                    raise ReconciliationError(f"Found orphaned test result summary without preceding target: {line}")

                status = r_match.group(1)
                passed = int(r_match.group(2))
                failed = int(r_match.group(3))
                ignored = int(r_match.group(4))
                measured = int(r_match.group(5))
                filtered = int(r_match.group(6))
                duration = float(r_match.group(7))

                if status == "FAILED" or failed > 0:
                    raise ReconciliationError(f"Target '{current_target}' contains failed tests: {failed} failed")

                targets.append({
                    "target": current_target,
                    "binary": current_binary,
                    "status": status,
                    "passed": passed,
                    "failed": failed,
                    "ignored": ignored,
                    "measured": measured,
                    "filtered": filtered,
                    "duration_seconds": duration,
                    "is_zero_test_target": (passed + failed + ignored == 0)
                })

                pending_target = False
                current_target = None
                current_binary = None

        if pending_target:
            raise ReconciliationError(f"Truncated test log: Target '{current_target}' never completed")

        if not targets:
            raise ReconciliationError("Execution log contains no test targets")

        total_passed = sum(t["passed"] for t in targets)
        total_failed = sum(t["failed"] for t in targets)
        total_ignored = sum(t["ignored"] for t in targets)
        total_measured = sum(t["measured"] for t in targets)
        total_filtered = sum(t["filtered"] for t in targets)
        total_targets = len(targets)
        active_targets = sum(1 for t in targets if not t["is_zero_test_target"])
        zero_test_targets = sum(1 for t in targets if t["is_zero_test_target"])
        total_target_duration = sum(t["duration_seconds"] for t in targets)

        return {
            "total_targets_executed": total_targets,
            "active_test_suites": active_targets,
            "zero_test_binaries": zero_test_targets,
            "total_executed_tests": total_passed + total_failed,
            "total_passed": total_passed,
            "total_failed": total_failed,
            "total_ignored": total_ignored,
            "total_measured": total_measured,
            "total_filtered": total_filtered,
            "total_target_duration_seconds": round(total_target_duration, 4),
            "command_wall_clock_duration_seconds": wall_clock_duration,
            "cargo_exit_status": exit_code,
            "targets": targets
        }

    def reconcile(self, inventory_content: str, execution_content: str, exit_code: int = 0, wall_clock_duration: float = None) -> dict:
        """Fully reconciles inventory against execution."""
        inv = self.parse_inventory(inventory_content)
        res = self.parse_execution_log(execution_content, exit_code=exit_code, wall_clock_duration=wall_clock_duration)

        # Cross-validation: Discovered vs Executed
        if inv["total_discovered_targets"] != res["total_targets_executed"]:
            raise ReconciliationError(
                f"Target count mismatch: Discovered {inv['total_discovered_targets']} in inventory, "
                f"but executed {res['total_targets_executed']} in test run"
            )

        if inv["total_discovered_tests"] != res["total_passed"]:
            raise ReconciliationError(
                f"Test count mismatch: Discovered {inv['total_discovered_tests']} in inventory, "
                f"but executed/passed {res['total_passed']} in test run"
            )

        # Check each target matches exactly
        inv_targets = inv["discovered_targets"]
        target_diffs = []
        for t in res["targets"]:
            name = t["target"]
            if name not in inv_targets:
                raise ReconciliationError(f"Target '{name}' executed but was not in inventory")
            disc_count = len(inv_targets[name]["tests"])
            if disc_count != t["passed"]:
                raise ReconciliationError(
                    f"Target '{name}' test count mismatch: {disc_count} in inventory, {t['passed']} passed"
                )
            target_diffs.append({
                "target": name,
                "discovered": disc_count,
                "executed": t["passed"],
                "passed": t["passed"],
                "failed": t["failed"],
                "ignored": t["ignored"],
                "duration_seconds": t["duration_seconds"],
                "status": "EXACT_PARITY"
            })

        return {
            "reconciliation_status": "VERIFIED_EXACT_MATCH",
            "inventory_summary": inv,
            "execution_summary": res,
            "target_reconciliation_details": target_diffs
        }


def generate_markdown_table_from_canonical(canonical_data: dict) -> str:
    """Generates the Markdown test inventory table directly from canonical JSON."""
    targets = canonical_data["execution_summary"]["targets"]
    lines = []
    lines.append("| Target Name | Passed | Failed | Ignored | Target Duration (s) | Classification |")
    lines.append("| :--- | :---: | :---: | :---: | :---: | :--- |")

    for t in targets:
        name = t["target"]
        passed = t["passed"]
        failed = t["failed"]
        ignored = t["ignored"]
        dur = t["duration_seconds"]

        if name == "unittests src/lib.rs":
            cls = "Library unit tests (in-memory)"
        elif name == "unittests src/bin/fsp_node.rs":
            cls = "Storage node daemon CLI entrypoint (0 tests)"
        elif name == "unittests src/main.rs":
            cls = "Simulation CLI entrypoint (0 tests)"
        elif "phase5" in name:
            cls = "Phase 5 distributed / persistence integration suite"
        elif "phase4" in name:
            cls = "Phase 4 mTLS / repair integration suite"
        elif "phase" in name:
            cls = f"Phase {name.split('phase')[1][0]} baseline integration suite"
        else:
            cls = "Core protocol integration suite"

        lines.append(f"| `{name}` | {passed} | {failed} | {ignored} | {dur:.2f} | {cls} |")

    total_passed = canonical_data["execution_summary"]["total_passed"]
    total_failed = canonical_data["execution_summary"]["total_failed"]
    total_ignored = canonical_data["execution_summary"]["total_ignored"]
    total_dur = canonical_data["execution_summary"]["total_target_duration_seconds"]

    lines.append(f"| **TOTAL** | **{total_passed}** | **{total_failed}** | **{total_ignored}** | **{total_dur:.2f} s** | **Canonical empirical count across {len(targets)} targets** |")
    return "\n".join(lines)


def run_self_tests():
    """Runs a comprehensive suite of unit tests on the reconciler using controlled test fixtures."""
    reconciler = TestReconciler()

    good_log = (
        "   Compiling test v0.1.0\n"
        "     Running unittests src/lib.rs (target/debug/deps/lib-123)\n"
        "running 2 tests\ntest a ... ok\ntest b ... ok\n"
        "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s\n"
        "     Running tests/test_foo.rs (target/debug/deps/foo-123)\n"
        "running 1 tests\ntest c ... ok\n"
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s\n"
    )
    parsed = reconciler.parse_execution_log(good_log, 0, 0.20)
    assert parsed["total_passed"] == 3
    assert parsed["total_targets_executed"] == 2
    assert parsed["active_test_suites"] == 2
    assert parsed["zero_test_binaries"] == 0
    assert abs(parsed["total_target_duration_seconds"] - 0.15) < 1e-4

    try:
        reconciler.parse_execution_log(good_log, 101)
        assert False, "Should have raised ReconciliationError on non-zero exit code"
    except ReconciliationError:
        pass

    fail_log = good_log + (
        "     Running tests/test_bar.rs (target/debug/deps/bar-123)\n"
        "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s\n"
    )
    try:
        reconciler.parse_execution_log(fail_log, 0)
        assert False, "Should have rejected failed test in log"
    except ReconciliationError:
        pass

    trunc_log = good_log + "     Running tests/test_trunc.rs (target/debug/deps/trunc-123)\nrunning 5 tests\n"
    try:
        reconciler.parse_execution_log(trunc_log, 0)
        assert False, "Should have rejected truncated log"
    except ReconciliationError:
        pass

    dup_log = good_log + (
        "     Running tests/test_foo.rs (target/debug/deps/foo-123)\n"
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s\n"
    )
    try:
        reconciler.parse_execution_log(dup_log, 0)
        assert False, "Should have rejected duplicate target"
    except ReconciliationError:
        pass

    zero_log = (
        "     Running unittests src/main.rs (target/debug/deps/main-123)\n"
        "running 0 tests\n"
        "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
    )
    p_zero = reconciler.parse_execution_log(zero_log, 0)
    assert p_zero["total_passed"] == 0
    assert p_zero["zero_test_binaries"] == 1
    assert p_zero["active_test_suites"] == 0

    print("[PASS] TestReconciler fixture self-tests passed (6/6 scenarios verified).")


def main():
    parser = argparse.ArgumentParser(description="FSP Test Reconciler")
    parser.add_argument("--inventory", type=str, help="Path to test_inventory.log")
    parser.add_argument("--run", type=str, help="Path to full_test_run.log")
    parser.add_argument("--environment", type=str, help="Path to environment.json")
    parser.add_argument("--output-canonical", type=str, help="Path to canonical_test_results.json")
    parser.add_argument("--output-reconciliation", type=str, help="Path to target_reconciliation.json")
    parser.add_argument("--generate-markdown", action="store_true", help="Print canonical markdown table")
    parser.add_argument("--self-test", action="store_true", help="Run reconciler unit tests")
    args = parser.parse_args()

    if args.self_test:
        run_self_tests()
        sys.exit(0)

    if not args.inventory or not args.run:
        parser.print_help()
        sys.exit(1)

    inv_path = Path(args.inventory)
    run_path = Path(args.run)

    if not inv_path.exists():
        print(f"Error: Inventory log not found: {inv_path}", file=sys.stderr)
        sys.exit(1)
    if not run_path.exists():
        print(f"Error: Full run log not found: {run_path}", file=sys.stderr)
        sys.exit(1)

    with open(inv_path, "r", encoding="utf-8") as f:
        inv_content = f.read()
    with open(run_path, "r", encoding="utf-8") as f:
        run_content = f.read()

    exit_code = 0
    wall_duration = None
    if args.environment:
        env_path = Path(args.environment)
        if env_path.exists():
            with open(env_path, "r", encoding="utf-8") as f:
                env_data = json.load(f)
            exit_code = env_data.get("full_run", {}).get("exit_status", 0)
            wall_duration = env_data.get("full_run", {}).get("duration_seconds")

    reconciler = TestReconciler()
    try:
        res = reconciler.reconcile(inv_content, run_content, exit_code=exit_code, wall_clock_duration=wall_duration)
    except ReconciliationError as e:
        print(f"[FAIL] Reconciliation error: {e}", file=sys.stderr)
        sys.exit(2)

    if args.output_canonical:
        p = Path(args.output_canonical)
        p.parent.mkdir(parents=True, exist_ok=True)
        with open(p, "w", encoding="utf-8") as f:
            json.dump(res, f, indent=2)
        print(f"[SUCCESS] Wrote canonical JSON: {p}")

    if args.output_reconciliation:
        p = Path(args.output_reconciliation)
        p.parent.mkdir(parents=True, exist_ok=True)
        diff_data = {
            "reconciliation_status": res["reconciliation_status"],
            "total_targets": res["execution_summary"]["total_targets_executed"],
            "total_tests": res["execution_summary"]["total_passed"],
            "active_suites": res["execution_summary"]["active_test_suites"],
            "zero_test_binaries": res["execution_summary"]["zero_test_binaries"],
            "targets": res["target_reconciliation_details"]
        }
        with open(p, "w", encoding="utf-8") as f:
            json.dump(diff_data, f, indent=2)
        print(f"[SUCCESS] Wrote target reconciliation JSON: {p}")

    if args.generate_markdown:
        print(generate_markdown_table_from_canonical(res))

    print(f"[PASS] Reconciled {res['execution_summary']['total_targets_executed']} targets, {res['execution_summary']['total_passed']} passed tests.")
    sys.exit(0)


if __name__ == "__main__":
    main()
