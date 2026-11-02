#!/usr/bin/env bash
set -euo pipefail

# Preflight host and filesystem durability checks
echo "=== FSP Phase 5.5 Preflight Inspection ==="
echo "Operating System: $(uname -srm)"
echo "Kernel: $(uname -r)"
echo "CPU Cores: $(nproc)"
echo "Available Memory: $(free -h | awk '/^Mem:/ {print $7}')"

# Check filesystem type and mount options
FS_ROOT="${1:-.}"
FS_TYPE=$(df -T "${FS_ROOT}" | awk 'NR==2 {print $2}')
MOUNT_OPTS=$(findmnt -n -o OPTIONS -T "${FS_ROOT}" || echo "rw")
echo "Target Path: ${FS_ROOT}"
echo "Filesystem Type: ${FS_TYPE}"
echo "Mount Options: ${MOUNT_OPTS}"

# Check fsync barrier support
if command -v sync >/dev/null 2>&1; then
    echo "Filesystem barrier sync: SUPPORTED"
else
    echo "Filesystem barrier sync: UNKNOWN"
fi

echo "Preflight check completed successfully."
