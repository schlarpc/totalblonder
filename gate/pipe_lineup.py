#!/usr/bin/env python3
"""pipe_lineup.py --label L --lineup a|a_mix|b|c|d|e|a_host|c_host [--n N | --cap LO:HI] [--secs 22] [--extra "..."]
(`nix run .#gate-lineup -- ...`, inside the memory cap)

Paced runs of the decode -> convert -> encode pipeline (tb-pipe, M2VPIPE) on a line-up of N
channels (c, c_host: N units of 1 x 1080i + 1 x 720p + 4 x 480i; e: N mosaic channels of 3 x 3):
  a       h264 1080p30 -> 1080i                       a_mix   1080-line sources of four kinds -> 1080i
  b       h264 720p60 -> 720p                         c       mixed inputs -> the mixed unit
  d       2160p / 1080p sources -> 480i (downscale)   e       3 x 3 mosaic of mixed sources -> 1080i, with overlay
  a_hyb   as a, the first 22 inputs on NVDEC and the rest decoded in software; c_hyb: c with the SD inputs decoded in software
  *_host  the comparison path: software decode + swscale on the host, frames uploaded from pinned memory
--cap LO:HI bisects the largest N that holds real time: at least 99 % of ticks on time, at most
1 % of decoded frames late for the frame clock. Every run records the card (gate/gpumon.py)
and is appended to $M2V_RESULTS/pipe/L.jsonl.
"""
import argparse, json, os, subprocess, sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formats as F  # noqa: E402
import gpumon  # noqa: E402

IN = os.path.join(F.MEDIA, "in") + "/"
HD = ["h264_1080p30.mkv", "h264_1080i.mkv", "hevc_1080p30.mkv", "mpeg2_1080i.ts"]
SD = ["h264_480i.mkv", "mpeg2_480i.ts", "h264_854x480p30.mkv", "h264_1366x768p30.mkv"]
DOWN = ["hevc_2160p30.mkv", "h264_1080p30.mkv", "h264_1080p60.mkv", "hevc_1080p30.mkv", "hevc_2160p24_10.mkv", "h264_1080i.mkv", "h264_720p60.mkv", "hevc_1080p30_10.mkv"]
MOS = ["h264_720p60.mkv", "h264_1080p30.mkv", "h264_480i.mkv", "hevc_1080p30.mkv", "h264_854x480p30.mkv", "h264_1080i.mkv", "mpeg2_480i.ts", "h264_1366x768p30.mkv", "h264_1080p24.mkv"]


def cmd(lineup, n, opt=""):
    a = []
    host = lineup.endswith("_host")
    sfx = "@sw" if host else (("@" + opt) if opt else "")
    base = lineup.replace("_host", "")
    def inp(f): a.extend(["--in", IN + f + sfx])
    if base == "a":
        a += ["--in", f"{IN}h264_1080p30.mkv*{n}{sfx}", "--ch", f"fmt=1080i,src=0,n={n}"]
    elif base == "a_hyb":   # 22 inputs on NVDEC (80 % of it for this kind), the rest decoded in software
        k = min(n, 22)
        a += ["--in", f"{IN}h264_1080p30.mkv*{k}"]
        if n > k: a += ["--in", f"{IN}h264_1080p30.mkv*{n - k}@sw"]
        a += ["--ch", f"fmt=1080i,src=0,n={n}"]
    elif base == "a_mix":
        for k in range(n): inp(HD[k % 4])
        a += ["--ch", f"fmt=1080i,src=0,n={n}"]
    elif base == "b":
        a += ["--in", f"{IN}h264_720p60.mkv*{n}{sfx}", "--ch", f"fmt=720p,src=0,n={n}"]
    elif base == "c":
        for u in range(n):
            inp(HD[u % 4]); inp("h264_720p60.mkv")
            for f in SD: inp(f)
        a += ["--ch", f"fmt=1080i,src=0,n={n},step=6", "--ch", f"fmt=720p,src=1,n={n},step=6", "--ch", f"fmt=480i,src=2,n={4 * n},grp=4,per=6" + (",fit=stretch" if host else "")]
    elif base == "c_hyb":   # the mixed unit with its four SD inputs decoded in software (they are the cheapest for the CPU)
        for u in range(n):
            inp(HD[u % 4]); inp("h264_720p60.mkv")
            for f in SD: a.extend(["--in", IN + f + "@sw"])
        a += ["--ch", f"fmt=1080i,src=0,n={n},step=6", "--ch", f"fmt=720p,src=1,n={n},step=6", "--ch", f"fmt=480i,src=2,n={4 * n},grp=4,per=6,fit=stretch"]
    elif base == "d":
        for k in range(n): inp(DOWN[k % 8])
        a += ["--ch", f"fmt=480i,src=0,n={n}"]
    elif base == "e":
        for k in range(9 * n): inp(MOS[k % 9])
        a += ["--ch", f"fmt=1080i,src=0,n={n},mosaic=3x3,ov=1"]
    else:
        raise SystemExit("unknown line-up")
    return a


