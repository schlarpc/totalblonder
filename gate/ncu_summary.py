#!/usr/bin/env python3
"""ncu_summary.py RAW.csv : per-kernel table from an Nsight Compute raw CSV export
(`ncu --import X.ncu-rep --page raw --csv`), averaged over the profiled launches of each kernel.

Three blocks are printed:
  1. time, occupancy, instruction and memory figures. Columns: ms per launch; share of the
     profiled kernel time; registers per thread; achieved occupancy (% of 48 warps per SM);
     issue slots busy; warp-level instructions executed (millions); warp cycles per issued
     instruction; threads per warp instruction and of those not predicated off (32 = no
     divergence); branches whose threads all go the same way (branch efficiency); share of
     branch instructions; active and eligible warps per scheduler; divergent branches (average
     per scheduler); warp-level global loads, global stores, local and shared memory
     instructions (millions); memory throughput (% of peak); L1 and L2 hit rates; DRAM traffic;
  2. where a warp's cycles go: average warps in each state per cycle in which an instruction is
     issued (WarpStateStats; their sum is the warp cycles per issued instruction);
  3. the same from PC sampling (share of samples per stall reason).
"""
import csv
import sys
from collections import defaultdict

rows = list(csv.reader(open(sys.argv[1])))
hdr, units, data = rows[0], rows[1], rows[2:]
col = {k: i for i, k in enumerate(hdr)}


def num(row, key):
    i = col.get(key)
    if i is None or i >= len(row):
        return None
    v = row[i].replace(",", "")
    try:
        return float(v)
    except ValueError:
        return None


by = defaultdict(list)
for r in data:
    name = r[col["Kernel Name"]].split("(")[0]
    by[name].append(r)


def avg(name, key, scale=1.0):
    v = [num(r, key) for r in by[name]]
    v = [x for x in v if x is not None]
    return scale * sum(v) / len(v) if v else None


def tot(name, key):
    v = [num(r, key) for r in by[name]]
    return sum(x for x in v if x is not None)


tunit = units[col["gpu__time_duration.sum"]]
tscale = {"ns": 1e-6, "us": 1e-3, "ms": 1.0, "s": 1e3, "nsecond": 1e-6, "usecond": 1e-3, "msecond": 1.0, "second": 1e3}.get(tunit, 1e-6)
total_ms = sum(tot(n, "gpu__time_duration.sum") for n in by) * tscale
order = sorted(by, key=lambda n: -tot(n, "gpu__time_duration.sum"))


def fmt(v, f="%.1f"):
    return "-" if v is None else f % v


def table(title, cols):
    print(title)
    w = [max(len(c[0]), 8) for c in cols]
    print("kernel".ljust(16) + " ".join(c[0].rjust(x) for c, x in zip(cols, w)))
    for n in order:
        print(n.ljust(16) + " ".join(fmt(c[1](n), c[2] if len(c) > 2 else "%.1f").rjust(x) for c, x in zip(cols, w)))
    print()


