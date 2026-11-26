#!/usr/bin/env python3
"""nsys_tick.py TRACE.sqlite : what the GPU does within one encoder tick.

Input: an Nsight Systems trace exported to SQLite (tools/gate/nsys_profile.sh). A tick starts at the
host call that launches k_tick (the encoder's --tickmark option); the script prints
  - per tick (averaged over the steady part of the run): tick length, time the GPU is busy
    (union of kernels, copies and memsets; and of kernels and memsets alone, which leaves out
    the frame uploads that run next to the kernels), idle time, split by what the host was doing;
  - launch overhead: time from the API call to the start of the kernel, API time per call type;
  - the timeline of one median tick: every GPU activity with start offset, duration and the idle
    gap before it.
"""
import sqlite3
import sys

db = sqlite3.connect(sys.argv[1])
names = dict(db.execute("select id, value from StringIds"))
kern = [(s, e, names[n], c) for s, e, n, c in db.execute("select start, end, shortName, correlationId from CUPTI_ACTIVITY_KIND_KERNEL")]
KIND = {1: "memcpy H2D", 2: "memcpy D2H", 8: "memcpy D2D"}
mcpy = [(s, e, KIND.get(k, f"memcpy {k}") + f" {b} B", c) for s, e, k, b, c in
        db.execute("select start, end, copyKind, bytes, correlationId from CUPTI_ACTIVITY_KIND_MEMCPY")]
mset = [(s, e, f"memset {b} B", c) for s, e, b, c in db.execute("select start, end, bytes, correlationId from CUPTI_ACTIVITY_KIND_MEMSET")]
api = [(s, e, names[n].split("_v")[0], c) for s, e, n, c in db.execute("select start, end, nameId, correlationId from CUPTI_ACTIVITY_KIND_RUNTIME")]
api.sort()
gpu = sorted(kern + mcpy + mset)
api_start = {c: (s, e, n) for s, e, n, c in api}

