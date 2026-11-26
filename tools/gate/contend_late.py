#!/usr/bin/env python3
# contend_late.py DIR... : per competitor window of a contend.sh run, late ticks and how they cluster.
# A tick is late when it takes longer than the period. In the paced encoder a late tick delays the next one
# (it starts at once), so a run of late ticks is a growing delay; "repeat share" = 1 - period / mean tick within
# the window: the share of pictures a skip-on-deadline policy would have to repeat to keep real time.
import os, sys, statistics as st
for d in sys.argv[1:]:
    ev = open(os.path.join(d, "events.txt")).read().split("\n")
    t0, per = float(ev[0].split()[1]), float(ev[0].split()[3]) * 1e3
    comps = [(x.split()[0], float(x.split()[1]), float(x.split()[2])) for x in ev[1:] if x.strip()]
    ticks, end = [], 0.0
    for line in open(os.path.join(d, "ticks.txt")):
        f = line.split()
        if len(f) >= 14:
            t = max(t0 + int(f[0]) * per / 1e3, end); end = t + float(f[1]) / 1e3   # actual start when behind
            ticks.append((t, float(f[1])))
    first = comps[0][1]
    alone = [dt for t, dt in ticks if t0 + 2 < t < first]
    print(f"## {os.path.basename(d)} period {per:.2f} ms, alone before: median {st.median(alone):.2f} ms ({100*st.median(alone)/per:.0f} % of period)")
    for name, a, b in comps:
        w = [dt for t, dt in ticks if a + 0.5 <= t <= b]
        if not w: continue
        late = [dt > per for dt in w]
        lag = max([t - (t0 + i * per / 1e3) for i, (t, _) in enumerate(ticks) if a <= t <= b + 1], default=0)
        streak = best = 0; streaks = []
        for x in late:
            streak = streak + 1 if x else 0
            if not x and best: pass
            best = max(best, streak)
        runs, cur = [], 0
        for x in late + [False]:
            if x: cur += 1
            elif cur: runs.append(cur); cur = 0
        rep = max(0.0, 1 - per / st.mean(w))
        print(f"  {name:40s} ticks {len(w):4d} median {st.median(w):6.2f} ms ({st.median(w)/per:4.2f} of period) late {100*sum(late)/len(w):5.1f} % "
              f"late runs {len(runs)} longest {max(runs) if runs else 0} ticks ({(max(runs) if runs else 0)*per/1e3:.2f} s), repeat share {100*rep:.0f} %, backlog at most {lag:.2f} s")
