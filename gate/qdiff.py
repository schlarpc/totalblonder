#!/usr/bin/env python3
"""qdiff.py [RESULT.json]: quality of a gate run ($M2V_RESULTS/check/latest.json by default) against
the stored baseline (gate/baselines), per standard case: luma PSNR and SSIM differences, their
mean and the worst case, and the mean bit rate difference. Run after
`nix run .#gate-check -- --kind std`."""
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import formats as F  # noqa: E402

RES = os.path.join(F.RESULTS, "check")
files = [a for a in sys.argv[1:] if not a.startswith("-")]
cur = json.load(open(files[0] if files else os.path.join(RES, "latest.json")))["results"]
base = json.load(open(os.path.join(F.BASELINES, "baseline.json")))
refp = os.path.join(RES, "ref.json") if os.path.exists(os.path.join(RES, "ref.json")) else os.path.join(F.BASELINES, "ref.json")
ref = json.load(open(refp)) if os.path.exists(refp) else {}
rows = []
for c in (cur.values() if isinstance(cur, dict) else cur):
    cid = c.get("id")
    if c.get("kind") != "std" or cid not in base or "psnr_y" not in c:
        continue
    b = base[cid]
    rows.append((cid, c["psnr_y"] - b["psnr_y"], c["ssim"] - b["ssim"], 100 * (c["mbps"] / b["mbps"] - 1), c.get("ok", True)))
if not rows:
    sys.exit("no standard cases in the result")
w = max(len(r[0]) for r in rows)
if "-v" in sys.argv:
    for r in rows:
        print(f"{r[0]:{w}} dPSNR-Y {r[1]:+.3f} dB  dSSIM {r[2]:+.4f}  rate {r[3]:+.2f} %")
n = len(rows)
worst = min(rows, key=lambda r: r[1])
best = max(rows, key=lambda r: r[1])
print(f"{n} cases: mean dPSNR-Y {sum(r[1] for r in rows) / n:+.3f} dB (worst {worst[1]:+.3f} {worst[0]}, best {best[1]:+.3f} {best[0]}); "
      f"mean dSSIM {sum(r[2] for r in rows) / n:+.4f} (worst {min(r[2] for r in rows):+.4f}); mean rate {sum(r[3] for r in rows) / n:+.2f} %")
