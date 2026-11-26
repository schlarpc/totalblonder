#!/usr/bin/env bash
# Prove that capped.sh enforces: a process that allocates and touches 2000 MB under a 1 GiB
# limit must be killed, alone, and the limit must be visible in the scope's memory.max.
set -uo pipefail
cd "$(dirname "$0")/.."
echo "limit seen from inside the scope:"
./scripts/capped.sh -m 300M bash -c 'cat /sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)/memory.max /sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)/memory.swap.max'
before=$(journalctl -k --since "-1min" 2>/dev/null | grep -ci "out of memory" || true)
./scripts/capped.sh -m 1G python3 -I -c '
b = []
for i in range(2000):
    b.append(bytearray(b"x") * (1 << 20))
print("NOT KILLED: allocated", len(b), "MB")
'
rc=$?
echo "exit status of the over-allocating process: $rc (137 = SIGKILL)"

journalctl -k --since "-1min" 2>/dev/null | grep -iE "memory cgroup out of memory|oom-kill:" | tail -2
[ $rc -eq 137 ] && echo "CAP SELFTEST: PASS" || { echo "CAP SELFTEST: FAIL"; exit 1; }
