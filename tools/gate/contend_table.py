#!/usr/bin/env python3
# contend_table.py PATTERN... : one line per competitor; ratio against the neighboring gaps (as contend_report.py), and
# against the pooled alone median of all runs matched by the same pattern (used when Plex activity removed the gaps)
import json, sys, glob, os, statistics as st
for pat in sys.argv[1:]:
    ds = sorted(glob.glob(pat))
    reps = [json.load(open(os.path.join(d, "report.json"))) for d in ds]
    alone = [x["med_ms"] for r in reps for x in r["rows"] if x["comp"] == "(alone)" and x["ticks"] >= 20]
    pool = st.median(alone) if alone else float("nan")
    print(f"## {pat}: pooled alone median {pool:.2f} ms from {len(alone)} runs")
    for d, r in zip(ds, reps):
        for x in r["rows"]:
            if x["comp"].startswith("("): continue
            late = x["late"]
            print(f"  {os.path.basename(d):24s} {x['comp']:44s} n={x['ticks']:4d} med {x['med_ms']:6.2f} ratio {x['ratio'] or float('nan'):5.2f} pooled {x['med_ms']/pool:5.2f} p95 {x['p95_ms']:6.2f} max {x['max_ms'] or 0:6.1f} ontime {100*(1-late) if late is not None else -1:6.1f}% slow {x['slow']:3d} W {x['pwr'] or 0:3.0f} MHz {x['pclk'] or 0:5.0f} | {x['result'][:150]}")
