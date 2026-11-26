# Sourced by the gate's shell tools (from the repository root): where things are.
#   M2V_MEDIA    the raw clips and media/in/ (default: media/ in the tree if it exists, else
#                ~/totalblonder-spike/media, where the corpus lives on nixark)
#   M2V_WORK     scratch and results of a run (default work/gate, not in git)
#   M2V_OUT      coded streams and other scratch (default $M2V_WORK/out)
#   M2V_RESULTS  result files (default $M2V_WORK/results)
#   M2VENC, M2VPIPE, GPUCOMP   the binaries (default: on PATH; the flake's apps set them)
if [ -z "${M2V_MEDIA:-}" ]; then
  if [ -d media ]; then M2V_MEDIA=$PWD/media; else M2V_MEDIA=$HOME/totalblonder-spike/media; fi
fi
MEDIA=$M2V_MEDIA
WORK=${M2V_WORK:-$PWD/work/gate}
OUTD=${M2V_OUT:-$WORK/out}
RES=${M2V_RESULTS:-$WORK/results}
M2VENC=${M2VENC:-tb-m2venc}
M2VPIPE=${M2VPIPE:-tb-pipe}
GPUCOMP=${GPUCOMP:-gpucomp}
mkdir -p "$WORK/tmp" "$OUTD" "$RES"
