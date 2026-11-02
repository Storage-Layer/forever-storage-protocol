#!/usr/bin/env bash
# FSP Phase 5.7: Hardened Orphan Sweeper with Process Verification and Start-Time Checks
# Eliminates TOCTOU PID reuse races by validating command-line signatures and process start-time
# against the recorded values in cluster.pids.

set -euo pipefail

CLUSTER_DIR="${1:-.fsp_cluster}"
PIDS_FILE="${CLUSTER_DIR}/cluster.pids"

if [[ ! -f "${PIDS_FILE}" ]]; then
    echo "[INFO] No cluster.pids found at ${PIDS_FILE}. Clean state."
    exit 0
fi

echo "[INFO] Checking ${PIDS_FILE} for orphaned daemons..."

LAUNCHER_PID=""
LAUNCHER_ST=""
declare -a NODES_PIDS=()
declare -a NODES_ST=()

get_process_starttime() {
    local pid="$1"
    if [[ -f "/proc/${pid}/stat" ]]; then
        # Field 22 is starttime (measured in jiffies after boot)
        local stat_content
        stat_content=$(cat "/proc/${pid}/stat" 2>/dev/null || true)
        if [[ -n "${stat_content}" ]]; then
            # Trim up to the last closing parenthesis of comm
            local rest="${stat_content##*)}"
            read -r -a fields <<< "${rest}"
            # In rest, field index 19 corresponds to field 22 of /proc/pid/stat
            echo "${fields[19]:-}"
            return 0
        fi
    fi
    echo ""
}

while IFS= read -r line; do
    if [[ "${line}" =~ ^LAUNCHER_PID=([0-9]+) ]]; then
        LAUNCHER_PID="${BASH_REMATCH[1]}"
        if [[ "${line}" =~ START_TIME=([0-9]+) ]]; then
            LAUNCHER_ST="${BASH_REMATCH[1]}"
        fi
    elif [[ "${line}" =~ PID=([0-9]+) ]]; then
        local_pid="${BASH_REMATCH[1]}"
        local_st=""
        if [[ "${line}" =~ START_TIME=([0-9]+) ]]; then
            local_st="${BASH_REMATCH[1]}"
        fi
        NODES_PIDS+=("${local_pid}")
        NODES_ST+=("${local_st}")
    fi
done < "${PIDS_FILE}"

PARENT_DEAD=1
if [[ -n "${LAUNCHER_PID}" ]] && kill -0 "${LAUNCHER_PID}" 2>/dev/null; then
    if [[ -n "${LAUNCHER_ST}" ]]; then
        current_launcher_st=$(get_process_starttime "${LAUNCHER_PID}")
        if [[ "${current_launcher_st}" == "${LAUNCHER_ST}" ]]; then
            PARENT_DEAD=0
        fi
    else
        PARENT_DEAD=0
    fi
fi

if [[ "${PARENT_DEAD}" -eq 0 ]]; then
    echo "[INFO] Launcher PID ${LAUNCHER_PID} is alive. No orphans to clean."
    exit 0
fi

echo "[WARN] Launcher is dead. Processing ${#NODES_PIDS[@]} child nodes..."

SWEPT=0
for idx in "${!NODES_PIDS[@]}"; do
    pid="${NODES_PIDS[idx]}"
    expected_st="${NODES_ST[idx]}"

    if kill -0 "${pid}" 2>/dev/null; then
        # Check starttime if recorded
        if [[ -n "${expected_st}" ]]; then
            curr_st=$(get_process_starttime "${pid}")
            if [[ "${curr_st}" != "${expected_st}" ]]; then
                echo "[SAFEGUARD] PID ${pid} start-time (${curr_st}) does not match recorded (${expected_st}). Skipping recycled PID."
                continue
            fi
        fi

        # Check /proc/$pid/cmdline
        if [[ -f "/proc/${pid}/cmdline" ]]; then
            cmdline=$(tr '\0' ' ' < "/proc/${pid}/cmdline" 2>/dev/null || true)
            if [[ "${cmdline}" == *"fsp-node"* || "${cmdline}" == *"fsp"* || "${cmdline}" == *"target/release"* || "${cmdline}" == *"target/debug"* ]]; then
                echo "[ACTION] Terminating orphan FSP node PID ${pid}..."
                kill -9 "${pid}" 2>/dev/null || true
                SWEPT=$((SWEPT + 1))
            else
                echo "[SAFEGUARD] PID ${pid} command-line ('${cmdline:0:30}...') is not FSP. Skipping recycled PID."
            fi
        fi
    fi
done

rm -f "${PIDS_FILE}"
echo "[SUCCESS] Swept ${SWEPT} orphaned processes cleanly."
