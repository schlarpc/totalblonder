#!/usr/bin/env bash
# nsys_pipe.sh LABEL LINEUP N   (Nsight Systems, no root)
# Kernel time table of a paced pipeline line-up (tools/gate/pipe_lineup.py line-ups), 8 s: which kernels the GPU
# time of a tick goes to, conversion against encoder. Writes $RES/nsys/LABEL-kern.txt.
set -euo pipefail
cd "$(dirname "$0")/../.."
. tools/gate/env.sh
label=$1; lineup=$2; n=$3
NSYS=$(cat $WORK/nsys.path 2>/dev/null || true)
[ -x "$NSYS" ] || { NSYS=$(./tools/gate/capped.sh -m 4G nix build .#nsys --no-link --print-out-paths | tail -1)/bin/nsys; echo "$NSYS" > $WORK/nsys.path; }
mkdir -p $RES/nsys $RES/ncu
T=$WORK/tmp
args=$(./tools/gate/capped.sh python3 -c "import sys; sys.path.insert(0, 'gate'); import pipe_lineup; print(' '.join(pipe_lineup.cmd('$lineup', $n)))")
rm -f $WORK/nsys-$label.nsys-rep
./tools/gate/capped.sh env TMPDIR=$T "$NSYS" profile -t cuda --sample=none --cpuctxsw=none -o $WORK/nsys-$label -f true \
  $M2VPIPE $args --paced --secs 8 > $WORK/nsys-$label.log 2>&1 < /dev/null || { tail $WORK/nsys-$label.log; exit 1; }
./tools/gate/capped.sh env TMPDIR=$T "$NSYS" stats -q --report cuda_gpu_kern_sum $WORK/nsys-$label.nsys-rep > $RES/nsys/$label-kern.txt 2>&1
grep -E 'ticks on|inputs:' $WORK/nsys-$label.log >> $RES/nsys/$label-kern.txt
head -24 $RES/nsys/$label-kern.txt | cut -c1-150
rm -f $WORK/nsys-$label.nsys-rep
