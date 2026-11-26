#!/usr/bin/env bash
# contend.sh LABEL FORMAT N COMPETITOR...   (nix run .#gate-contend -- ...; experiment 40 of the optlog)
# One paced encoder run of N sessions (FORMAT mixed: N units) with the per-tick log; during it the named competitors run
# one after the other on the same GPU, SEG seconds each (default 10) with GAP seconds (default 3)
# of the encoder alone before, between and after. nvidia-smi dmon/pmon (1 s) and query-gpu
# (250 ms) sample the card. Output: $M2V_RESULTS/contend/LABEL/ (ticks.txt, events.txt, dmon.txt,
# pmon.txt, gq.csv, one log per competitor). Evaluate with gate/contend_report.py.
# Competitor media: $M2V_OUT/contend/h264.mkv (1080p H.264) and hevc10.mkv (2160p HEVC 10-bit PQ),
# made by `contend.sh prep`. A competitor A+B+C runs those commands at the same time. M2VENC / M2VPIPE: the encoder and the pipeline (tb-m2venc, tb-pipe; gate/env.sh), GPUCOMP: the trivial-kernel competitor (gate/gpucomp.cu); ENC_ARGS: extra encoder options.
set -euo pipefail
cd "$(dirname "$0")/.."
. gate/env.sh
M=$OUTD/contend; H=$M/h264.mkv; X=$M/hevc10.mkv; RAW=$MEDIA/crowd_run_1080p.yuv
FF="ffmpeg -hide_banner -loglevel error -stats -nostdin"
if [ "$1" = prep ]; then
  mkdir -p $M
  $FF -y -stream_loop 11 -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 24000/1001 -i $RAW -c:v h264_nvenc -preset p5 -b:v 10M -g 48 -bf 2 $H
  $FF -y -stream_loop 3 -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 24000/1001 -i $RAW -vf scale=3840:2160:flags=fast_bilinear,format=p010le \
    -c:v hevc_nvenc -preset p5 -profile:v main10 -b:v 30M -g 48 -color_primaries bt2020 -color_trc smpte2084 -colorspace bt2020nc $X
  ffprobe -hide_banner -loglevel error -show_entries stream=codec_name,profile,pix_fmt,width,height,r_frame_rate,nb_read_frames,color_transfer -count_frames -of csv $H $X
  exit 0
