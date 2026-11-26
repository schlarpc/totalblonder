#!/usr/bin/env bash
# contend_nsys.sh LABEL N [COMPETITOR COMMAND...]   (experiment 40)
# Nsight Systems timeline of a paced run of N sessions of 480i (4 s), alone or with one competitor
# command running next to it. Writes $RES/nsys/contend-LABEL-tick.txt (gate/nsys_tick.py)
# and the kernels' and copies' durations per tick in $RES/nsys/contend-LABEL-phase0.txt.
set -euo pipefail
cd "$(dirname "$0")/.."
. gate/env.sh
label=contend-$1; n=$2; shift 2
NSYS=$(cat $WORK/nsys.path 2>/dev/null || true)
mkdir -p $RES/nsys $RES/ncu
T=$WORK/tmp
rm -f $WORK/nsys-$label.nsys-rep $WORK/nsys-$label.sqlite
cpid=
if [ $# -gt 0 ]; then ./scripts/capped.sh timeout -s INT 25 "$@" > $WORK/nsys-$label.comp.log 2>&1 & cpid=$!; sleep 2; fi
./scripts/capped.sh env TMPDIR=$T LD_LIBRARY_PATH=/run/opengl-driver/lib "$NSYS" profile -t cuda --sample=none --cpuctxsw=none \
  -o $WORK/nsys-$label -f true $M2VENC --input $MEDIA/crowd_run_480i.yuv --size 704x480 --interlaced --gop 15 --bframes 2 \
  --bitrate 4000000 --vbv 1835008 --sessions $n --frames 120 --write 0 --max-in-frames 48 --stagger --overlap --paced --tickmark \
  > $WORK/nsys-$label.log 2>&1 || { tail $WORK/nsys-$label.log; exit 1; }
[ -n "$cpid" ] && { kill -INT $cpid 2>/dev/null || true; pkill -INT -P $cpid 2>/dev/null || true; wait $cpid 2>/dev/null || true; }
grep '^tick ms' $WORK/nsys-$label.log
./scripts/capped.sh env TMPDIR=$T "$NSYS" export -q true --type sqlite -o $WORK/nsys-$label.sqlite -f true $WORK/nsys-$label.nsys-rep
python3 -I gate/nsys_tick.py $WORK/nsys-$label.sqlite > $RES/nsys/$label-tick.txt
