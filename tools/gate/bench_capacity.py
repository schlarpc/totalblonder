#!/usr/bin/env python3
"""bench_capacity.py --bin NAME=PATH [--bin NAME="PATH --option ..."] [--mode free|paced|both] ...

Capacity of the encoder alone (frames from host memory, the realistic workload `--stagger
--overlap`), for one encoder binary or for several compared with each other. With one `--bin`
it is the standing benchmark of the encoder; several are compared with each other (an old and
a new build, or a build with an option).

What it does itself, so that a number can be trusted without a person watching the card:
  - binaries are run interleaved (A B A B ...), round by round, so that warming of the card
    (3 to 4 % between cold and warm) falls on all of them alike;
  - before every run it looks at `nvidia-smi pmon` and waits (up to --wait seconds) while
    another process is using the card; during the run it samples pmon and dmon (the Sampler
    of the gate's bench.py); a run next to which another process used the card (compute,
    decoder or encoder) is DISCARDED and repeated (up to --retries times; if it is still
    disturbed the result is kept, marked `contaminated`, and left out of means where a clean
    run of the same kind exists);
  - every run is short: free runs are --seconds of video (default 5), paced runs
    --paced-seconds (default 25).

Modes:
  free    macroblocks per second without pacing at the reference line-ups of bench.py (16
          sessions; 96 for 480i; 8 mixed units), --rounds runs per binary: mean, spread
          (min, max, standard deviation), tick time, host CPU, and for a binary that reports
          it the host time per tick between phases, in queueing and in waits.
  paced   on-time fraction (a tick's output exists before the next tick is due; first second
          left out) at given line-ups (--points 1080i=67,...), and around each point the
          largest count with >= 99 % (cap99) and with >= 99.9 % (cap999) of ticks on time,
          searched in steps of the format (1; 4 for 480i) within --span steps of the point.
Results: printed, and $M2V_RESULTS/bench/<label>.json with every run (the discarded ones too).
Needs the gate's environment (M2V_MEDIA); run through the capped wrapper. With nvidia-smi it
watches the card as described; without it the card is the AMD one of a desktop (bench.AmdSampler):
binaries run with `--device hip`, no run is discarded, and the desktop probe runs beside each.
"""
import argparse
import json
import math
import os
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import formats as F  # noqa: E402
import bench as B  # noqa: E402

EXTRA = ["--stagger", "--overlap"]
# Processes whose use of the card does not disturb a run (M2V_IGNORE_PROCS, comma-separated
# substrings of the pmon name): a production server that is always there falls on every run alike.
IGNORE = ["tb-m2venc"] + [x for x in os.environ.get("M2V_IGNORE_PROCS", "").split(",") if x]
POINTS = "1080i=67,1080p=75,720p=90,480i=400,mixed=24"   # the capacity the C++ spike measured (spike notes 0.1), held by the Rust encoder (G2)


def parse_pmon(text):
    """Other processes that USE the card in a `nvidia-smi pmon -s u` listing: {name: highest of
    sm, mem, enc, dec in percent}. A process that only holds a context and memory (an idle
    transcoder) costs the encoder nothing (spike notes 0.7) and is not counted."""
    busy = {}
    for line in text.splitlines():
        t = line.split()
        if line.startswith("#") or len(t) < 8 or t[1] == "-" or any(x in t[-1] for x in IGNORE):
            continue
        use = max((float(x) for x in t[3:7] if x.replace(".", "").isdigit()), default=0.0)
        if use > 0:
            busy[t[-1]] = max(busy.get(t[-1], 0.0), use)
    return busy


def others_busy():
    """Other processes using the card right now (none are counted on a card without NVIDIA's
    tools: it serves a desktop, see bench.AmdSampler)."""
    if not B.NVIDIA:
        return {}
    return parse_pmon(subprocess.run(["nvidia-smi", "pmon", "-c", "1", "-s", "u"], stdout=subprocess.PIPE, text=True).stdout)


