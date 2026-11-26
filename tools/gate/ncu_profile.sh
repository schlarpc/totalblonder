#!/usr/bin/env bash
# ncu_profile.sh LABEL [BINARY] [extra encoder args...]
# Hardware-counter profile of the kernels with Nsight Compute, complete metric set (--set full
# plus the warp-state sampling section). Needs root for the counters: only the profiler runs
# under sudo, inside the memory cap. Workload: 4 sessions of 1080i crowd_run, 9 frames; the 70
# launches after the first 20 cover one P, three B and the kernels of an anchor reconstruction.
# Writes $WORK/ncu-LABEL.ncu-rep (root-owned), $RES/ncu/LABEL-details.txt (text export of
# every section per launch), $RES/ncu/LABEL-raw.csv and $RES/ncu/LABEL-summary.txt.
set -euo pipefail
cd "$(dirname "$0")/../.."
. tools/gate/env.sh
label=$1; bin=${2:-$M2VENC}; shift; [ $# -gt 0 ] && shift
NCU=$(cat $WORK/ncu.path 2>/dev/null || true)
[ -x "$NCU" ] || { NCU=$(./scripts/capped.sh -m 4G nix build .#ncu --no-link --print-out-paths | tail -1)/bin/ncu; echo "$NCU" > $WORK/ncu.path; }
mkdir -p $RES/ncu
./scripts/capped.sh sudo env LD_LIBRARY_PATH=/run/opengl-driver/lib "$NCU" --set full --section PmSampling_WarpStates \
  -k regex:'^k_' --launch-skip ${SKIP:-20} --launch-count ${COUNT:-70} -o $WORK/ncu-$label -f \
  "$bin" --input $MEDIA/crowd_run_1080i.yuv --size 1920x1080 --interlaced --gop 15 --bframes 2 \
  --bitrate 15000000 --vbv 7487488 --sessions 4 --frames 9 --write 0 --max-in-frames 16 "$@" > $WORK/ncu-$label.log 2>&1 || { tail -20 $WORK/ncu-$label.log; exit 1; }
./scripts/capped.sh -m 4G "$NCU" --import $WORK/ncu-$label.ncu-rep --page details > $RES/ncu/$label-details.txt
./scripts/capped.sh -m 4G "$NCU" --import $WORK/ncu-$label.ncu-rep --page raw --csv > $RES/ncu/$label-raw.csv
./scripts/capped.sh -m 2G python3 -I tools/gate/ncu_summary.py $RES/ncu/$label-raw.csv > $RES/ncu/$label-summary.txt
wc -c $RES/ncu/$label-*
