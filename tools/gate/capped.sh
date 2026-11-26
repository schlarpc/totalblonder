#!/usr/bin/env bash
# capped.sh [-m SIZE] command [args...]
# Run a command inside a transient systemd scope (cgroup v2) with a hard memory limit and no
# swap. If the command's cgroup exceeds the limit the kernel kills processes in that cgroup
# only; nothing else on the host is touched. Default limit 10G (override: -m 6G or CAP_MEM=6G).
# Refuses to start when the host has less than CAP_MIN_AVAIL_GB (default 12) GiB available.
# Everything memory-hungry on nixark (encoder, benchmarks, ffmpeg, media preparation) must be
# started through this script.
set -euo pipefail
mem=${CAP_MEM:-10G}
if [ "${1:-}" = "-m" ]; then mem=$2; shift 2; fi
[ $# -ge 1 ] || { echo "usage: capped.sh [-m SIZE] command [args...]" >&2; exit 2; }
if [ -n "${CAPPED_INSIDE:-}" ]; then exec "$@"; fi   # already inside a capped scope: do not nest
avail=$(awk '/MemAvailable/ {print int($2/1048576)}' /proc/meminfo)
min=${CAP_MIN_AVAIL_GB:-12}
if [ "$avail" -lt "$min" ]; then
  echo "capped.sh: only ${avail} GiB available on the host (< ${min}); not starting" >&2; exit 75
fi
export CAPPED_INSIDE=1
exec systemd-run --user --scope --quiet --collect --expand-environment=no -p MemoryMax="$mem" -p MemorySwapMax=0 \
  -p OOMPolicy=continue -- "$@"