class Watch(B.Sampler):
    """The gate's sampler (dmon: utilization, power, temperature, clock) plus a pmon of our own
    at 1 s that tells use from presence."""

    def __enter__(self):
        if B.NVIDIA:
            self.mine = subprocess.Popen(["nvidia-smi", "pmon", "-s", "u", "-d", "1"], stdout=subprocess.PIPE, text=True)
        return super().__enter__()

    def __exit__(self, *a):
        super().__exit__(*a)
        if not B.NVIDIA:
            return
        self.mine.terminate()
        self.others = parse_pmon(self.mine.communicate()[0])
        # the card's decoder and encoder are never ours in these runs
        self.dirty = bool(self.others) or (self.gpu.get("enc_max") or 0) > 0 or (self.gpu.get("dec_max") or 0) > 0
        self.gpu["other_processes"] = self.others
        self.gpu["contaminated"] = self.dirty


def wait_clean(limit, after_burst=False):
    """Wait while another process uses the card (at most `limit` seconds). `after_burst`: first
    wait for the other process to come back and finish again, so that the run starts right after
    a burst of it: a media server transcodes ahead of real time in bursts of a few seconds every
    10 to 20, and the gap after one is the longest clean window there is."""
    t0 = time.time()
    if after_burst:
        while not others_busy() and time.time() - t0 < limit / 2:
            time.sleep(1)
    while True:
        b = others_busy()
        if not b or time.time() - t0 > limit:
            return b
        time.sleep(1)


def command(binary, fmt, n, frames, paced):
    with tempfile.NamedTemporaryFile(suffix=".json", delete=False) as tf:
        jpath = tf.name
    if fmt == "mixed":
        cmd = B.mixed_cmd("crowd_run", n, frames / B.MIXED_FPS, EXTRA)
    else:
        cmd = F.enc_args(fmt, "crowd_run") + ["--sessions", str(n), "--frames", str(frames)] + EXTRA
    # a binary may carry options of its own: NAME="PATH --option ..."
    binary, *own = binary.split()
    if not B.NVIDIA and "--device" not in own:
        own += ["--device", "hip"]   # no NVIDIA tools on this machine: its card is the AMD one
    cmd = [binary] + cmd[1:] + ["--write", "0", "--max-in-frames", str(B.MAX_IN), "--json", jpath] + own
    if paced:
        cmd += ["--paced", "--warmup", str(int(B.WARM_S * B.fmt_fps(fmt)))]
    return cmd, jpath


