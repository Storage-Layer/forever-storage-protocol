#!/usr/bin/env bash
# FSP Phase 5.7: Physical Host Preflight Verification Script
# Validates system limits, network interfaces, filesystem mount properties, and crypto requirements.

set -euo pipefail

DRY_RUN=0
if [[ "${1:-}" == "--dry-run" ]]; then
    DRY_RUN=1
    echo "[MODE] Running in DRY-RUN mode. Validations will be logged without enforcing changes."
fi

echo "================================================================================"
echo " FSP Phase 5.7 Physical Host Preflight Verification"
echo " Target: Two-Host Staging LAN Pilot Prerequisites"
echo "================================================================================"

# 1. Check CPU and Architecture
ARCH=$(uname -m)
CORES=$(nproc)
echo "[CHECK] Architecture: ${ARCH}, Logical CPU Cores: ${CORES}"
if [[ "${ARCH}" != "x86_64" && "${ARCH}" != "aarch64" ]]; then
    echo "[FAIL] Unsupported architecture: ${ARCH}"
    [[ "${DRY_RUN}" -eq 1 ]] || exit 1
fi

# 2. Check Memory
MEM_TOTAL_KB=$(grep MemTotal /proc/meminfo | awk '{print $2}')
MEM_TOTAL_GB=$((MEM_TOTAL_KB / 1024 / 1024))
echo "[CHECK] RAM: ${MEM_TOTAL_GB} GiB"
if [[ "${MEM_TOTAL_GB}" -lt 4 ]]; then
    echo "[WARN] Host has less than 4 GiB RAM; at least 8 GiB recommended for 10-node physical staging."
fi

# 3. Check Filesystem & Mount options
TARGET_DIR="${FSP_DATA_ROOT:-/var/lib/fsp}"
echo "[CHECK] Target storage root: ${TARGET_DIR}"
FS_TYPE=$(df -T "${TARGET_DIR}" 2>/dev/null | tail -n 1 | awk '{print $2}' || echo "unknown")
echo "[CHECK] Filesystem type: ${FS_TYPE}"
if [[ "${FS_TYPE}" != "ext4" && "${FS_TYPE}" != "xfs" && "${FS_TYPE}" != "btrfs" ]]; then
    echo "[WARN] Target directory filesystem is '${FS_TYPE}'. Production clusters require ext4 or xfs with barrier support."
fi

# 4. Check File Descriptors Limit
NOFILE=$(ulimit -n)
echo "[CHECK] Open file descriptor limit: ${NOFILE}"
if [[ "${NOFILE}" -lt 65536 ]]; then
    echo "[WARN] Current nofile limit (${NOFILE}) is lower than recommended (65536). Systemd units enforce LimitNOFILE=65536."
fi

# 5. Check Physical NICs
echo "[CHECK] Physical Network Interfaces:"
for iface in /sys/class/net/*; do
    name=$(basename "${iface}")
    if [[ "${name}" != "lo" && ! "${name}" =~ ^veth && ! "${name}" =~ ^docker && ! "${name}" =~ ^br- ]]; then
        operstate=$(cat "${iface}/operstate" 2>/dev/null || echo "unknown")
        mtu=$(cat "${iface}/mtu" 2>/dev/null || echo "unknown")
        echo "  - Interface ${name}: state=${operstate}, MTU=${mtu}"
    fi
done

# 6. Check Binaries and TLS tools
for tool in openssl ip ss awk; do
    if command -v "${tool}" >/dev/null 2>&1; then
        echo "[CHECK] Required tool found: ${tool}"
    else
        echo "[FAIL] Missing required system tool: ${tool}"
        [[ "${DRY_RUN}" -eq 1 ]] || exit 1
    fi
done

echo "================================================================================"
echo " Preflight verification completed successfully."
echo "================================================================================"