S = "smsp__average_warps_issue_stalled_%s_per_issue_active.ratio"
table(f"1. Averages per launch ({sum(len(v) for v in by.values())} launches, {total_ms:.2f} ms of kernel time in total)", [
    ("launches", lambda n: float(len(by[n])), "%.0f"),
    ("ms", lambda n: avg(n, "gpu__time_duration.sum", tscale), "%.3f"),
    ("time%", lambda n: 100 * tot(n, "gpu__time_duration.sum") * tscale / total_ms),
    ("regs", lambda n: avg(n, "launch__registers_per_thread"), "%.0f"),
    ("occ%", lambda n: avg(n, "sm__warps_active.avg.pct_of_peak_sustained_active")),
    ("issue%", lambda n: avg(n, "smsp__issue_active.avg.pct_of_peak_sustained_active")),
    ("Minst", lambda n: avg(n, "smsp__inst_executed.sum", 1e-6), "%.2f"),
    ("cyc/inst", lambda n: avg(n, "smsp__average_warp_latency_per_inst_issued.ratio"), "%.2f"),
    ("thr/warp", lambda n: avg(n, "smsp__thread_inst_executed_per_inst_executed.ratio")),
    ("pred-on", lambda n: avg(n, "smsp__thread_inst_executed_pred_on_per_inst_executed.ratio")),
    ("br-unif%", lambda n: avg(n, "smsp__sass_average_branch_targets_threads_uniform.pct")),
    ("branch%", lambda n: avg(n, "derived__smsp__inst_executed_op_branch_pct")),
    ("actW/sch", lambda n: avg(n, "smsp__warps_active.avg.per_cycle_active"), "%.2f"),
    ("eligW", lambda n: avg(n, "smsp__warps_eligible.avg.per_cycle_active"), "%.2f"),
    ("divBr", lambda n: avg(n, "smsp__sass_branch_targets_threads_divergent.avg"), "%.0f"),
    ("MgLoad", lambda n: avg(n, "sass__inst_executed_global_loads", 1e-6), "%.2f"),
    ("MgStore", lambda n: avg(n, "sass__inst_executed_global_stores", 1e-6), "%.2f"),
    ("Mlocal", lambda n: (avg(n, "sass__inst_executed_local_loads", 1e-6) or 0) + (avg(n, "sass__inst_executed_local_stores", 1e-6) or 0), "%.2f"),
    ("Mshared", lambda n: (avg(n, "sass__inst_executed_shared_loads", 1e-6) or 0) + (avg(n, "sass__inst_executed_shared_stores", 1e-6) or 0), "%.2f"),
    ("mem%", lambda n: avg(n, "gpu__compute_memory_throughput.avg.pct_of_peak_sustained_elapsed")),
    ("L1hit%", lambda n: avg(n, "l1tex__t_sector_hit_rate.pct")),
    ("L2hit%", lambda n: avg(n, "lts__t_sector_hit_rate.pct")),
    ("dramRdMB", lambda n: avg(n, "dram__bytes_read.sum", {"byte": 1e-6, "Kbyte": 1e-3, "Mbyte": 1.0, "Gbyte": 1e3}.get(units[col["dram__bytes_read.sum"]], 1e-6)), "%.2f"),
    ("dramWrMB", lambda n: avg(n, "dram__bytes_write.sum", {"byte": 1e-6, "Kbyte": 1e-3, "Mbyte": 1.0, "Gbyte": 1e3}.get(units[col["dram__bytes_write.sum"]], 1e-6)), "%.2f"),
])
states = ["long_scoreboard", "short_scoreboard", "lg_throttle", "mio_throttle", "math_pipe_throttle", "wait", "not_selected",
          "selected", "no_instruction", "branch_resolving", "barrier", "dispatch_stall", "membar", "drain", "imc_miss",
          "sleeping", "tex_throttle", "misc"]
table("2. Warp states: average warps per issue-active cycle (sum = warp cycles per issued instruction)",
      [(s.replace("_scoreboard", "_sb").replace("_throttle", "_thr").replace("branch_resolving", "br_res").replace("no_instruction", "no_inst")
        .replace("not_selected", "not_sel").replace("dispatch_stall", "dispatch"), (lambda s: lambda n: avg(n, S % s))(s), "%.2f") for s in states])


def pcs(s):
    def f(n):
        tot_s = sum(tot(n, "smsp__pcsamp_warps_issue_stalled_%s" % x) for x in states + ["no_instructions"]
                    if ("smsp__pcsamp_warps_issue_stalled_%s" % x) in col)
        k = "smsp__pcsamp_warps_issue_stalled_%s" % s
        return 100 * tot(n, k) / tot_s if tot_s and k in col else None
    return f


table("3. PC sampling: share of warp samples per state, %",
      [(s.replace("_scoreboard", "_sb").replace("_throttle", "_thr").replace("branch_resolving", "br_res").replace("no_instructions", "no_inst")
        .replace("not_selected", "not_sel").replace("dispatch_stall", "dispatch"), pcs(s)) for s in
       ["long_scoreboard", "short_scoreboard", "lg_throttle", "mio_throttle", "math_pipe_throttle", "wait", "not_selected", "selected",
        "no_instructions", "branch_resolving", "barrier", "dispatch_stall", "misc"]])
