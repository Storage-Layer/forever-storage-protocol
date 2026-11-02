#!/usr/bin/env python3
"""
FSP Evidence Consistency Verification Pipeline
Orchestrates validation of all Phase 5.7 reports against authoritative execution evidence.
Ensures zero tolerance for mathematical contradictions, phantom targets, or mislabeled claims.
"""

import argparse
import json
import os
import sys
from pathlib import Path

# Import EvidenceValidator and generate_markdown_table
from test_evidence_validator import EvidenceValidator
from test_reconciler import generate_markdown_table_from_canonical


def main():
    parser = argparse.ArgumentParser(description="FSP Evidence Consistency Pipeline Validator")
    parser.add_argument("--results-dir", type=str, default="results/phase5_7/verification_hotfix_v2/run_01", help="Path to run results directory")
    parser.add_argument("--canonical-json", type=str, help="Path to canonical JSON (defaults to <results-dir>/canonical_test_results.json)")
    parser.add_argument("--reports", nargs="+", help="Paths to markdown reports to validate")
    parser.add_argument("--generate-table", action="store_true", help="Print canonical markdown test table to stdout")
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent
    results_dir = repo_root / args.results_dir

    canonical_path = Path(args.canonical_json) if args.canonical_json else (results_dir / "canonical_test_results.json")
    if not canonical_path.exists():
        # Fall back to test_reconciliation.json if canonical_test_results.json not yet created
        fallback_path = results_dir / "test_reconciliation.json"
        if fallback_path.exists():
            canonical_path = fallback_path
        else:
            fallback_v1 = repo_root / "results" / "phase5_7" / "verification_hotfix" / "test_reconciliation.json"
            if fallback_v1.exists():
                canonical_path = fallback_v1

    if not canonical_path.exists():
        print(f"[FAIL] Canonical evidence not found: {canonical_path}", file=sys.stderr)
        sys.exit(1)

    with open(canonical_path, "r", encoding="utf-8") as f:
        canonical_data = json.load(f)

    if args.generate_table:
        print(generate_markdown_table_from_canonical(canonical_data))
        sys.exit(0)

    # Determine reports to validate
    default_reports = [
        "docs/PHASE5_7_FINAL_REPORT.md",
        "docs/PHASE5_7_TEST_COUNT_AUDIT.md",
        "docs/PHASE5_7_BENCHMARK_PROVENANCE.md",
        "docs/PHASE5_7_EVIDENCE_CORRECTION_V2.md"
    ]
    report_paths = [Path(r) if Path(r).is_absolute() else (repo_root / r) for r in (args.reports or default_reports)]

    # Load raw execution log and environment metadata if available
    raw_log_content = None
    raw_log_path = results_dir / "full_test_run.log"
    if raw_log_path.exists():
        with open(raw_log_path, "r", encoding="utf-8") as f:
            raw_log_content = f.read()

    env_data = None
    env_path = results_dir / "environment.json"
    if env_path.exists():
        with open(env_path, "r", encoding="utf-8") as f:
            env_data = json.load(f)

    validator = EvidenceValidator(repo_root=repo_root)
    all_ok = True
    validation_summary = {}

    for rep in report_paths:
        if not rep.exists():
            # If report does not exist yet (e.g. before generation), report failure if explicit or note
            print(f"[WARN] Report file does not exist yet: {rep.relative_to(repo_root) if rep.is_relative_to(repo_root) else rep}", file=sys.stderr)
            all_ok = False
            validation_summary[rep.name] = {"status": "FILE_NOT_FOUND"}
            continue

        with open(rep, "r", encoding="utf-8") as f:
            rep_text = f.read()

        violations = validator.validate_rules(
            rep_text,
            canonical_data,
            raw_cargo_log_content=raw_log_content,
            environment_meta=env_data
        )

        if violations:
            all_ok = False
            validation_summary[rep.name] = {
                "status": "FAILED",
                "violations": violations
            }
            print(f"[FAIL] Report validation FAILED: {rep.name}", file=sys.stderr)
            for v in violations:
                print(f"  - {v}", file=sys.stderr)
        else:
            validation_summary[rep.name] = {
                "status": "PASSED",
                "violations": []
            }
            print(f"[PASS] Report validation PASSED: {rep.name}")

    # Write summary to results_dir if exists
    if results_dir.exists():
        out_sum = results_dir / "report_consistency.json"
        with open(out_sum, "w", encoding="utf-8") as f:
            json.dump({
                "overall_status": "PASSED" if all_ok else "FAILED",
                "reports": validation_summary
            }, f, indent=2)

    if not all_ok:
        sys.exit(2)

    print("[SUCCESS] All evaluated reports passed evidence consistency validation.")
    sys.exit(0)


if __name__ == "__main__":
    main()
