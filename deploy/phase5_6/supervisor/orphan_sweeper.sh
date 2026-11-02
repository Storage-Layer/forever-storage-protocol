#!/usr/bin/env bash
# FSP Phase 5.6: Safe Orphan Daemon Sweeper with PID Reuse Protection
# Scans cluster.pids and safely terminates orphaned fsp-node processes.
# Guards against killing unrelated processes if OS reallocated a recycled PID.

set -euo pipefail

CLUSTER_DIR="${1:-.fsp_cluster}"
PIDS_FILE="${CLUSTER_DIR}/cluster.pids"

if [[ ! -f "${PIDS_FILE}" ]]; then
    echo "[INFO] No active cluster.pids found at ${PIDS_FILE}. Nothing to sweep."
    exit 0
fi

echo "[INFO] Inspecting ${PIDS_FILE} for orphaned FSP processes..."

LAUNCHER_PID=""
CHILD_PIDS=()

while IFS= read -r line; do
    if [[ "${line}" =~ ^LAUNCHER_PID=([0-9]+) ]]; then
        LAUNCHER_PID="${BASH_REMATCH[1]}"
    elif [[ "${line}" =~ PID=([0-9]+) ]]; then
        CHILD_PIDS+=("${BASH_REMATCH[1]}")
    fi
done < "${PIDS_FILE}"

PARENT_ALIVE=0
if [[ -n "${LAUNCHER_PID}" ]] && kill -0 "${LAUNCHER_PID}" 2>/dev/null; then
    PARENT_ALIVE=1
fi

if [[ "${PARENT_ALIVE}" -eq 1 ]]; then
    echo "[INFO] Launcher PID ${LAUNCHER_PID} is still alive. Not sweeping children."
    exit 0
fi

echo "[WARN] Launcher PID ${LAUNCHER_PID} is dead. Auditing ${#CHILD_PIDS[@]} child processes..."

TERMINATED_COUNT=0
for pid in "${CHILD_PIDS[@]}"; do
    if kill -0 "${pid}" 2>/dev/null; then
        # Check /proc/$pid/cmdline to ensure this PID belongs to an FSP daemon
        CMDLINE_FILE="/proc/${pid}/cmdline"
        if [[ -f "${CMDLINE_FILE}" ]]; then
            CMDLINE=$(tr '\0' ' ' < "${CMDLINE_FILE}" 2>/dev/null || true)
            if [[ "${CMDLINE}" == *"fsp-node"* || "${CMDLINE}" == *"fsp"* || "${CMDLINE}" == *"target/debug"* ]]; then
                echo "[ACTION] Terminating verified orphaned FSP daemon PID ${pid} (${CMDLINE:0:40}...)"
                kill -9 "${pid}" 2>/dev/null || true
                TERMINATED_COUNT=$((TERMINATED_COUNT + 1))
            else
                echo "[SAFEGUARD] Skipping PID ${pid}: cmdline '${CMDLINE:0:40}' does not match FSP. PID was reused by OS."
            fi
        else
            echo "[SAFEGUARD] /proc/${pid}/cmdline not readable. Skipping."
        fi
    fi
done

rm -f "${PIDS_FILE}"
echo "[SUCCESS] Swept ${TERMINATED_COUNT} orphaned daemons safely. Removed ${PIDS_FILE}."