def run(label, lineup, n, secs, extra, opt=""):
    os.makedirs(os.path.join(F.RESULTS, "pipe"), exist_ok=True)
    tmp = os.path.join(F.WORK, "pipe-one.json")
    if os.path.exists(tmp): os.remove(tmp)
    m = gpumon.Mon("pipe")
    os.makedirs(F.WORK, exist_ok=True)
    c = [F.M2VPIPE] + cmd(lineup, n, opt) + ["--paced", "--secs", str(secs), "--json", tmp] + extra
    p = subprocess.run(c, capture_output=True, text=True)
    g = m.stop()
    if not os.path.exists(tmp):
        print("FAILED n=%d: %s" % (n, (p.stderr or p.stdout)[-300:])); return None
    j = json.load(open(tmp))
    j.update(lineup=lineup, n=n, label=label, extra=" ".join(extra), opt=opt, gpu=g)
    j["late_frac"] = j["dec_late"] / max(1, j["dec_n"])
    j["ok99"] = j["ontime_frac"] >= 0.99 and j["late_frac"] <= 0.01
    j["ok999"] = j["ontime_frac"] >= 0.999 and j["late_frac"] <= 0.01
    with open(os.path.join(F.RESULTS, "pipe", f"{label}.jsonl"), "a") as o: o.write(json.dumps(j) + "\n")
    print(f"{lineup:7s} n={n:3d} ch={j['channels']:3d} in={j['inputs']:3d} on time {100 * j['ontime_frac']:6.2f}%  tick {j['tick_mean_ms']:6.2f}/{j['tick_p99_ms']:6.2f} ms  "
          f"dec late {100 * j['late_frac']:5.2f}% (max {j['dec_late_max_ms']:.0f} ms)  rep {j['again']} drop {j['drops']} slate {j['slates']}  cpu {j['cpu_cores']:.2f} (tick {j['cpu_tick']:.2f} dec {j['cpu_decode']:.2f})  "
          f"NVDEC {g['dec']:.0f}% SM {g['sm']:.0f}% {g['pwr']:.0f} W {g['temp']:.0f} C pci rx {g['rxpci']:.0f} tx {g['txpci']:.0f} MB/s  ctx {j['ctx_mb']:.0f} MB "
          f"{'OK' if j['ok99'] else 'no'}{' CONTAMINATED ' + ','.join(g['others']) if g['others'] else ''}", flush=True)
    return j


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--label", default="lineup")
    ap.add_argument("--lineup", required=True)
    ap.add_argument("--n", type=int, default=0)
    ap.add_argument("--cap", default="")
    ap.add_argument("--secs", type=float, default=22)
    ap.add_argument("--extra", default="")
    ap.add_argument("--opt", default="")
    a = ap.parse_args()
    extra = a.extra.split()
    if a.n: run(a.label, a.lineup, a.n, a.secs, extra, a.opt)
    if a.cap:
        lo, hi = map(int, a.cap.split(":"))
        best = 0
        # lo is assumed to hold and is tested first; hi to fail
        j = run(a.label, a.lineup, lo, a.secs, extra, a.opt)
        if j and j["ok99"]:
            best = lo
            while hi - lo > 1:
                mid = (lo + hi + 1) // 2
                if mid == hi: mid = hi - 1
                j = run(a.label, a.lineup, mid, a.secs, extra, a.opt)
                if j and j["ok99"]: lo = best = mid
                else: hi = mid
        print(f"{a.lineup}: cap99 = {best}")
