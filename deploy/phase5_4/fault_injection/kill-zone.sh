#!/usr/bin/env bash
set -euo pipefail

ZONE="${1:-zone-a}"
echo "=== Injecting Outage for Zone: ${ZONE} ==="

case "${ZONE}" in
  zone-a)
    NODES=(0 1 2 3)
    ;;
  zone-b)
    NODES=(4 5 6 7)
    ;;
  zone-c)
    NODES=(8 9 10 11)
    ;;
  zone-d)
    NODES=(12 13 14 15)
    ;;
  zone-e)
    NODES=(16 17 18 19)
    ;;
  *)
    echo "Unknown zone: ${ZONE}. Valid zones: zone-a..zone-e"
    exit 1
    ;;
esac

for n in "${NODES[@]}"; do
  PORT=$((19000 + n))
  echo "Sending SIGKILL to node ${n} on port ${PORT}..."
  fuser -k -9 "${PORT}/tcp" || true
done

echo "Zone ${ZONE} offline."
