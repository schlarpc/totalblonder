#!/usr/bin/env python3
"""contend_report.py DIR... : evaluate runs of tools/gate/contend.sh (run on the host of the run: the
nvidia-smi time stamps are local time of day).

Per competitor: encoder tick (median, p95) while it ran against the encoder alone in the gaps
right before and after it, the ratio and the added milliseconds, late ticks, single slow ticks
(> 1.25 x the window's median) and how much of their excess is in phase 0, and the card (watts,
clock, temperature, SM/decoder/encoder utilization, power and thermal violation counters).
Seconds in which another process (Plex) shows activity in pmon are left out of both and are
reported as their own row `(plex)`. Writes DIR/report.json and prints a table.
"""
import json
import os
import re
import statistics as st
import sys
import time


def tod(s):
    h, m, x = s.split(":")
    return int(h) * 3600 + int(m) * 60 + float(x)


def load(d):
    ev = open(os.path.join(d, "events.txt")).read().split("\n")
    h = ev[0].split()
    t0, period = float(h[1]), float(h[3])
    comps = [(x.split()[0], float(x.split()[1]), float(x.split()[2])) for x in ev[1:] if x.strip()]
    lt = time.localtime(t0)
    day0 = t0 - (lt.tm_hour * 3600 + lt.tm_min * 60 + lt.tm_sec + t0 % 1)
    ticks, end = [], 0.0
    for line in open(os.path.join(d, "ticks.txt")):
        f = line.split()
        if len(f) >= 14:
            # when the tick starts: when it is due, or when the one before ended if the encoder is behind
            t = max(t0 + int(f[0]) * period, end)
            end = t + float(f[1]) / 1e3
            ticks.append(dict(t=t, dt=float(f[1]), upwait=float(f[5]), ph=[float(f[6]), float(f[7]), float(f[8])], retry=int(f[10])))
    dm, cols = [], None
    for line in open(os.path.join(d, "dmon.txt")):
        f = line.split()
        if line.startswith("#"):
            if f[0] == "#Time":
                cols = ["Time"] + f[1:]
            continue
        r = dict(zip(cols, f))
        r["t"] = day0 + tod(r["Time"])
        dm.append(r)
    other = []   # seconds in which a process that is not ours and not a competitor was active
    for line in open(os.path.join(d, "pmon.txt")):
        f = line.split()
        if line.startswith("#") or len(f) < 11:
            continue
        if "Plex" in line or "jellyfin" in line.lower():
            act = [int(x) for x in (f[4], f[6], f[7]) if x.isdigit()]
            if act and max(act) > 0:
                other.append((day0 + tod(f[0]), max(act)))
    return dict(t0=t0, period=period, comps=comps, ticks=ticks, dm=dm, other=other, hdr=" ".join(h[4:]))


def med(x):
    return st.median(x) if x else float("nan")


def pct(x, p):
    x = sorted(x)
    return x[min(len(x) - 1, int(len(x) * p))] if x else float("nan")


def dstat(run, a, b):
    rows = [r for r in run["dm"] if a + 1 <= r["t"] <= b + 0.5]
    out = {}
    for k in ("pwr", "gtemp", "sm", "mem", "enc", "dec", "pclk", "pviol", "tviol"):
        v = [float(r[k]) for r in rows if r.get(k, "-").replace(".", "").isdigit()]
        out[k] = (max(v) if k in ("pviol", "tviol", "gtemp") else sum(v) / len(v)) if v else None
    v = [float(r["pclk"]) for r in rows if r.get("pclk", "-").isdigit()]
    out["pclk_min"] = min(v) if v else None
    return out


def comp_result(d, name):
    try:
        s = open(os.path.join(d, f"comp-{name}.log"), errors="replace").read()
    except OSError:
        return ""
    if "gpucomp" in s:
        m = re.search(r"(\d+) launches.*p50 ([\d.]+) p90 ([\d.]+) p99 ([\d.]+) max ([\d.]+)", s)
        return f"{m.group(1)} launches, round trip p50 {m.group(2)} p99 {m.group(4)} max {m.group(5)} ms" if m else s.strip()[-80:]
    parts = []
    for k in range(1, 9) if "+" in name else [0]:
        try:
            t = open(os.path.join(d, f"comp-{name}.{k}.log" if k else f"comp-{name}.log"), errors="replace").read()
        except OSError:
            break
        fr = re.findall(r"frame=\s*(\d+) fps=\s*([\d.]+)", t)
        err = [x for x in re.split(r"[\r\n]", t) if x and not x.startswith("frame=")]
        parts.append((f"{fr[-1][0]} frames, {fr[-1][1]} fps" if fr else "no frames") + (f"; {err[-1][:70]}" if err else ""))
    return " | ".join(parts)


