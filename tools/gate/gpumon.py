"""Sampling of the card during a run: nvidia-smi dmon (power, temperature, SM / NVDEC / NVENC
utilization, clocks, PCIe throughput) and pmon (other processes on the card)."""
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formats as F  # noqa: E402


class Mon:
    def __init__(self, tag, own=("tb-m2venc", "tb-pipe")):
        self.tag, self.own = tag, own
        os.makedirs(F.WORK, exist_ok=True)
        self.dpath, self.ppath = os.path.join(F.WORK, f"dmon-{tag}.txt"), os.path.join(F.WORK, f"pmon-{tag}.txt")
        self.d = open(self.dpath, "w"); self.p = open(self.ppath, "w")
        self.a = subprocess.Popen(["nvidia-smi", "dmon", "-s", "pucmt", "-d", "1"], stdout=self.d, stderr=subprocess.DEVNULL)
        self.b = subprocess.Popen(["nvidia-smi", "pmon", "-s", "u", "-d", "1"], stdout=self.p, stderr=subprocess.DEVNULL)

    def stop(self, skip=3):
        for x in (self.a, self.b):
            x.terminate(); x.wait()
        self.d.close(); self.p.close()
        names, rows = [], []
        for line in open(self.dpath):
            f = line.split()
            if line.startswith("# gpu"):
                names = f[1:]
            elif not line.startswith("#") and names and len(f) == len(names):
                rows.append({n: (float(v) if v != "-" else 0.0) for n, v in zip(names, f)})
        rows = rows[skip:] if len(rows) > skip + 3 else rows
        def col(n): return round(sum(r.get(n, 0) for r in rows) / len(rows), 1) if rows else 0
        def mx(n): return max((r.get(n, 0) for r in rows), default=0)
        others = {}
        for line in open(self.ppath):
            if line.startswith("#"): continue
            f = line.split()
            if len(f) >= 9 and f[1] != "-" and not any(o in f[-1] for o in self.own) and "nvidia-smi" not in f[-1]:
                act = [x for x in f[3:9] if x not in ("-", "0")]
                if act: others[f[-1]] = others.get(f[-1], 0) + 1
        return dict(pwr=col("pwr"), temp=mx("gtemp"), sm=col("sm"), dec=col("dec"), enc=col("enc"), pclk=col("pclk"), rxpci=col("rxpci"), txpci=col("txpci"),
                    others=sorted(others), samples=len(rows))
