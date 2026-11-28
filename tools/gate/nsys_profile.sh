#!/usr/bin/env bash
# nsys_profile.sh LABEL [BINARY] [extra encoder args...]
# Timeline of the CUDA API calls and GPU activities (Nsight Systems, no root needed) for a short
# free-running encode: 16 sessions of 1080i crowd_run, 45 frames. Writes $WORK/nsys-LABEL.nsys-rep
# With MIXED=N the workload is the mixed line-up of the benchmark instead (N units of 1 x 1080i +
# 1 x 720p + 4 x 480i, one second). Writes $WORK/nsys-LABEL.sqlite, $RES/nsys/LABEL-summary.txt (nsys' own kernel, API and memory-operation tables)
# and $RES/nsys/LABEL-tick.txt (tools/gate/nsys_tick.py: what the GPU does within one tick).
set -euo pipefail
cd "$(dirname "$0")/../.."
. tools/gate/env.sh
label=$1; bin=${2:-$M2VENC}; shift; [ $# -gt 0 ] && shift
NSYS=$(cat $WORK/nsys.path 2>/dev/null || true)
[ -x "$NSYS" ] || { NSYS=$(./tools/gate/capped.sh -m 4G nix build .#nsys --no-link --print-out-paths | tail -1)/bin/nsys; echo "$NSYS" > $WORK/nsys.path; }
mkdir -p $RES/nsys $RES/ncu
T=$WORK/tmp
rm -f $WORK/nsys-$label.nsys-rep $WORK/nsys-$label.sqlite
if [ -n "${MIXED:-}" ]; then
  work=(--input $MEDIA/crowd_run_1080i.yuv --size 1920x1080 --interlaced --gop 15 --bframes 2 --bitrate 15000000 --vbv 7487488 --sessions $MIXED --frames 30 --stagger
        --group --input $MEDIA/crowd_run_720p.yuv --size 1280x720 --fps 60000/1001 --gop 30 --bframes 2 --bitrate 12000000 --vbv 5996544 --sessions $MIXED --frames 60 --stagger
        --group --input $MEDIA/crowd_run_480i.yuv --size 704x480 --interlaced --gop 15 --bframes 2 --bitrate 4000000 --vbv 1835008 --sessions $((4 * MIXED)) --frames 30 --stagger
        --write 0 --max-in-frames 16)
else
  work=(--input $MEDIA/crowd_run_1080i.yuv --size 1920x1080 --interlaced --gop 15 --bframes 2
        --bitrate 15000000 --vbv 7487488 --sessions 16 --frames 45 --write 0 --max-in-frames 16)
fi
./tools/gate/capped.sh env TMPDIR=$T LD_LIBRARY_PATH=/run/opengl-driver/lib "$NSYS" profile -t cuda --sample=none --cpuctxsw=none \
  -o $WORK/nsys-$label -f true "$bin" "${work[@]}" --tickmark "$@" > $WORK/nsys-$label.log 2>&1 || { tail $WORK/nsys-$label.log; exit 1; }
./tools/gate/capped.sh env TMPDIR=$T "$NSYS" stats -q --report cuda_gpu_kern_sum --report cuda_api_sum --report cuda_gpu_mem_time_sum \
  $WORK/nsys-$label.nsys-rep > $RES/nsys/$label-summary.txt 2>&1
./tools/gate/capped.sh env TMPDIR=$T "$NSYS" export -q true --type sqlite -o $WORK/nsys-$label.sqlite -f true $WORK/nsys-$label.nsys-rep
python3 -I tools/gate/nsys_tick.py $WORK/nsys-$label.sqlite > $RES/nsys/$label-tick.txt
tail -30 $RES/nsys/$label-tick.txt
