#!/usr/bin/env bash
# prep_inputs.sh   (nix run .#gate-prep -- inputs; inside the memory cap)
# Compressed inputs for the decode benchmark and the pipeline harness, under $M2V_MEDIA/in/ (not in
# git; about 150 MB). 10 s each, made from the raw clips of prep_media.sh; they are looped at run
# time. Rates are what such material typically has ("random stuff pushed into the server").
set -euo pipefail
cd "$(dirname "$0")/../.."
. tools/gate/env.sh
cd "$MEDIA"
O=in; mkdir -p $O
FF="ffmpeg -hide_banner -loglevel error -nostdin -y"
P=crowd_run_1080p.yuv; I=crowd_run_1080i.yuv; H=crowd_run_720p.yuv; S=crowd_run_480i.yuv
Y4M=$(ls download/crowd_run_1080p50*.y4m 2>/dev/null | head -1 || true)
raw() { echo "-f rawvideo -pix_fmt yuv420p -s $1 -r $2"; }
X264="-c:v libx264 -preset veryfast -profile:v high -bf 2 -g 60"
mk() { [ -s "$1" ] || { echo "making $1"; shift; "$@"; }; }
# H.264 progressive
mk $O/h264_1080p30.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $P $X264 -b:v 8M -maxrate 12M -bufsize 16M $O/h264_1080p30.mkv
mk $O/h264_720p60.mkv  $FF -stream_loop 1 $(raw 1280x720 60000/1001) -i $H $X264 -g 120 -b:v 6M -maxrate 9M -bufsize 12M $O/h264_720p60.mkv
if [ -n "$Y4M" ]; then
  mk $O/h264_1080p60.mkv $FF -r 60000/1001 -i "$Y4M" $X264 -g 120 -b:v 12M -maxrate 18M -bufsize 24M $O/h264_1080p60.mkv
else
  mk $O/h264_1080p60.mkv $FF -stream_loop 1 $(raw 1280x720 60000/1001) -i $H -vf scale=1920:1080:flags=bicubic $X264 -g 120 -b:v 12M -maxrate 18M -bufsize 24M $O/h264_1080p60.mkv
fi
mk $O/h264_1080p24.mkv $FF -stream_loop 1 $(raw 1920x1080 24000/1001) -i $P $X264 -g 48 -b:v 8M -maxrate 12M -bufsize 16M $O/h264_1080p24.mkv
mk $O/h264_1080p25.mkv $FF -stream_loop 1 $(raw 1920x1080 25) -i $P $X264 -g 50 -b:v 8M -maxrate 12M -bufsize 16M $O/h264_1080p25.mkv
mk $O/h264_720p50.mkv  $FF -stream_loop 1 $(raw 1280x720 50) -i $H $X264 -g 100 -b:v 6M -maxrate 9M -bufsize 12M $O/h264_720p50.mkv
mk $O/h264_854x480p30.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $P -vf scale=854:480:flags=lanczos $X264 -b:v 1500k -maxrate 2500k -bufsize 3M $O/h264_854x480p30.mkv
mk $O/h264_1366x768p30.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $P -vf scale=1366:768:flags=lanczos $X264 -b:v 3M -maxrate 5M -bufsize 6M $O/h264_1366x768p30.mkv
# H.264 interlaced (MBAFF), top field first
mk $O/h264_1080i.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $I -vf setfield=tff $X264 -flags +ildct+ilme -b:v 10M -maxrate 15M -bufsize 20M $O/h264_1080i.mkv
mk $O/h264_480i.mkv  $FF -stream_loop 1 $(raw 704x480 30000/1001) -i $S -vf setfield=tff -aspect 4:3 $X264 -flags +ildct+ilme -b:v 2500k -maxrate 4M -bufsize 5M $O/h264_480i.mkv
# HEVC
mk $O/hevc_1080p30.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $P -c:v libx265 -preset veryfast -x265-params log-level=error -b:v 5M -g 60 $O/hevc_1080p30.mkv
mk $O/hevc_2160p30.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $P -vf scale=3840:2160:flags=bicubic -c:v hevc_nvenc -preset p5 -b:v 25M -g 60 $O/hevc_2160p30.mkv
mk $O/hevc_2160p24_10.mkv $FF -stream_loop 1 $(raw 1920x1080 24000/1001) -i $P -vf scale=3840:2160:flags=bicubic,format=p010le -c:v hevc_nvenc -preset p5 -profile:v main10 -b:v 30M -g 48 $O/hevc_2160p24_10.mkv
mk $O/hevc_1080p30_10.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $P -vf format=p010le -c:v hevc_nvenc -preset p5 -profile:v main10 -b:v 6M -g 60 $O/hevc_1080p30_10.mkv
# MPEG-2
mk $O/mpeg2_1080i.ts $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $I -vf setfield=tff -c:v mpeg2video -flags +ildct+ilme -b:v 15M -maxrate 18M -bufsize 7M -g 15 -bf 2 $O/mpeg2_1080i.ts
mk $O/mpeg2_480i.ts  $FF -stream_loop 1 $(raw 704x480 30000/1001) -i $S -aspect 4:3 -vf setfield=tff -c:v mpeg2video -flags +ildct+ilme -b:v 4M -maxrate 6M -bufsize 1835k -g 15 -bf 2 $O/mpeg2_480i.ts
# AV1, VP9
mk $O/av1_1080p30.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $P -c:v av1_nvenc -preset p5 -b:v 4M -g 60 $O/av1_1080p30.mkv
mk $O/vp9_1080p30.mkv $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i $P -c:v libvpx-vp9 -deadline realtime -cpu-used 8 -row-mt 1 -b:v 5M -g 60 $O/vp9_1080p30.mkv
for f in $O/*; do
  printf '%-28s ' "$(basename $f)"
  ffprobe -hide_banner -loglevel error -select_streams v:0 -show_entries stream=codec_name,profile,pix_fmt,width,height,r_frame_rate,field_order:format=bit_rate -of csv=p=0 $f | tr '\n' ' '; echo
done