# Ticks: the encoder launches the empty kernel k_tick at the start of every step().
starts = sorted(api_start.get(c, (s0,))[0] for s0, e, n, c in kern if n == "k_tick")
gpu = [x for x in gpu if x[2] != "k_tick"]
ticks = [(starts[i], starts[i + 1]) for i in range(len(starts) - 1)]
steady = ticks[len(ticks) // 3:]
if not steady:
    print("no ticks found")
    sys.exit(0)


def union(iv):
    tot, cur_s, cur_e = 0, None, None
    for s, e in sorted(iv):
        if cur_e is None or s > cur_e:
            if cur_e is not None:
                tot += cur_e - cur_s
            cur_s, cur_e = s, e
        else:
            cur_e = max(cur_e, e)
    if cur_e is not None:
        tot += cur_e - cur_s
    return tot


rows = []
for t0, t1 in steady:
    act = [(max(s, t0), min(e, t1)) for s, e, n, c in gpu if e > t0 and s < t1]
    busy = union(act)
    kbusy = union([(max(s, t0), min(e, t1)) for s, e, n, c in gpu if e > t0 and s < t1 and not n.startswith("memcpy")])
    sync = union([(max(s, t0), min(e, t1)) for s, e, n, c in api if n == "cudaStreamSynchronize" and e > t0 and s < t1])
    launch = sum(min(e, t1) - max(s, t0) for s, e, n, c in api if n != "cudaStreamSynchronize" and e > t0 and s < t1)
    nk = sum(1 for s, e, n, c in kern if t0 <= s < t1)
    nc = sum(1 for s, e, n, c in mcpy + mset if t0 <= s < t1)
    rows.append((t1 - t0, busy, sync, launch, nk, nc, kbusy))
n = len(rows)
m = [sum(r[i] for r in rows) / n for i in range(7)]
print(f"{len(ticks)} ticks found, {n} used (the last two thirds)")
print(f"per tick: length {m[0] / 1e6:.3f} ms; GPU busy {m[1] / 1e6:.3f} ms ({100 * m[1] / m[0]:.1f} %), idle {(m[0] - m[1]) / 1e6:.3f} ms ({100 * (m[0] - m[1]) / m[0]:.1f} %)")
print(f"          kernels and memsets alone busy {m[6] / 1e6:.3f} ms ({100 * m[6] / m[0]:.1f} %); tick length min {min(r[0] for r in rows) / 1e6:.3f}, "
      f"median {sorted(r[0] for r in rows)[n // 2] / 1e6:.3f}, max {max(r[0] for r in rows) / 1e6:.3f} ms")
print("          tick lengths, ms: " + " ".join(f"{r[0] / 1e6:.1f}" for r in rows))
print(f"          host: in cudaStreamSynchronize {m[2] / 1e6:.3f} ms, in other CUDA API calls {m[3] / 1e6:.3f} ms, "
      f"outside CUDA {(m[0] - m[2] - m[3]) / 1e6:.3f} ms; {m[4]:.0f} kernel launches, {m[5]:.0f} copies and memsets")
# launch latency: API call start -> kernel start, when the GPU was idle at the time of the call
lat = []
prev_end = 0
for s, e, nme, c in gpu:
    a = api_start.get(c)
    if a and a[0] >= prev_end:      # nothing queued in front of it
        lat.append(s - a[0])
    prev_end = max(prev_end, e)
lat.sort()
if lat:
    print(f"launch latency when the GPU is idle (API call to start on the GPU): median {lat[len(lat) // 2] / 1e3:.1f} us, "
          f"90th percentile {lat[int(len(lat) * 0.9)] / 1e3:.1f} us ({len(lat)} activities)")
per = {}
for s, e, nme, c in api:
    if steady[0][0] <= s < steady[-1][1]:
        d = per.setdefault(nme, [0, 0])
        d[0] += 1
        d[1] += e - s
print("API calls per tick (count, total ms, mean us):")
for k, (cnt, tot) in sorted(per.items(), key=lambda kv: -kv[1][1]):
    print(f"  {k:26} {cnt / n:7.1f} {tot / n / 1e6:8.3f} {tot / cnt / 1e3:8.1f}")
# idle gaps by the activity that follows
gaps = {}
for t0, t1 in steady:
    cur = t0
    for s, e, nme, c in gpu:
        if e <= t0 or s >= t1:
            continue
        if s > cur:
            g = gaps.setdefault(nme.split()[0] + (" " + nme.split()[1] if nme.startswith("mem") and len(nme.split()) > 1 and nme.split()[1] in ("H2D", "D2H") else ""), [0, 0])
            g[0] += 1
            g[1] += s - cur
        cur = max(cur, e)
print("GPU idle time before an activity, per tick (count, total ms):")
for k, (cnt, tot) in sorted(gaps.items(), key=lambda kv: -kv[1][1])[:12]:
    print(f"  {k:26} {cnt / n:7.1f} {tot / n / 1e6:8.3f}")
# one median tick
order = sorted(range(n), key=lambda i: rows[i][0])
t0, t1 = steady[order[n // 2]]
print(f"\ntimeline of a median tick ({(t1 - t0) / 1e6:.3f} ms): start offset ms, duration ms, idle gap before ms, activity")
cur = t0
agg = None
for s, e, nme, c in gpu:
    if s < t0 or s >= t1:
        continue
    gap = max(0, s - cur)
    key = nme if not nme.startswith("mem") else ("memset" if nme.startswith("memset") else " ".join(nme.split()[:2]))
    if agg and agg[3] == key and gap < 20000:
        agg[1] = e
        agg[4] += 1
    else:
        if agg:
            print(f"  {(agg[0] - t0) / 1e6:8.3f} {(agg[1] - agg[0]) / 1e6:8.3f} {agg[2] / 1e6:8.3f}  {agg[3]}" + (f" x{agg[4]}" if agg[4] > 1 else ""))
        agg = [s, e, gap, key, 1]
    cur = max(cur, e)
if agg:
    print(f"  {(agg[0] - t0) / 1e6:8.3f} {(agg[1] - agg[0]) / 1e6:8.3f} {agg[2] / 1e6:8.3f}  {agg[3]}" + (f" x{agg[4]}" if agg[4] > 1 else ""))
