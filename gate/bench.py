#!/usr/bin/env python3
"""The benchmark's definitions, its line-ups and the card sampler (used by bench_capacity.py, the
standing capacity benchmark: `nix run .#gate-bench`), and the spike's older benchmark run
(`python3 -I gate/bench.py --label L --mode quick|cap|sustain`, with M2VENC, inside the memory cap)
whose results are under spikes/cuda-mpeg2/results/bench (frozen history).

Definitions (stable; do not change them between runs that are to be compared):
  tick       one frame period of the format; in a tick every session takes one input frame and
             (after the reorder delay) produces one coded picture
  free-run   macroblocks per second with no pacing at a fixed reference N per format
             (1080i/1080p/720p: 16 sessions, 480i: 96), 5 s of video (150 or 300 pictures)
  on time    paced run: the tick's output exists before the next tick is due (deadline form;
             the first second of a run is left out as warm-up)
  mixed      the format "mixed" is a fixed line-up in one process: N units of (1 x 1080i +
             1 x 720p59.94 + 4 x 480i) on a 59.94 Hz tick; the reference is N = 8 (8 + 8 + 32
             sessions, 4.95 M macroblocks/s of real-time load); its capacity is counted in units
  capacity   cap99: largest session count N for which >= 99 % of ticks are on time in a paced
             run of 25 s on hard content (crowd_run), found by bisection. cap999: the largest
             tested N with >= 99.9 %.

Modes:
  quick    (under a minute; the iteration metric) per format: 3 free-runs (mean and spread of
           macroblocks/s) and one stage profile (the encoder synchronizes after every stage).
  cap      (milestones only, about 2 minutes per format) quick, then the capacity bisection.
  sustain  one paced run of --paced-seconds (default 180) at --paced-n per format: steady-state
           temperature, power and on-time fraction.

Every run records: GPU utilization, power, temperature and clock (nvidia-smi dmon, 1 s samples),
other GPU processes (nvidia-smi pmon; a run is marked contaminated if another process used the
GPU), load average, host CPU. temp_start tells whether a number was taken warm or cold.
Output: $M2V_RESULTS/bench/<label>.json (everything) and <label>.csv (one row per run).
"""
import argparse
import csv
import json
import os
import subprocess
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formats as F  # noqa: E402

RES = os.path.join(F.RESULTS, "bench")
REF_N = {"1080i": 16, "1080p": 16, "720p": 16, "480i": 96, "mixed": 8}     # fixed N for free-run and profile
STEP = {"1080i": 1, "1080p": 1, "720p": 1, "480i": 4, "mixed": 1}
# The mixed line-up: N units of (1 x 1080i + 1 x 720p + 4 x 480i) in one process on a 59.94 Hz
# tick; N = 8 is 8 + 8 + 32 sessions. One unit is 618,579 macroblocks per second.
MIXED = (("1080i", 1), ("720p", 1), ("480i", 4))
MIXED_FPS = 60000 / 1001


def fmt_fps(fmt):
    return MIXED_FPS if fmt == "mixed" else F.fps(fmt)


def unit_mb_per_s(fmt):
    if fmt == "mixed":
        return sum(m * unit_mb_per_s(f) for f, m in MIXED)
    w, h = F.FORMATS[fmt]["size"]
    return ((w + 15) // 16) * ((h + 15) // 16) * F.fps(fmt)


def mixed_cmd(clip, n, seconds, extra):
    cmd, glob = [], [x for x in extra if x != "--stagger"]
    for i, (f, m) in enumerate(MIXED):
        a = F.enc_args(f, clip)
        cmd += (a if i == 0 else ["--group"] + a[1:]) + ["--sessions", str(n * m), "--frames", str(int(F.fps(f) * seconds))]
        if "--stagger" in extra:
            cmd.append("--stagger")
    return cmd + glob
MAX_IN = 48          # input frames pinned in host memory per process (ring)
WARM_S = 1.0


def sh(cmd):
    return subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True).stdout


