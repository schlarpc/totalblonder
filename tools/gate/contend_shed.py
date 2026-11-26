#!/usr/bin/env python3
"""contend_shed.py DIR... : runs of tools/gate/contend.sh with the deadline policy on (ENC_ARGS=--deadline).
Per competitor window: ticks late (output after the next tick was due), pictures replaced by repeats as a share of the
pictures due, the largest backlog (how far a tick started after its time), the median tick; and the same for the gaps
before and after (alone), which shows the recovery."""
import os, sys, statistics as st
for d in sys.argv[1:]:
    ev = open(os.path.join(d, "events.txt")).read().split("\n")
    t0, per = float(ev[0].split()[1]), float(ev[0].split()[3])
    comps = [(x.split()[0], float(x.split()[1]), float(x.split()[2])) for x in ev[1:] if x.strip()]
    ticks, end = [], 0.0
    for line in open(os.path.join(d, "ticks.txt")):
        f = line.split()
        if len(f) < 19: continue
        due = t0 + int(f[0]) * per
        t = max(due, end); end = t + float(f[1]) / 1e3
        ticks.append(dict(t=t, lag=t - due, dt=float(f[1]), shed=int(f[14]), cand=int(f[17]), late=int(f[18]),
                          again=int(f[19]) if len(f) > 20 else 0, fresh=int(f[20]) if len(f) > 20 else 0, slate=int(f[21]) if len(f) > 21 else 0))
    def stat(name, a, b):
        w = [k for k in ticks if a <= k["t"] <= b]
        if not w: return
        cand = sum(k["cand"] for k in w); shed = sum(k["shed"] for k in w)
        print(f"  {name:34s} ticks {len(w):4d}  late {100 * sum(k['late'] for k in w) / len(w):5.1f} %  repeats {shed:5d} of {cand:6d} pictures = {100 * shed / max(1, cand):5.1f} %  "
              f"median tick {st.median(k['dt'] for k in w):6.2f} ms ({st.median(k['dt'] for k in w) / per / 1e3:4.2f} of the period)  backlog at most {1e3 * max(k['lag'] for k in w):6.1f} ms" +
              (f"  output frames from new input {sum(k['fresh'] for k in w)}, input repeated {sum(k['again'] for k in w)} = {100 * sum(k['again'] for k in w) / max(1, sum(k['again'] + k['fresh'] + k['slate'] for k in w)):.1f} %, slate {sum(k['slate'] for k in w)}"
               if any(k['fresh'] for k in w) else "") + res.get(name, ""))
    res = {}
    import re
    for name, _, _ in comps:   # what the competitors achieved
        parts = []
        for k in ([0] if "+" not in name else range(1, 9)):
            fn = os.path.join(d, f"comp-{name}.{k}.log" if k else f"comp-{name}.log")
            if not os.path.exists(fn): break
            fr = re.findall(r"frame=\s*(\d+) fps=\s*([\d.]+)", open(fn, errors="replace").read())
            parts.append(f"{fr[-1][1]} fps" if fr else "no frames")
        res[name] = "  competitor: " + " | ".join(parts)
    print(f"## {os.path.basename(d.rstrip('/'))}: {' '.join(ev[0].split()[4:])}, period {1e3 * per:.2f} ms")
    prev = t0 + 2
    for name, a, b in comps:
        stat("(alone before)", prev, a - 0.3)
        stat(name, a + 0.5, b)
        prev = b + 1.0
    stat("(alone after)", prev, ticks[-1]["t"] if ticks else prev)