fi
label=$1; fmt=$2; n=$3; shift 3
SEG=${SEG:-10}; GAP=${GAP:-3}
D=$RES/contend/$label; mkdir -p $D; rm -f $D/*
. gate/fmt.sh
case $fmt in
  alone) FPS=30 ;;
  pipe:*) FPS=60000/1001 ;;   # FORMAT pipe:LINEUP runs the decode -> convert -> encode pipeline (gate/pipe_lineup.py line-ups)
  mixed) FPS=60000/1001 ;;
  *) fmt_args $fmt; [ $fmt = 480i ] && SIZE=704x480 ;;
esac
DEC="-hwaccel cuda -hwaccel_output_format cuda"
comp() {  # the command of one competitor
  case $1 in
    dec_h264_re)   echo "$FF -re -stream_loop -1 $DEC -i $H -an -f null -" ;;
    dec_h264_max)  echo "$FF -stream_loop -1 $DEC -i $H -an -f null -" ;;
    dec_hevc_re)   echo "$FF -re -stream_loop -1 $DEC -i $X -an -f null -" ;;
    dec_hevc_max)  echo "$FF -stream_loop -1 $DEC -i $X -an -f null -" ;;
    dec_h264_max_nocopy) echo "$FF -stream_loop -1 $DEC -hwaccel_flags unsafe_output -i $H -an -f null -" ;;
    dec_hevc_max_nocopy) echo "$FF -stream_loop -1 $DEC -hwaccel_flags unsafe_output -i $X -an -f null -" ;;
    decdl_re)      echo "$FF -re -stream_loop -1 -hwaccel cuda -i $H -an -f null -" ;;
    decdl_max)     echo "$FF -stream_loop -1 -hwaccel cuda -i $H -an -f null -" ;;
    xcode_re)      echo "$FF -re -stream_loop -1 $DEC -i $H -an -c:v h264_nvenc -b:v 4M -f null -" ;;
    xcode_max)     echo "$FF -stream_loop -1 $DEC -i $H -an -c:v h264_nvenc -b:v 4M -f null -" ;;
    scale_re)      echo "$FF -re -stream_loop -1 $DEC -i $H -an -vf scale_cuda=w=720:h=404:format=nv12 -c:v h264_nvenc -b:v 1297k -f null -" ;;
    scale_max)     echo "$FF -stream_loop -1 $DEC -i $H -an -vf scale_cuda=w=720:h=404:format=nv12 -c:v h264_nvenc -b:v 1297k -f null -" ;;
    x4k_re)        echo "$FF -re -stream_loop -1 $DEC -i $X -an -vf scale_cuda=w=1920:h=1080:format=nv12 -c:v h264_nvenc -b:v 8M -f null -" ;;
    x4k_max)       echo "$FF -stream_loop -1 $DEC -i $X -an -vf scale_cuda=w=1920:h=1080:format=nv12 -c:v h264_nvenc -b:v 8M -f null -" ;;
    tonemap_re)    echo "$FF -re -stream_loop -1 -init_hw_device opencl=ocl -filter_hw_device ocl -hwaccel cuda -i $X -an -vf format=p010,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc,hwupload,tonemap_opencl=tonemap=hable:format=nv12:t=bt709:m=bt709:p=bt709,hwdownload,format=nv12 -c:v h264_nvenc -b:v 8M -f null -" ;;
    tonemap_max)   echo "$FF -stream_loop -1 -init_hw_device opencl=ocl -filter_hw_device ocl -hwaccel cuda -i $X -an -vf format=p010,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc,hwupload,tonemap_opencl=tonemap=hable:format=nv12:t=bt709:m=bt709:p=bt709,hwdownload,format=nv12 -c:v h264_nvenc -b:v 8M -f null -" ;;
    nvenc_re)      echo "$FF -re -stream_loop -1 -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i $RAW -c:v h264_nvenc -b:v 4M -f null -" ;;
    nvenc_max)     echo "$FF -stream_loop -1 -f rawvideo -pix_fmt yuv420p -s 1920x1080 -r 30 -i $RAW -c:v h264_nvenc -b:v 4M -f null -" ;;
    cuda_idle)     echo "$GPUCOMP --idle --secs $((SEG - 1))" ;;
    cuda_tiny60)   echo "$GPUCOMP --hz 60 --iters 2000 --secs $((SEG - 1))" ;;
    cuda_1ms60)    echo "$GPUCOMP --hz 60 --iters ${IT1MS:-400000} --secs $((SEG - 1))" ;;
    cuda_4ms60)    echo "$GPUCOMP --hz 60 --iters $((4 * ${IT1MS:-400000})) --secs $((SEG - 1))" ;;
    cuda_cont)     echo "$GPUCOMP --hz 0 --iters ${IT1MS:-400000} --secs $((SEG - 1))" ;;
    cuda_tinycont) echo "$GPUCOMP --hz 0 --iters 2000 --secs $((SEG - 1))" ;;
    cuda_copy60)   echo "$GPUCOMP --hz 60 --iters 0 --copy 33554432 --secs $((SEG - 1))" ;;
    *) echo "unknown competitor $1" >&2; return 1 ;;
  esac
}
for c in "$@"; do for p in ${c//+/ }; do comp $p > /dev/null; done; done
if [ $fmt = alone ]; then   # contend.sh LABEL alone 0 COMPETITOR...: the competitors without the encoder (their own rates)
  for c in "$@"; do
    pids=(); k=0
    for p in ${c//+/ }; do k=$((k + 1)); timeout -s INT $SEG $(comp $p) > $D/comp-$c.$k.log 2>&1 & pids+=($!); done
    wait "${pids[@]}" || true
    printf '%-40s' "$c"; for f in $D/comp-$c.*.log; do tr '\r' '\n' < $f | grep -o 'fps= *[0-9.]*' | tail -1 | tr '\n' ' '; done; echo
    sleep 2
  done
  exit 0
fi
fps=$(python3 -c "print($FPS)"); total=$((GAP + 3 + $# * (SEG + GAP)))
frames=$(python3 -c "print(int($total * $fps))")
nvidia-smi dmon -s pucvmet -o T -d 1 > $D/dmon.txt & s1=$!
nvidia-smi pmon -s um -o T -d 1 > $D/pmon.txt & s2=$!
nvidia-smi --query-gpu=timestamp,power.draw,clocks.gr,temperature.gpu,utilization.gpu,utilization.memory,clocks_event_reasons.active --format=csv,nounits -lms 250 > $D/gq.csv & s3=$!
trap 'kill $s1 $s2 $s3 2>/dev/null; wait $s1 $s2 $s3 2>/dev/null || true' EXIT
if [[ $fmt == pipe:* ]]; then args=
elif [ $fmt = mixed ]; then   # N units of 1 x 1080i + 1 x 720p + 4 x 480i (gate/bench.py)
  args=$(python3 -c "import sys; sys.path.insert(0, 'scripts'); import bench; print(' '.join(bench.mixed_cmd('crowd_run', $n, $total, ['--stagger', '--overlap'])[1:]))")
else
  args="--input $MEDIA/crowd_run_${fmt}.yuv --size $SIZE --fps $FPS $IL --gop $GOP --bframes 2 --bitrate $BITRATE --vbv $VBV --sessions $n --frames $frames --stagger --overlap"
fi
if [[ $fmt == pipe:* ]]; then
  pargs=$(python3 -c "import sys; sys.path.insert(0, 'scripts'); import pipe_lineup; print(' '.join(pipe_lineup.cmd('${fmt#pipe:}', $n)))")
  $M2VPIPE $pargs --paced --secs $total --ticklog $D/ticks.txt --json $D/pipe.json ${ENC_ARGS:-} > $D/enc.txt 2>&1 & enc=$!
else
$M2VENC $args --write 0 --max-in-frames 48 --paced --warmup 30 --ticklog $D/ticks.txt ${ENC_ARGS:-} > $D/enc.txt 2>&1 & enc=$!
fi
while [ ! -e $D/ticks.txt ]; do sleep 0.02; kill -0 $enc 2>/dev/null || { cat $D/enc.txt; exit 1; }; done
echo "T0 $(date +%s.%N) period $(python3 -c "print(1/($FPS))") fmt $fmt n $n" > $D/events.txt
sleep $((GAP + 3))
for c in "$@"; do
  a=$(date +%s.%N); pids=(); k=0
  for p in ${c//+/ }; do   # A+B+C: the parts run at the same time, each its own process and context
    k=$((k + 1)); cmd=$(comp $p)
    timeout -s INT $SEG $cmd > $D/comp-$c.$k.log 2>&1 & pids+=($!)
  done
  wait "${pids[@]}" || true
  if [ $k = 1 ]; then mv $D/comp-$c.1.log $D/comp-$c.log; else cat $D/comp-$c.*.log > $D/comp-$c.log; fi
  echo "$c $a $(date +%s.%N)" >> $D/events.txt
  sleep $GAP
done
wait $enc || true
grep -v '^session' $D/enc.txt | tail -4
