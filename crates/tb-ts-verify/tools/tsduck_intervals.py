#!/usr/bin/env python3
"""Repetition intervals of tables from TSDuck's section log, to compare with
`tb-ts-verify --tables`.

usage: tsp --atsc -I file X.ts -P tables --all-sections --packet-index --log \
           [--pid ...] -O drop 2>&1 | python3 -I tsduck_intervals.py RATE

Prints, per (PID, table_id, table_id_extension, section), the number of occurrences and
the largest interval in ms between the packets holding their last bytes, at RATE bit/s.
"""

import re
import sys

rate = float(sys.argv[1])
line_re = re.compile(
    r"Packet ([\d,]+) to ([\d,]+), PID (0x[0-9A-F]+), TID (0x[0-9A-F]+), "
    r"TIDext (0x[0-9A-F]+), V(\d+), Sec (\d+)/(\d+)"
)
last = {}
stats = {}
for line in sys.stdin:
    m = line_re.search(line)
    if not m:
        continue
    end = int(m.group(2).replace(",", ""))
    key = (m.group(3), m.group(4), m.group(5), m.group(7))
    if key in last:
        ms = (end - last[key]) * 1504 / rate * 1000
        n, mx = stats.get(key, (1, 0.0))
        stats[key] = (n + 1, max(mx, ms))
    else:
        stats.setdefault(key, (1, 0.0))
    last[key] = end
for key in sorted(stats):
    n, mx = stats[key]
    print(f"PID {key[0]} TID {key[1]} ext {key[2]} sec {key[3]}: {n} times, max {mx:.1f} ms")
