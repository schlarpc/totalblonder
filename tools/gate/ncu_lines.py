#!/usr/bin/env python3
"""ncu_lines.py LISTING SASS.csv KERNEL [N] : where a kernel's warps wait, per source line.

Nsight Compute samples warp states per instruction, but its CSV export of the source page
carries the numbers only in the SASS view. This joins that view with the line table of the
device image:

  LISTING   `nvdisasm -g -c tb_kernels.cubin` (nix build .#nvdisasm, .#kernels-device): the
            image built from the same sources as the profiled binary
  SASS.csv  `ncu --import X.ncu-rep --page source --print-source sass --csv -k regex:'^KERNEL$'`
            of a report from tools/gate/ncu_profile.sh (every profiled launch of the kernel)

Printed, for the N lines with the most samples (default 25): the line's share of the kernel's
warp samples and of its executed instructions; of the line's samples, the percentage waiting
for memory (long scoreboard), throttled on the memory queue (lg), waiting for a fixed-latency
result, throttled on the math pipe, and issuing; the 32-byte sectors its loads and stores
asked for per launch (thousands) and the fewest that would hold the same bytes. A line whose
sectors are several times the ideal reads or writes scattered memory; lines of an inlined
function are counted where the function is defined.
"""
import collections
import csv
import os
import re
import sys

KERNELS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "crates", "tb-kernel-abi", "kernels")
sass, ncsv, kern = sys.argv[1:4]
topn = int(sys.argv[4]) if len(sys.argv) > 4 else 25
# nvdisasm -g: map instruction ordinal -> (file,line) for the function
lines = open(sass, errors="replace").read().split("\n")
start = next(i for i, l in enumerate(lines) if l.startswith("//---") and re.search(r"\.text\._Z\d+%s\d" % kern, l))
cur = None; ordmap = []
for l in lines[start + 1:]:
    if l.startswith("//---"): break
    m = re.search(r'//## File "([^"]+)", line (\d+)(.*)', l)
    if m:
        cur = (m.group(1).split("/")[-1], int(m.group(2)), m.group(3).strip()); continue
    if re.match(r"\s+/\*[0-9a-f]{4,}\*/\s+\S", l):
        ordmap.append(cur)
rows = list(csv.reader(open(ncsv)))
blocks = []; b = None
for r in rows:
    if r and r[0] == "Kernel Name": b = None; continue
    if r and r[0] == "Address": hdr = r; b = []; blocks.append(b); continue
    if b is not None and r: b.append(r)
H = {k: i for i, k in enumerate(hdr)}
agg = collections.defaultdict(lambda: collections.Counter())
tot = collections.Counter()
for b in blocks:
    if len(b) != len(ordmap):
        print("instr count mismatch", len(b), len(ordmap), file=sys.stderr)
    for i, r in enumerate(b):
        key = ordmap[i][:2] if i < len(ordmap) and ordmap[i] else ("?", 0)
        def g(k):
            try: return float(r[H[k]])
            except Exception: return 0.0
        a = agg[key]
        for k in ("# Samples", "Instructions Executed", "stall_long_sb", "stall_lg", "stall_wait", "stall_math", "stall_selected", "stall_not_selected", "stall_branch_resolving", "stall_barrier", "L2 Theoretical Sectors Global", "L2 Theoretical Sectors Global Ideal"):
            a[k] += g(k); tot[k] += g(k)
print(f"{kern}: {len(blocks)} launches, {len(ordmap)} instr, samples {tot['# Samples']:.0f}, Minst {tot['Instructions Executed']/1e6/len(blocks):.2f}/launch")
src = {}
print(f"{'file:line':20} {'samp%':>6} {'inst%':>6} {'long_sb':>7} {'lg':>5} {'wait':>5} {'math':>5} {'sel':>5} {'sect':>8} {'ideal':>8}  source")
for key, a in sorted(agg.items(), key=lambda kv: -kv[1]["# Samples"])[:topn]:
    f, ln = key
    if f not in src:
        try: src[f] = open(os.path.join(KERNELS, f)).read().split("\n")
        except OSError: src[f] = []
    text = src[f][ln - 1].strip()[:110] if 0 < ln <= len(src[f]) else ""
    s = a["# Samples"] or 1
    print(f"{f + ':' + str(ln):20} {100*a['# Samples']/tot['# Samples']:6.1f} {100*a['Instructions Executed']/tot['Instructions Executed']:6.1f} {100*a['stall_long_sb']/s:7.0f} {100*a['stall_lg']/s:5.0f} {100*a['stall_wait']/s:5.0f} {100*a['stall_math']/s:5.0f} {100*a['stall_selected']/s:5.0f} {a['L2 Theoretical Sectors Global']/len(blocks)/1e3:8.0f} {a['L2 Theoretical Sectors Global Ideal']/len(blocks)/1e3:8.0f}  {text}")