class Sampler:
    """nvidia-smi dmon and pmon in the background for the length of one run."""

    def __enter__(self):
        self.d = subprocess.Popen(["nvidia-smi", "dmon", "-s", "pucm", "-d", "1"], stdout=subprocess.PIPE, text=True)
        self.p = subprocess.Popen(["nvidia-smi", "pmon", "-s", "um", "-d", "2"], stdout=subprocess.PIPE, text=True)
        self.load0 = os.getloadavg()[0]
        return self

    def __exit__(self, *a):
        for x in (self.d, self.p):
            x.terminate()
        dout, pout = self.d.communicate()[0], self.p.communicate()[0]
        cols, rows = [], []
        for line in dout.splitlines():
            t = line.split()
            if line.startswith("#"):
                if "gpu" in t:
                    cols = t[1:]
                continue
            if cols and len(t) == len(cols):
                try:
                    rows.append({c: float(v) if v != "-" else 0.0 for c, v in zip(cols, t)})
                except ValueError:
                    pass
        busy = [r for r in rows if r.get("sm", 0) > 0] or rows
        def stat(k, f):
            v = [r[k] for r in busy if k in r]
            return round(f(v), 1) if v else None
        mean = lambda v: sum(v) / len(v)  # noqa: E731
        self.gpu = dict(samples=len(busy), sm_mean=stat("sm", mean), sm_max=stat("sm", max), mem_mean=stat("mem", mean),
                        watts_mean=stat("pwr", mean), watts_max=stat("pwr", max), temp_start=rows[0]["gtemp"] if rows else None,
                        temp_max=stat("gtemp", max), pclk_min=stat("pclk", min), pclk_max=stat("pclk", max),
                        fb_mb_max=stat("fb", max), enc_max=stat("enc", max), dec_max=stat("dec", max))
        others = {}
        for line in pout.splitlines():
            t = line.split()
            if line.startswith("#") or len(t) < 5 or t[1] == "-":
                continue
            name = t[-1]
            if "tb-m2venc" in name or os.path.basename(F.M2VENC)[:15] in name:
                continue
            use = sum(float(x) for x in t[3:-1] if x.replace(".", "").isdigit())
            others[name] = max(others.get(name, 0.0), use)
        self.gpu["other_processes"] = others
        self.gpu["contaminated"] = bool(any(v > 0 for v in others.values()) or (self.gpu["enc_max"] or 0) > 0
                                        or (self.gpu["dec_max"] or 0) > 0)
        self.gpu["load_before"] = round(self.load0, 2)
        self.gpu["load_after"] = round(os.getloadavg()[0], 2)