class Runner:
    def __init__(self, a):
        self.a = a
        self.all = []

    def run(self, who, binary, fmt, n, seconds, paced):
        """One clean run (or, after the retries, a disturbed one marked so)."""
        frames = int(B.fmt_fps(fmt) * seconds)
        j = {}
        for attempt in range(self.a.retries + 1):
            before = wait_clean(self.a.wait, after_burst=attempt > 0)
            cmd, jpath = command(binary, fmt, n, frames, paced)
            with Watch() as s:
                p = subprocess.run(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                j = json.load(open(jpath))
            except Exception:  # noqa: BLE001
                j = {"error": (p.stderr or p.stdout)[-300:]}
            os.unlink(jpath)
            dirty = bool(before) or s.gpu.get("contaminated")
            j.update(who=who, fmt=fmt, n=n, kind="paced" if paced else "free", gpu=s.gpu, busy_before=before,
                     contaminated=bool(dirty), attempt=attempt)
            self.all.append(j)
            if "error" in j or not dirty:
                break
            print(f"   (discarded: {who} {fmt} x {n}: another process used the card: {before or s.gpu.get('other_processes')}"
                  f" dec {s.gpu.get('dec_max')} enc {s.gpu.get('enc_max')})", flush=True)
        return j


def desk(g):
    """What the run did to a desktop frame, when the probe ran (bench.AmdSampler)."""
    if "desk_p99_ms" not in g:
        return ""
    return f" | desktop frame p99 {g['desk_p99_ms']:.2f} max {g['desk_max_ms']:.2f} ms, {g['desk_over8']} over 8 ms"


def stats(v):
    m = sum(v) / len(v)
    sd = math.sqrt(sum((x - m) ** 2 for x in v) / (len(v) - 1)) if len(v) > 1 else 0.0
    return m, sd, min(v), max(v)


def free(r, bins, fmts, out):
    a = r.a
    for fmt in fmts:
        runs = {w: [] for w in bins}
        for _ in range(a.rounds):
            for who, binary in bins.items():
                j = r.run(who, binary, fmt, B.REF_N[fmt], a.seconds, False)
                runs[who].append(j)
                if "error" in j:
                    print(f"{fmt:6} {who:7} ERROR {j['error'][-200:]}", flush=True)
                    continue
                more = ""
                if "host_between_ms" in j:
                    more = (f" | host/tick: between {j['host_between_ms']:.3f} queue {j['host_submit_ms']:.3f}"
                            f" wait {j['host_wait_ms']:.3f} ms")
                g = j["gpu"]
                print(f"{fmt:6} {who:7} {j['mb_per_s'] / 1e6:7.3f} Mmb/s tick {j['tick_mean_ms']:.3f}/{j['tick_p99_ms']:.3f} ms cpu "
                      f"{j['cpu_cores']:.3f} {g.get('temp_start')}C {g.get('watts_mean')}W{' CONTAMINATED' if j['contaminated'] else ''}{desk(g)}{more}",
                      flush=True)
        res = {}
        for who, rs in runs.items():
            ok = [j for j in rs if "error" not in j]
            clean = [j for j in ok if not j["contaminated"]] or ok
            if not clean:
                continue
            m, sd, lo, hi = stats([j["mb_per_s"] for j in clean])
            res[who] = dict(mb_per_s=m, sd=sd, min=lo, max=hi, runs=len(clean), clean=all(not j["contaminated"] for j in clean),
                            tick_mean_ms=stats([j["tick_mean_ms"] for j in clean])[0],
                            cpu_cores=stats([j["cpu_cores"] for j in clean])[0])
            for k in ("host_between_ms", "host_submit_ms", "host_wait_ms"):
                if all(k in j for j in clean):
                    res[who][k] = stats([j[k] for j in clean])[0]
            print(f"{fmt:6} {who:7} mean {m / 1e6:.3f} Mmb/s, spread {100 * (hi - lo) / m:.2f} % (sd {100 * sd / m:.2f} %), "
                  f"{len(clean)} runs{'' if res[who]['clean'] else ' (CONTAMINATED)'}, tick {res[who]['tick_mean_ms']:.3f} ms, "
                  f"cpu {res[who]['cpu_cores']:.3f}", flush=True)
        names = list(res)
        for w in names[1:]:
            # Round by round: the two runs of a round are neighbors in time, so warming and a
            # steady neighbor on the card fall on both; the median over the rounds is the
            # figure to trust when not every run was clean.
            pairs = [(b["mb_per_s"] / a["mb_per_s"], not (a["contaminated"] or b["contaminated"]))
                     for a, b in zip(runs[names[0]], runs[w]) if "error" not in a and "error" not in b]
            line = f"{fmt:6} {w} / {names[0]}: throughput {res[w]['mb_per_s'] / res[names[0]]['mb_per_s']:.4f} (means)"
            if pairs:
                q = sorted(x for x, _ in pairs)
                med = (q[(len(q) - 1) // 2] + q[len(q) // 2]) / 2
                res[w]["paired"] = dict(median=med, min=q[0], max=q[-1], rounds=len(q), clean_rounds=sum(1 for _, c in pairs if c))
                line += f"; by round: median {med:.4f}, {q[0]:.4f} to {q[-1]:.4f}, {len(q)} rounds ({res[w]['paired']['clean_rounds']} clean)"
            print(line, flush=True)
            # The median tick of a run does not move with a few long ticks (on the AMD card a
            # run has one or two of 100 ms, when the driver evicts the process's queues): the
            # ratio of the medians over the rounds is the figure to trust there.
            p50 = lambda who: sorted(j["tick_p50_ms"] for j in runs[who] if "tick_p50_ms" in j)  # noqa: E731
            a50, b50 = p50(names[0]), p50(w)
            if a50 and b50:
                ma, mb = a50[len(a50) // 2], b50[len(b50) // 2]
                res[w]["tick_p50_ratio"] = ma / mb
                print(f"{fmt:6} {w} / {names[0]}: by median tick {ma / mb:.4f} ({mb:.3f} against {ma:.3f} ms)", flush=True)
        out.setdefault("free", {})[fmt] = res


def paced(r, bins, fmts, points, out):
    a = r.a
    for fmt in fmts:
        if fmt not in points:
            continue
        step, p0 = B.STEP[fmt], points[fmt]
        tested = {w: {} for w in bins}

        def at(n):
            # all binaries at this count, interleaved
            for who, binary in bins.items():
                if n in tested[who]:
                    continue
                j = r.run(who, binary, fmt, n, a.paced_seconds, True)
                tested[who][n] = j
                if "error" in j:
                    print(f"{fmt:6} {who:7} x {n:4} ERROR {j['error'][-200:]}", flush=True)
                    continue
                g = j["gpu"]
                print(f"{fmt:6} {who:7} x {n:4} on time {100 * j['ontime_frac']:7.3f} % tick {j['tick_mean_ms']:.3f}/"
                      f"{j['tick_p99_ms']:.3f} ms cpu {j['cpu_cores']:.3f} {g.get('temp_start')}-{g.get('temp_max')}C "
                      f"{g.get('watts_mean')}W{' CONTAMINATED' if j['contaminated'] else ''}{desk(g)}", flush=True)

        def ok(who, n, frac):
            return tested[who].get(n, {}).get("ontime_frac", 0.0) >= frac

        at(p0)
        # upwards while any binary still holds 99 %, downwards while any binary fails 99.9 %
        n = p0
        while n < p0 + a.span * step and any(ok(w, n, 0.99) for w in bins):
            n += step
            at(n)
        n = p0
        while n > max(step, p0 - a.span * step) and any(not ok(w, n, 0.999) for w in bins):
            n -= step
            at(n)
        res = {}
        for who in bins:
            t = tested[who]
            c99 = [n for n in sorted(t) if ok(who, n, 0.99)]
            c999 = [n for n in sorted(t) if ok(who, n, 0.999)]
            # the largest count that holds with every smaller tested count holding too
            def largest(c):
                best = None
                for n in sorted(t):
                    if n in c:
                        best = n
                    elif best is not None:
                        break
                return best
            res[who] = dict(cap99=largest(c99), cap999=largest(c999), point=p0,
                            tested={str(n): dict(ontime=j.get("ontime_frac"), tick_mean_ms=j.get("tick_mean_ms"),
                                                 tick_p99_ms=j.get("tick_p99_ms"), cpu=j.get("cpu_cores"),
                                                 contaminated=j.get("contaminated")) for n, j in sorted(t.items())})
            print(f"{fmt:6} {who:7} cap99 {res[who]['cap99']} cap999 {res[who]['cap999']} (tested "
                  f"{', '.join(f'{n}: {100 * (j.get('ontime_frac') or 0):.2f} %' for n, j in sorted(t.items()))})", flush=True)
        out.setdefault("paced", {})[fmt] = res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", action="append", required=True, help="NAME=PATH; the first is the reference of ratios")
    ap.add_argument("--mode", default="free", choices=["free", "paced", "both"])
    ap.add_argument("--formats", default="1080i,1080p,720p,480i,mixed")
    ap.add_argument("--seconds", type=float, default=5)
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--points", default=POINTS)
    ap.add_argument("--paced-seconds", type=float, default=25)
    ap.add_argument("--span", type=int, default=3, help="paced: steps searched on either side of the point")
    ap.add_argument("--retries", type=int, default=4)
    ap.add_argument("--wait", type=float, default=60, help="seconds to wait for the card to be free of other processes")
    ap.add_argument("--label")
    a = ap.parse_args()
    bins = dict(b.split("=", 1) for b in a.bin)
    fmts = a.formats.split(",")
    points = {k: int(v) for k, v in (x.split("=") for x in a.points.split(",") if x)}
    if B.NVIDIA:
        print(subprocess.run(["nvidia-smi", "--query-gpu=name,temperature.gpu,power.draw,power.limit,utilization.gpu", "--format=csv,noheader"],
                             stdout=subprocess.PIPE, text=True).stdout.strip(), "| others:", others_busy(), flush=True)
    else:
        print("card:", B.amd_card(), "| desktop probe:", os.environ.get("M2V_GFXPROBE") or "off", flush=True)
    r = Runner(a)
    out = dict(bins=bins, args=vars(a))
    try:
        if a.mode in ("free", "both"):
            free(r, bins, fmts, out)
        if a.mode in ("paced", "both"):
            paced(r, bins, fmts, points, out)
    finally:
        out["runs"] = r.all
        out["discarded"] = sum(1 for j in r.all if j.get("contaminated"))
        print(f"runs {len(r.all)}, of them disturbed by another process and discarded or marked: {out['discarded']}", flush=True)
        if a.label:
            res = os.path.join(F.RESULTS, "bench")
            os.makedirs(res, exist_ok=True)
            json.dump(out, open(os.path.join(res, a.label + ".json"), "w"), indent=1)
    return 0


if __name__ == "__main__":
    sys.exit(main())
