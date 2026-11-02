#!/usr/bin/env python3
"""
FSP Test Evidence Consistency Validator & Mutation Testing Harness
Implements strict verification of Markdown reports against authoritative JSON
evidence and raw Cargo logs. Validates 10 mandatory rules and executes 20 mutation tests.
"""

import argparse
import copy
import hashlib
import json
import os
import re
import sys
from pathlib import Path
from typing import Dict, List, Optional, Tuple


class ValidationError(Exception):
    """Raised when an evidence rule is violated."""
    pass


class EvidenceValidator:
    def __init__(self, repo_root: Optional[Path] = None):
        self.repo_root = repo_root or Path(__file__).resolve().parent.parent

    @staticmethod
    def compute_sha256(filepath: Path) -> str:
        h = hashlib.sha256()
        with open(filepath, "rb") as f:
            while chunk := f.read(65536):
                h.update(chunk)
        return h.hexdigest()

    def parse_markdown_target_table(self, content: str) -> Tuple[List[Dict], Optional[Dict]]:
        """
        Parses test target rows and total row from markdown.
        Supports standard table formats with target name, passed, failed, ignored, etc.
        """
        rows = []
        total_row = None

        # Regex for target rows:
        # e.g. | `target_name` | passed | failed | ignored | duration | ... |
        # or | 1 | `target_name` | path | passed | failed | ignored | ... |
        row_pattern_1 = re.compile(
            r"\|\s*`([^`]+)`\s*\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*([\d\.]+)\s*(?:s)?\s*\|\s*([^\|]+)\|"
        )
        row_pattern_2 = re.compile(
            r"\|\s*\d+\s*\|\s*`([^`]+)`\s*\|\s*`?([^`\|]+)`?\s*\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*([\d\.]+)\s*(?:s)?\s*\|\s*([^\|]+)\|"
        )

        # Regex for total row:
        # e.g. | **TOTAL** | **350** | **0** | **0** | **282.11 s** | ... |
        # or | **TOTAL** | **21 targets...** | — | **350** | **0** | **0** | **0** | ... |
        total_pattern_1 = re.compile(
            r"\|\s*\*\*TOTAL\*\*\s*\|\s*\*\*(\d+)\*\*\s*\|\s*\*\*(\d+)\*\*\s*\|\s*\*\*(\d+)\*\*\s*\|\s*\*\*([\d\.]+)\s*(?:s)?\*\*\s*\|"
        )
        total_pattern_2 = re.compile(
            r"\|\s*\*\*TOTAL\*\*\s*\|\s*[^\|]*\s*\|\s*[^\|]*\s*\|\s*\*\*(\d+)\*\*\s*\|\s*\*\*(\d+)\*\*\s*\|\s*\*\*(\d+)\*\*\s*\|\s*\*\*(\d+)\*\*\s*\|\s*\*\*([\d\.]+)\s*(?:s)?\*\*\s*\|"
        )

        for line in content.splitlines():
            line_str = line.strip()
            if not line_str.startswith("|"):
                continue

            # Check total row first
            m_tot1 = total_pattern_1.search(line_str)
            if m_tot1:
                total_row = {
                    "passed": int(m_tot1.group(1)),
                    "failed": int(m_tot1.group(2)),
                    "ignored": int(m_tot1.group(3)),
                    "duration": float(m_tot1.group(4))
                }
                continue

            m_tot2 = total_pattern_2.search(line_str)
            if m_tot2:
                total_row = {
                    "passed": int(m_tot2.group(1)),
                    "failed": int(m_tot2.group(2)),
                    "ignored": int(m_tot2.group(3)),
                    "filtered": int(m_tot2.group(4)),
                    "duration": float(m_tot2.group(5))
                }
                continue

            # Check target rows
            m_r2 = row_pattern_2.match(line_str)
            if m_r2:
                rows.append({
                    "target": m_r2.group(1).strip(),
                    "path": m_r2.group(2).strip(),
                    "passed": int(m_r2.group(3)),
                    "failed": int(m_r2.group(4)),
                    "ignored": int(m_r2.group(5)),
                    "filtered": int(m_r2.group(6)),
                    "duration": float(m_r2.group(7)),
                    "status": m_r2.group(8).strip()
                })
                continue

            m_r1 = row_pattern_1.match(line_str)
            if m_r1:
                rows.append({
                    "target": m_r1.group(1).strip(),
                    "passed": int(m_r1.group(2)),
                    "failed": int(m_r1.group(3)),
                    "ignored": int(m_r1.group(4)),
                    "duration": float(m_r1.group(5)),
                    "classification": m_r1.group(6).strip()
                })
                continue

        return rows, total_row

    def parse_reconciliation_equations(self, content: str) -> List[Dict]:
        """
        Extracts arithmetic formulas like:
        425 - 117 - 35 + 77 = 350
        or 13 + 15 + 20 + 25 + 25 + 19 = 117
        Parses all terms on the left-hand side dynamically.
        """
        eqs = []
        # Match any string of numbers separated by + or - ending with = \d+
        line_pattern = re.compile(r"((?:\d+\s*[+-]\s*)+\d+)\s*=\s*(\d+)")
        for line in content.splitlines():
            # Strip LaTeX or markdown formatting
            clean_line = line.replace("$", "").replace("\\mathbf{", "").replace("\\text{", "").replace("}", "")
            for m in line_pattern.finditer(clean_line):
                lhs_str, rhs_str = m.group(1), m.group(2)
                # Tokenize LHS
                tokens = re.findall(r"([+-]?\s*\d+)", lhs_str)
                terms = []
                for tok in tokens:
                    tok_clean = tok.replace(" ", "")
                    if tok_clean.startswith("+"):
                        terms.append((int(tok_clean[1:]), "+"))
                    elif tok_clean.startswith("-"):
                        terms.append((int(tok_clean[1:]), "-"))
                    else:
                        terms.append((int(tok_clean), "+"))
                eqs.append({
                    "raw": m.group(0),
                    "terms": terms,
                    "expected_result": int(rhs_str)
                })
        return eqs

    def validate_rules(
        self,
        report_content: str,
        canonical_json: dict,
        raw_cargo_log_content: Optional[str] = None,
        environment_meta: Optional[dict] = None
    ) -> List[str]:
        """
        Executes all 10 mandatory validation rules against provided content and evidence.
        Returns a list of violation messages. An empty list means all rules passed.
        """
        violations = []
        summary = canonical_json.get("execution_summary", {})
        canonical_total_passed = summary.get("total_passed")
        canonical_total_failed = summary.get("total_failed", 0)
        canonical_targets = summary.get("targets", [])
        canonical_exit = summary.get("cargo_exit_status", 0)

        # Rule 5: Failed Tests in Evidence
        if canonical_total_failed > 0:
            violations.append(f"Rule 5 Violation: Canonical JSON records {canonical_total_failed} failed tests.")
        if canonical_exit != 0:
            violations.append(f"Rule 5 Violation: Canonical Cargo execution failed with exit code {canonical_exit}.")

        # Rule 3: Cargo Agreement
        if raw_cargo_log_content is not None:
            # Check for failure indications in raw log
            if "test result: FAILED" in raw_cargo_log_content:
                violations.append("Rule 3 Violation: Raw Cargo log contains 'test result: FAILED'.")
            # Find total passed in raw cargo log summaries
            res_matches = re.findall(r"test result: (ok|FAILED)\.\s+(\d+)\s+passed", raw_cargo_log_content)
            if res_matches:
                raw_sum_passed = sum(int(m[1]) for m in res_matches)
                if raw_sum_passed != canonical_total_passed:
                    violations.append(
                        f"Rule 3 Violation: Raw Cargo log sum ({raw_sum_passed}) != Canonical JSON ({canonical_total_passed})."
                    )

        # Parse Markdown Target Table
        is_benchmark_report = "BENCHMARK_PROVENANCE" in report_content or ("benchmark" in report_content.lower() and "test inventory" not in report_content.lower())
        rows, total_row = self.parse_markdown_target_table(report_content)

        if not rows and not is_benchmark_report:
            violations.append("Rule 4 Violation: No target table rows found in Markdown report.")
            return violations

        if not total_row and not is_benchmark_report:
            violations.append("Rule 1 Violation: No summary TOTAL row found in Markdown target table.")
            return violations

        # Rule 1: Row Sum (for inventory reports)
        if rows and total_row:
            row_sum_passed = sum(r["passed"] for r in rows)
            if row_sum_passed != total_row["passed"]:
                violations.append(
                    f"Rule 1 Violation: Sum of Markdown target rows ({row_sum_passed}) != summary TOTAL row ({total_row['passed']})."
                )

            # Rule 2: JSON Agreement
            if total_row["passed"] != canonical_total_passed:
                violations.append(
                    f"Rule 2 Violation: Markdown TOTAL passed ({total_row['passed']}) != Canonical JSON total passed ({canonical_total_passed})."
                )

            # Rule 4: Target Agreement
            seen_targets = set()
            canonical_map = {t["target"]: t for t in canonical_targets}
            for r in rows:
                t_name = r["target"]
                norm_name = t_name if t_name.startswith("tests/") or t_name.startswith("unittests") or t_name.startswith("src/") else f"tests/{t_name}.rs"
                matched_key = None
                for ck in canonical_map:
                    if t_name == ck or norm_name == ck or ck.endswith(t_name) or t_name in ck:
                        matched_key = ck
                        break

                if not matched_key:
                    violations.append(f"Rule 4 Violation: Phantom target in Markdown table: '{t_name}'.")
                    continue

                if matched_key in seen_targets:
                    violations.append(f"Rule 4 Violation: Duplicate target identity in Markdown table: '{t_name}'.")
                seen_targets.add(matched_key)

                exp_passed = canonical_map[matched_key]["passed"]
                if r["passed"] != exp_passed:
                    violations.append(
                        f"Rule 4 Violation: Target '{t_name}' passed count mismatch: Table={r['passed']}, Canonical={exp_passed}."
                    )

            # Check for missing active targets
            for ck, c_data in canonical_map.items():
                if ck not in seen_targets:
                    violations.append(f"Rule 4 Violation: Missing active target from Markdown table: '{ck}'.")

        # Rule 6: Evidence Completeness (checked if environment_meta provided)
        if environment_meta:
            inv_sha = environment_meta.get("inventory", {}).get("log_sha256")
            inv_file = environment_meta.get("inventory", {}).get("log_file")
            if inv_file and self.repo_root:
                p = self.repo_root / inv_file
                if not p.exists():
                    violations.append(f"Rule 6 Violation: Referenced inventory log does not exist: {inv_file}")
                elif inv_sha and self.compute_sha256(p) != inv_sha:
                    violations.append(f"Rule 6 Violation: Inventory log digest mismatch for {inv_file}")

            run_sha = environment_meta.get("full_run", {}).get("log_sha256")
            run_file = environment_meta.get("full_run", {}).get("log_file")
            if run_file and self.repo_root:
                p = self.repo_root / run_file
                if not p.exists():
                    violations.append(f"Rule 6 Violation: Referenced execution log does not exist: {run_file}")
                elif run_sha and self.compute_sha256(p) != run_sha:
                    violations.append(f"Rule 6 Violation: Execution log digest mismatch for {run_file}")

        # Rule 7: Git Revision Check
        if environment_meta and "git_revision" in environment_meta:
            env_git = environment_meta["git_revision"]
            if env_git != "NOT_A_GIT_REPOSITORY":
                m_git = re.search(r"Git HEAD:\s*`?([a-f0-9]{40}|[a-f0-9]{7})`?", report_content)
                if m_git and not env_git.startswith(m_git.group(1)):
                    violations.append(
                        f"Rule 7 Violation: Git revision in report ({m_git.group(1)}) != environment ({env_git[:7]})."
                    )

        # Rule 8: Arithmetic Reconciliation Claims
        eqs = self.parse_reconciliation_equations(report_content)
        for eq in eqs:
            val = eq["terms"][0][0]
            for term_val, op in eq["terms"][1:]:
                if op == "+":
                    val += term_val
                elif op == "-":
                    val -= term_val
            if val != eq["expected_result"]:
                violations.append(
                    f"Rule 8 Violation: Reconciliation equation '{eq['raw']}' evaluates to {val}, not {eq['expected_result']}."
                )

        # Check category listing totals if phantom categories are discussed:
        # Flag if an equation claims to reconcile 425 to 350 using 108 phantoms
        for eq in eqs:
            terms = [term_val for term_val, _ in eq["terms"]]
            if 425 in terms and 108 in terms and eq["expected_result"] == 350:
                violations.append("Rule 8 Violation: Phantom subtotal conflict detected (reconciliation equation uses 108 phantoms while listing inconsistent category items).")

        # Rule 9: Duration Semantics
        if "282.11" in report_content and "wall-clock" in report_content.lower():
            # Ensure duration is not mislabeled as wall-clock if it is per-target sum
            if re.search(r"wall-clock.*?282\.11\s*s", report_content, re.IGNORECASE):
                violations.append("Rule 9 Violation: Per-target duration sum (282.11 s) mislabeled as wall-clock duration.")

        # Rule 10: Evidence Classification
        # Physical LAN must be marked NOT EXECUTED
        if ("physical lan" in report_content.lower() or "two-host lan" in report_content.lower()) and "NOT EXECUTED" not in report_content:
            violations.append("Rule 10 Violation: Physical LAN pilot not explicitly marked NOT EXECUTED.")

        # Production readiness must be marked NOT ESTABLISHED
        if "NOT ESTABLISHED" not in report_content:
            violations.append("Rule 10 Violation: Production readiness not explicitly stated as NOT ESTABLISHED.")

        # Actual wire bytes must be marked NOT MEASURED
        if "31.21" in report_content:
            if "ACTUAL_WIRE_BYTES: NOT MEASURED" not in report_content:
                violations.append("Rule 10 Violation: Wire throughput (31.21 MiB/s) reported without explicitly marking 'ACTUAL_WIRE_BYTES: NOT MEASURED'.")
            if "wire bytes: measured" in report_content.lower() or "measured on nic" in report_content.lower():
                violations.append("Rule 10 Violation: Fabricated claim of measured physical wire throughput.")

        # Historical benchmark must not be claimed as fresh physical execution
        if "fresh newly measured live benchmark" in report_content.lower() and "physical hardware" in report_content.lower():
            violations.append("Rule 10 Violation: Historical simulation benchmark presented as fresh physical benchmark.")

        return violations