def window(run, a, b):
    bad = run["other"]
    return [k for k in run["ticks"] if a <= k["t"] <= b and not any(s - 1.5 <= k["t"] <= s + 0.5 for s, _ in bad)]


def row(run, name, ws, base, a, b, d=None):
    dts, bd = [k["dt"] for k in ws], [k["dt"] for k in base]
    m, bm = med(dts), med(bd)
    slow = [k for k in ws if k["dt"] > 1.25 * m]
    mph0 = med([k["ph"][0] for k in ws])
    r = dict(comp=name, ticks=len(ws), base_ms=bm, med_ms=m, p95_ms=pct(dts, 0.95), max_ms=max(dts) if dts else None,
             ratio=m / bm if bd and dts else None, added_ms=m - bm if bd and dts else None,
             late=sum(1 for x in dts if x > 1e3 * run["period"]) / len(dts) if dts else None,
             slow=len(slow), slow_excess_ms=med([k["dt"] - m for k in slow]) if slow else None,
             slow_ph0_excess_ms=med([k["ph"][0] - mph0 for k in slow]) if slow else None,
             ph_ms=[med([k["ph"][i] for k in ws]) for i in range(3)], base_ph_ms=[med([k["ph"][i] for k in base]) for i in range(3)],
             upwait_ms=med([k["upwait"] for k in ws]), result=comp_result(d, name) if d else "")
    r.update(dstat(run, a, b))
    return r


def main():
    for d in sys.argv[1:]:
        run = load(d)
        end = run["ticks"][-1]["t"]
        rows, busy = [], [(a - 0.3, b + 1.2) for _, a, b in run["comps"]]
        alone = [k for k in window(run, run["t0"] + 2, end) if not any(a <= k["t"] <= b for a, b in busy)]
        rows.append(row(run, "(alone)", alone, alone, run["t0"] + 2, run["comps"][0][1] if run["comps"] else end))
        for name, a, b in run["comps"]:
            base = [k for k in alone if a - 3.0 <= k["t"] <= a or b <= k["t"] <= b + 4.0]
            rows.append(row(run, name, window(run, a + 2.5, b - 0.3), base, a + 2.5, b - 0.3, d))
        px = [k for k in run["ticks"] if any(s - 1.0 <= k["t"] <= s for s, _ in run["other"]) and not any(a <= k["t"] <= b for a, b in busy)]
        if px:
            rows.append(row(run, "(plex)", px, alone, 0, 0))
        json.dump(dict(dir=d, hdr=run["hdr"], period_ms=1e3 * run["period"], other_active_s=len(run["other"]), rows=rows), open(os.path.join(d, "report.json"), "w"), indent=1)
        print(f"\n## {d}: {run['hdr']}, period {1e3 * run['period']:.1f} ms, seconds with Plex/Jellyfin activity: {len(run['other'])}")
        print("competitor      ticks  base   med   ratio  +ms    p95   late%  slow(ex,ph0)   W    MHz(min)  degC sm% dec% enc% pviol tviol  competitor result")
        f = lambda v, w, p=1: (f"{v:{w}.{p}f}" if v is not None and v == v else " " * (w - 1) + "-")
        for r in rows:
            sl = f"{r['slow']}({f(r['slow_excess_ms'], 0)},{f(r['slow_ph0_excess_ms'], 0)})" if r["slow"] else "0"
            print(f"{r['comp']:15s} {r['ticks']:5d} {f(r['base_ms'], 5, 2)} {f(r['med_ms'], 5, 2)} {f(r['ratio'], 6, 3)} {f(r['added_ms'], 5, 2)} {f(r['p95_ms'], 6, 2)} "
                  f"{f(100 * r['late'] if r['late'] is not None else None, 5)} {sl:14s} {f(r['pwr'], 4)} {f(r['pclk'], 5, 0)}({f(r['pclk_min'], 4, 0)}) {f(r['gtemp'], 3, 0)} "
                  f"{f(r['sm'], 3, 0)} {f(r['dec'], 3, 0)} {f(r['enc'], 3, 0)} {f(r['pviol'], 3, 0)} {f(r['tviol'], 3, 0)}  {r['result']}")


main()
