#!/usr/bin/env python3
"""
FSP Deterministic Source Manifest Generator
Computes a canonical SHA-256 source manifest and root digest for non-git workspaces.
Excludes build artifacts, volatile results, and transient caches.
"""

import hashlib
import json
import os
import sys
from pathlib import Path


def compute_file_sha256(filepath: Path) -> str:
    h = hashlib.sha256()
    with open(filepath, "rb") as f:
        while chunk := f.read(65536):
            h.update(chunk)
    return h.hexdigest()


def generate_source_manifest(repo_root: Path) -> dict:
    include_extensions = {".rs", ".toml", ".lock", ".py", ".sh", ".service", ".json"}
    include_dirs = {"src", "tests", "tools", "deploy", "scripts"}
    include_root_files = {"Cargo.toml", "Cargo.lock"}

    exclude_dirs = {"target", "results", "docs", ".gemini", "__pycache__", ".git"}
    exclude_extensions = {".key", ".pem", ".log", ".tmp"}

    tracked_files = []

    for path in sorted(repo_root.rglob("*")):
        if not path.is_file():
            continue

        rel_path = path.relative_to(repo_root)
        parts = rel_path.parts

        # Check directory exclusions
        if any(part in exclude_dirs for part in parts[:-1]):
            continue

        # Check extension exclusions
        if path.suffix in exclude_extensions:
            continue

        # Check include rules
        is_included = False
        if len(parts) == 1 and parts[0] in include_root_files:
            is_included = True
        elif len(parts) > 1 and parts[0] in include_dirs:
            if path.suffix in include_extensions or parts[0] == "deploy":
                is_included = True

        if is_included:
            file_sha = compute_file_sha256(path)
            tracked_files.append({
                "path": str(rel_path),
                "size_bytes": path.stat().st_size,
                "sha256": file_sha
            })

    # Sort deterministically by relative path
    tracked_files.sort(key=lambda x: x["path"])

    # Compute root snapshot digest: SHA-256 of concatenated sorted "path sha256\n"
    root_hasher = hashlib.sha256()
    for f in tracked_files:
        root_hasher.update(f"{f['path']} {f['sha256']}\n".encode("utf-8"))
    source_snapshot_digest = root_hasher.hexdigest()

    return {
        "workspace_type": "STANDALONE_WORKSPACE (NOT_A_GIT_REPOSITORY)",
        "source_snapshot_digest": source_snapshot_digest,
        "total_tracked_files": len(tracked_files),
        "inclusion_rules": {
            "root_files": list(sorted(include_root_files)),
            "directories": list(sorted(include_dirs)),
            "extensions": list(sorted(include_extensions))
        },
        "exclusion_rules": {
            "directories": list(sorted(exclude_dirs)),
            "extensions": list(sorted(exclude_extensions))
        },
        "files": tracked_files
    }


def main():
    repo_root = Path(__file__).resolve().parent.parent
    manifest = generate_source_manifest(repo_root)

    out_path = repo_root / "results" / "phase5_8" / "source_provenance.json"
    if len(sys.argv) > 1:
        out_path = Path(sys.argv[1])

    out_path.parent.mkdir(parents=True, exist_ok=True)
    with open(out_path, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=2)

    print(f"[SUCCESS] Generated canonical source manifest: {out_path}")
    print(f"  Tracked Files: {manifest['total_tracked_files']}")
    print(f"  Source Snapshot Digest: {manifest['source_snapshot_digest']}")


if __name__ == "__main__":
    main()