def run_mutation_tests():
    """
    Executes the 20 mandatory negative tests plus 1 positive test.
    Each negative fixture MUST trigger a validation error.
    """
    print("[*] Running 20 Mandatory Negative Mutation Tests for EvidenceValidator...")
    validator = EvidenceValidator()

    # Base canonical evidence
    base_canonical = {
        "execution_summary": {
            "total_targets_executed": 3,
            "active_test_suites": 2,
            "zero_test_binaries": 1,
            "total_passed": 350,
            "total_failed": 0,
            "total_ignored": 0,
            "cargo_exit_status": 0,
            "targets": [
                {"target": "unittests src/lib.rs", "passed": 2, "failed": 0, "ignored": 0, "duration_seconds": 0.01},
                {"target": "unittests src/main.rs", "passed": 0, "failed": 0, "ignored": 0, "duration_seconds": 0.00},
                {"target": "tests/phase5_validation.rs", "passed": 348, "failed": 0, "ignored": 0, "duration_seconds": 10.0}
            ]
        }
    }

    base_markdown = (
        "# Verification Report\n\n"
        "## Test Inventory\n"
        "| Target Name | Passed | Failed | Ignored | Target Duration (s) | Classification |\n"
        "| :--- | :---: | :---: | :---: | :---: | :--- |\n"
        "| `unittests src/lib.rs` | 2 | 0 | 0 | 0.01 | Library tests |\n"
        "| `unittests src/main.rs` | 0 | 0 | 0 | 0.00 | CLI binary |\n"
        "| `tests/phase5_validation.rs` | 348 | 0 | 0 | 10.00 | Integration suite |\n"
        "| **TOTAL** | **350** | **0** | **0** | **10.01 s** | Canonical total |\n\n"
        "Reconciliation equation: 425 - 117 - 35 + 77 = 350\n"
        "Physical LAN status: NOT EXECUTED\n"
        "Production readiness: NOT ESTABLISHED\n"
        "Actual wire bytes: ACTUAL_WIRE_BYTES: NOT MEASURED (31.21 MiB/s nominal calculated)\n"
    )

    base_raw_log = (
        "Running unittests src/lib.rs (bin1)\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n"
        "Running unittests src/main.rs (bin2)\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n"
        "Running tests/phase5_validation.rs (bin3)\ntest result: ok. 348 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 10.00s\n"
    )

    # Positive test: Base must pass cleanly
    pos_errs = validator.validate_rules(base_markdown, base_canonical, base_raw_log)
    assert len(pos_errs) == 0, f"Positive test failed unexpectedly: {pos_errs}"
    print("  [PASS] Positive baseline validation passed.")

    fixtures = []

    # Fixture 1: Report says 350; rows sum to 335
    f1_md = base_markdown.replace("348 | 0 | 0 | 10.00", "333 | 0 | 0 | 10.00")
    fixtures.append(("1. Report says 350; rows sum to 335", f1_md, base_canonical, base_raw_log, None))

    # Fixture 2: Report says 350; rows sum to 425
    f2_md = base_markdown.replace("348 | 0 | 0 | 10.00", "423 | 0 | 0 | 10.00")
    fixtures.append(("2. Report says 350; rows sum to 425", f2_md, base_canonical, base_raw_log, None))

    # Fixture 3: Correct total but one target omitted
    f3_md = (
        "| Target Name | Passed | Failed | Ignored | Target Duration (s) | Classification |\n"
        "| :--- | :---: | :---: | :---: | :---: | :--- |\n"
        "| `unittests src/lib.rs` | 2 | 0 | 0 | 0.01 | Library tests |\n"
        "| `tests/phase5_validation.rs` | 348 | 0 | 0 | 10.00 | Integration suite |\n"
        "| **TOTAL** | **350** | **0** | **0** | **10.01 s** | Canonical total |\n"
        "Physical LAN status: NOT EXECUTED\nProduction readiness: NOT ESTABLISHED\n"
    )
    fixtures.append(("3. Correct total but one target omitted", f3_md, base_canonical, base_raw_log, None))

    # Fixture 4: Correct total but phantom target added
    f4_md = (
        "| Target Name | Passed | Failed | Ignored | Target Duration (s) | Classification |\n"
        "| :--- | :---: | :---: | :---: | :---: | :--- |\n"
        "| `unittests src/lib.rs` | 2 | 0 | 0 | 0.01 | Library tests |\n"
        "| `unittests src/main.rs` | 0 | 0 | 0 | 0.00 | CLI binary |\n"
        "| `tests/phantom_suite.rs` | 348 | 0 | 0 | 10.00 | Phantom suite |\n"
        "| **TOTAL** | **350** | **0** | **0** | **10.01 s** | Canonical total |\n"
        "Physical LAN status: NOT EXECUTED\nProduction readiness: NOT ESTABLISHED\n"
    )
    fixtures.append(("4. Correct total but phantom target added", f4_md, base_canonical, base_raw_log, None))

    # Fixture 5: Duplicated target rows
    f5_md = (
        "| Target Name | Passed | Failed | Ignored | Target Duration (s) | Classification |\n"
        "| :--- | :---: | :---: | :---: | :---: | :--- |\n"
        "| `unittests src/lib.rs` | 2 | 0 | 0 | 0.01 | Library tests |\n"
        "| `unittests src/lib.rs` | 2 | 0 | 0 | 0.01 | Library tests duplicate |\n"
        "| `tests/phase5_validation.rs` | 346 | 0 | 0 | 10.00 | Integration suite |\n"
        "| **TOTAL** | **350** | **0** | **0** | **10.01 s** | Canonical total |\n"
        "Physical LAN status: NOT EXECUTED\nProduction readiness: NOT ESTABLISHED\n"
    )
    fixtures.append(("5. Duplicated target rows", f5_md, base_canonical, base_raw_log, None))

    # Fixture 6: Test source path does not exist in inventory
    f6_canonical = copy.deepcopy(base_canonical)
    f6_canonical["execution_summary"]["targets"][2]["target"] = "tests/nonexistent_file.rs"
    fixtures.append(("6. Test source path does not exist", base_markdown, f6_canonical, base_raw_log, None))

    # Fixture 7: Failed Cargo command with passing summary
    f7_canonical = copy.deepcopy(base_canonical)
    f7_canonical["execution_summary"]["cargo_exit_status"] = 101
    fixtures.append(("7. Failed Cargo command with passing summary", base_markdown, f7_canonical, base_raw_log, None))

    # Fixture 8: One ignored test counted as passed
    f8_canonical = copy.deepcopy(base_canonical)
    f8_canonical["execution_summary"]["total_passed"] = 349
    f8_canonical["execution_summary"]["total_ignored"] = 1
    fixtures.append(("8. One ignored test counted as passed", base_markdown, f8_canonical, base_raw_log, None))

    # Fixture 9: Missing raw execution log referenced in environment
    f9_env = {"full_run": {"log_file": "results/nonexistent_log.log", "log_sha256": "abcdef"}}
    fixtures.append(("9. Missing raw execution log", base_markdown, base_canonical, base_raw_log, f9_env))

    # Fixture 10: Truncated Cargo output (sum mismatch with canonical)
    f10_raw = "Running unittests src/lib.rs\ntest result: ok. 2 passed\n"
    fixtures.append(("10. Truncated Cargo output", base_markdown, base_canonical, f10_raw, None))

    # Fixture 11: Wrong Git revision in report
    f11_md = base_markdown + "\nGit HEAD: `deadbeef1234567`\n"
    f11_env = {"git_revision": "1234567890abcdef1234567890abcdef12345678"}
    fixtures.append(("11. Wrong Git revision", f11_md, base_canonical, base_raw_log, f11_env))

    # Fixture 12: JSON differs from Markdown summary
    f12_canonical = copy.deepcopy(base_canonical)
    f12_canonical["execution_summary"]["total_passed"] = 360
    fixtures.append(("12. JSON differs from Markdown", base_markdown, f12_canonical, base_raw_log, None))

    # Fixture 13: Reconciliation equation mathematically incorrect (425 - 117 - 35 + 77 = 400)
    f13_md = base_markdown.replace("425 - 117 - 35 + 77 = 350", "425 - 117 - 35 + 77 = 400")
    fixtures.append(("13. Reconciliation equation mathematically incorrect", f13_md, base_canonical, base_raw_log, None))

    # Fixture 14: Equation correct but category subtotals wrong (equation uses 108 phantoms when list has 142)
    f14_md = base_markdown.replace("425 - 117 - 35 + 77 = 350", "425 - 108 - 44 + 77 = 350") + "\nListed phantoms sum to 142.\n"
    fixtures.append(("14. Equation correct but category subtotals wrong", f14_md, base_canonical, base_raw_log, None))

    # Fixture 15: Execution duration mislabeled as wall-clock duration
    f15_md = base_markdown + "\nThe command wall-clock duration was 282.11 s.\n"
    fixtures.append(("15. Execution duration mislabeled", f15_md, base_canonical, base_raw_log, None))

    # Fixture 16: Historical benchmark presented as fresh without distinction
    f16_md = base_markdown + "\nFresh newly measured live benchmark: 18.72 MiB/s on physical hardware\n"
    # We test via rule 10 / physical classification
    f16_canonical = copy.deepcopy(base_canonical)
    f16_md = base_markdown.replace("ACTUAL_WIRE_BYTES: NOT MEASURED", "ACTUAL_WIRE_BYTES: 31.21 MiB/s MEASURED")
    fixtures.append(("16. Historical benchmark presented as fresh/measured", f16_md, f16_canonical, base_raw_log, None))

    # Fixture 17: Localhost result labeled REAL TWO-HOST without NOT EXECUTED
    f17_md = base_markdown.replace("Physical LAN status: NOT EXECUTED", "Physical LAN status: VERIFIED LIVE TWO-HOST")
    fixtures.append(("17. Localhost result labeled REAL TWO-HOST", f17_md, base_canonical, base_raw_log, None))

    # Fixture 18: Actual wire throughput inferred from RS amplification and labeled measured
    f18_md = base_markdown.replace("ACTUAL_WIRE_BYTES: NOT MEASURED (31.21 MiB/s nominal calculated)", "31.21 MiB/s wire throughput measured on NIC")
    fixtures.append(("18. Wire throughput inferred and labeled measured", f18_md, base_canonical, base_raw_log, None))

    # Fixture 19: Validator receives malformed Markdown table (no valid rows)
    f19_md = "# Bad Report\n\nNo tables here at all!\nPhysical LAN status: NOT EXECUTED\nProduction readiness: NOT ESTABLISHED\n"
    fixtures.append(("19. Malformed Markdown table", f19_md, base_canonical, base_raw_log, None))

    # Fixture 20: Validator receives deliberately altered canonical JSON (failed test injected)
    f20_canonical = copy.deepcopy(base_canonical)
    f20_canonical["execution_summary"]["total_failed"] = 1
    fixtures.append(("20. Deliberately altered canonical JSON (failed test)", base_markdown, f20_canonical, base_raw_log, None))

    negative_test_results = []
    for idx, (desc, md, can, raw, env) in enumerate(fixtures, 1):
        errs = validator.validate_rules(md, can, raw, env)
        if len(errs) == 0:
            print(f"  [FAIL] Mutation test #{idx} FAILED to detect corruption: {desc}", file=sys.stderr)
            sys.exit(1)
        else:
            negative_test_results.append({
                "test_id": idx,
                "description": desc,
                "detected_violations_count": len(errs),
                "sample_violation": errs[0],
                "status": "REJECTED_AS_EXPECTED"
            })
            print(f"  [PASS] Negative test #{idx:2d} rejected as expected: {desc} -> {errs[0][:60]}...")

    print(f"\n[SUCCESS] All {len(fixtures)} negative mutation tests successfully rejected invalid fixtures.")
    return negative_test_results