def encode(fmt, clip, n, frames, extra, paced=False, profile=False):
    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tf:
        jpath = tf.name
    if fmt == "mixed":
        cmd = mixed_cmd(clip, n, frames / MIXED_FPS, extra) + ["--write", "0", "--max-in-frames", str(MAX_IN), "--json", jpath]
    else:
        cmd = F.enc_args(fmt, clip) + ["--sessions", str(n), "--frames", str(frames), "--write", "0",
                                       "--max-in-frames", str(MAX_IN), "--json", jpath] + extra
    if paced:
        cmd += ["--paced", "--warmup", str(int(WARM_S * fmt_fps(fmt)))]
    if profile:
        cmd += ["--profile"]
    with Sampler() as s:
        t0 = time.time()
        p = subprocess.run(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        wall = time.time() - t0
    try:
        j = json.load(open(jpath))
    except Exception:  # noqa: BLE001
        j = {"error": (p.stderr or p.stdout)[-300:]}
    os.unlink(jpath)
    j.update(gpu=s.gpu, wall_total_s=round(wall, 1), fmt=fmt, clip=clip, n=n,
             kind="profile" if profile else "paced" if paced else "freerun")
    return j


def brief(j):
    g = j.get("gpu", {})
    if "error" in j:
        return f"  {j['kind']:8} {j['fmt']} N={j['n']}: ERROR {j['error'][-120:]}"
    s = (f"  {j['kind']:8} {j['fmt']:5} N={j['n']:3d} rt {j['rt']:.3f} {j['mb_per_s'] / 1e6:6.3f} Mmb/s tick mean {j['tick_mean_ms']:.2f} "
         f"p99 {j['tick_p99_ms']:.2f} p99.9 {j['tick_p999_ms']:.2f} ms")
    if j["kind"] == "paced":
        s += f" on-time {100 * j['ontime_frac']:.2f}%"
    s += f" | sm {g.get('sm_mean')}% {g.get('watts_mean')} W {g.get('temp_max')} C pclk {g.get('pclk_min')} load {g.get('load_after')}"
    if g.get("contaminated"):
        s += " CONTAMINATED " + str(g.get("other_processes"))
    return s


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", required=True)
    ap.add_argument("--mode", choices=["quick", "cap", "sustain"], default="quick")
    ap.add_argument("--formats", default="1080i,720p,480i,mixed")
    ap.add_argument("--clip", default="crowd_run")
    ap.add_argument("--paced-n", default="", help="sustain mode: fmt=N,... session counts")
    ap.add_argument("--baseline", default="baseline", help="label whose cap99 is the default N of sustain mode")
    ap.add_argument("--enc-arg", action="append", default=[], help="extra encoder argument (repeatable)")
    ap.add_argument("--paced-seconds", type=float, default=0)
    a = ap.parse_args()
    os.makedirs(RES, exist_ok=True)
    extra = a.enc_arg
    runs, summary = [], {}
    paced_n = dict(x.split("=") for x in a.paced_n.split(",") if x)
    basecap = {}
    bp = os.path.join(RES, a.baseline + ".json")
    if os.path.exists(bp):
        basecap = {k: v.get("cap99") for k, v in json.load(open(bp)).get("summary", {}).items()}
    print(f"bench {a.label} mode {a.mode} formats {a.formats} clip {a.clip} extra {extra}")
    print(sh(["nvidia-smi", "--query-gpu=name,temperature.gpu,power.draw,utilization.gpu,memory.used", "--format=csv,noheader"]).strip())
    t_all = time.time()

    def rec(j):
        runs.append(j)
        print(brief(j), flush=True)
        return j

    for fmt in a.formats.split(","):
        fps = fmt_fps(fmt)
        nref = REF_N[fmt]
        sm = summary[fmt] = dict(ref_n=nref)
        if a.mode == "sustain":
            n = int(paced_n.get(fmt) or basecap.get(fmt) or 1)
            j = rec(encode(fmt, a.clip, n, int(fps * (a.paced_seconds or 180)), extra, paced=True))
            g = j.get("gpu", {})
            sm.update(paced_n=n, paced_seconds=a.paced_seconds or 180, paced_ontime=j.get("ontime_frac"),
                      paced_p99_ms=j.get("tick_p99_ms"), paced_p999_ms=j.get("tick_p999_ms"), paced_mean_ms=j.get("tick_mean_ms"),
                      watts_mean=g.get("watts_mean"), watts_max=g.get("watts_max"), temp_start=g.get("temp_start"),
                      temp_max=g.get("temp_max"), pclk_min=g.get("pclk_min"), sm_mean=g.get("sm_mean"),
                      cpu_cores=j.get("cpu_cores"), contaminated=g.get("contaminated"))
            continue
        # free-run at the reference N, three times
        fr = [rec(encode(fmt, a.clip, nref, int(fps * 5), extra)) for _ in range(3)]
        ok = [j for j in fr if "error" not in j]
        if not ok:
            continue
        mbs = [j["mb_per_s"] for j in ok]
        sm["freerun_mb_per_s"] = round(sum(mbs) / len(mbs))
        sm["freerun_spread_pct"] = round(100 * (max(mbs) - min(mbs)) / (sum(mbs) / len(mbs)), 2)
        sm["freerun_tick_ms"] = round(sum(j["tick_mean_ms"] for j in ok) / len(ok), 3)
        sm["freerun_temp_start"] = ok[0]["gpu"].get("temp_start")
        est = nref * ok[0]["period_ms"] / sm["freerun_tick_ms"]
        sm["cap_estimate_freerun"] = round(est, 1)
        pj = rec(encode(fmt, a.clip, nref, int(fps * 5), extra, profile=True))
        sm["stages_ms"] = pj.get("stages_ms")
        sm["contaminated_runs"] = sum(1 for r in runs if r["fmt"] == fmt and r.get("gpu", {}).get("contaminated"))
        if a.mode != "cap":
            continue
        # capacity: bisection with paced runs
        secs = a.paced_seconds or 25
        step = STEP[fmt]
        tested = {}

        def paced(n):
            if n not in tested:
                tested[n] = rec(encode(fmt, a.clip, n, int(fps * secs), extra, paced=True))
            return tested[n].get("ontime_frac", 0.0)

        q = lambda x: max(step, int(round(x / step)) * step)  # noqa: E731
        lo, hi = q(est * 0.80), q(est * 1.10)
        while lo > step and paced(lo) < 0.99:
            hi, lo = lo, q(lo * 0.8)
        while paced(hi) >= 0.99:
            lo, hi = hi, q(hi * 1.15) + step
        while hi - lo > step:
            mid = q((lo + hi) / 2)
            if mid in (lo, hi):
                break
            if paced(mid) >= 0.99:
                lo = mid
            else:
                hi = mid
        cap = lo
        r = tested[cap]
        g = r.get("gpu", {})
        sm.update(cap99=cap, cap99_ontime=r.get("ontime_frac"), cap99_mean_ms=r.get("tick_mean_ms"),
                  cap99_p99_ms=r.get("tick_p99_ms"), cap99_p999_ms=r.get("tick_p999_ms"), cap99_watts=g.get("watts_mean"),
                  cap99_temp_max=g.get("temp_max"), cap99_temp_start=g.get("temp_start"), cap99_sm=g.get("sm_mean"),
                  cap99_cpu_cores=r.get("cpu_cores"),
                  cap99_mb_per_s=round(cap * unit_mb_per_s(fmt)))
        # 99.9 %: walk down from the capacity (at most three more runs)
        n9 = cap
        for _ in range(3):
            if paced(n9) >= 0.999 or n9 <= step:
                break
            n9 -= step
        p9 = [n for n, t in tested.items() if t.get("ontime_frac", 0) >= 0.999 and n <= cap]
        sm["cap999"] = max(p9) if p9 else None
        sm["tested"] = {str(n): t.get("ontime_frac") for n, t in sorted(tested.items())}
        sm["contaminated_runs"] = sum(1 for r in runs if r["fmt"] == fmt and r.get("gpu", {}).get("contaminated"))
    out = dict(label=a.label, mode=a.mode, clip=a.clip, enc_args=extra, date=time.strftime("%Y-%m-%d %H:%M:%S"),
               seconds=round(time.time() - t_all), summary=summary, runs=runs)
    json.dump(out, open(os.path.join(RES, a.label + ".json"), "w"), indent=1)
    keys = ["fmt", "kind", "n", "frames", "rt", "mb_per_s", "tick_mean_ms", "tick_p50_ms", "tick_p99_ms", "tick_p999_ms",
            "tick_max_ms", "ontime_frac", "late_deadline", "cpu_cores", "mbps_per_session", "underflows", "dev_mb"]
    gkeys = ["sm_mean", "mem_mean", "watts_mean", "watts_max", "temp_start", "temp_max", "pclk_min", "load_before",
             "load_after", "contaminated"]
    stages = sorted({k for r in runs for k in (r.get("stages_ms") or {})})
    with open(os.path.join(RES, a.label + ".csv"), "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["label"] + keys + gkeys + ["stage:" + s for s in stages])
        for r in runs:
            w.writerow([a.label] + [r.get(k) for k in keys] + [r.get("gpu", {}).get(k) for k in gkeys] +
                       [(r.get("stages_ms") or {}).get(s) for s in stages])
    print(json.dumps(summary, indent=1))
    print(f"total {time.time() - t_all:.0f} s; wrote {RES}/{a.label}.json and .csv")


if __name__ == "__main__":
    main()