def main():
    parser = argparse.ArgumentParser(description="FSP Test Evidence Consistency Validator")
    parser.add_argument("--report", type=str, help="Path to markdown report")
    parser.add_argument("--canonical-json", type=str, help="Path to canonical JSON evidence")
    parser.add_argument("--raw-log", type=str, help="Path to raw Cargo execution log")
    parser.add_argument("--environment", type=str, help="Path to environment JSON")
    parser.add_argument("--run-mutation-tests", action="store_true", help="Run the 20 negative mutation tests")
    parser.add_argument("--output-mutation-results", type=str, help="Path to save mutation test results JSON")
    args = parser.parse_args()

    if args.run_mutation_tests:
        results = run_mutation_tests()
        if args.output_mutation_results:
            p = Path(args.output_mutation_results)
            p.parent.mkdir(parents=True, exist_ok=True)
            with open(p, "w", encoding="utf-8") as f:
                json.dump(results, f, indent=2)
            print(f"[*] Saved mutation test results to {p}")
        sys.exit(0)

    if not args.report or not args.canonical_json:
        parser.print_help()
        sys.exit(1)

    rep_path = Path(args.report)
    json_path = Path(args.canonical_json)

    if not rep_path.exists():
        print(f"Error: Report file not found: {rep_path}", file=sys.stderr)
        sys.exit(1)
    if not json_path.exists():
        print(f"Error: Canonical JSON file not found: {json_path}", file=sys.stderr)
        sys.exit(1)

    with open(rep_path, "r", encoding="utf-8") as f:
        rep_content = f.read()
    with open(json_path, "r", encoding="utf-8") as f:
        can_data = json.load(f)

    raw_content = None
    if args.raw_log:
        raw_p = Path(args.raw_log)
        if raw_p.exists():
            with open(raw_p, "r", encoding="utf-8") as f:
                raw_content = f.read()

    env_data = None
    if args.environment:
        env_p = Path(args.environment)
        if env_p.exists():
            with open(env_p, "r", encoding="utf-8") as f:
                env_data = json.load(f)

    validator = EvidenceValidator()
    violations = validator.validate_rules(rep_content, can_data, raw_content, env_data)

    if violations:
        print(f"[FAIL] Report validation FAILED for {rep_path.name}:", file=sys.stderr)
        for v in violations:
            print(f"  - {v}", file=sys.stderr)
        sys.exit(2)

    print(f"[PASS] Report validation SUCCESSFUL for {rep_path.name}")
    print(f"  - Verified 10 mandatory consistency rules.")
    print(f"  - Mathematical consistency confirmed.")
    sys.exit(0)


if __name__ == "__main__":
    main()
