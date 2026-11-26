#!/usr/bin/env bash
# Prepare the raw 4:2:0 test corpus under $M2V_MEDIA (gate/env.sh; not in git). Run on nixark as
#   nix run .#gate-prep -- media
# (inside the memory cap: scripts/capped.sh). Idempotent: existing files are kept.
# Downloads (Xiph derf collection, media.xiph.org) go to $M2V_MEDIA/download/ and are only read
# by ffmpeg. Total size after this script: about 17 GB (limit agreed for nixark: 25 GB).
#
# Naming: <clip>_<format>.yuv, format in 1080i 1080p 720p 480i:
#   1080i  1920x1080 interlaced, top field first, 29.97 frames/s
#   1080p  1920x1080 progressive, 29.97 frames/s
#   720p   1280x720 progressive, 59.94 frames/s
#   480i   704x480 interlaced, 29.97 frames/s (top field first unless the clip name ends _bff).
#          704 and not 720 samples per line: ATSC A/53 Part 4 Table 6.2 allows 704 or 640 only,
#          SCTE 43 Table 3 allows 720 and 704, so 704 is legal under both.
# Interlaced clips made from progressive sources weave alternate fields from successive
# source frames (ffmpeg `interlace`, no low-pass), so every field is a different instant.
# The nominal source rate (25, 50 or 60 Hz) is ignored: frames are used one for one.
set -euo pipefail
cd "$(dirname "$0")/.."
. gate/env.sh
cd "$MEDIA"
D=download
mkdir -p "$D"
get() { [ -s "$D/$1" ] || { curl -sS -f -A curl -C - --retry 3 -o "$D/$1.part" "https://media.xiph.org/video/derf/y4m/$1" && mv "$D/$1.part" "$D/$1"; }; }
get crowd_run_1080p50.y4m          # hard: dense texture, camera motion
get pedestrian_area_1080p25.y4m    # moderate: people walking close to the camera
get park_joy_1080p50.y4m           # high-detail pan (trees, running people, film grain)
get FourPeople_1280x720_60.y4m     # talking heads, static camera
get football_422_ntsc.y4m          # native interlaced 720x486 4:2:2, sport
get mobile_calendar_422_ntsc.y4m   # native interlaced 720x486 4:2:2, slow pan over fine detail
FF="ffmpeg -hide_banner -loglevel error -y -threads 4"
IL="interlace=scan=tff:lowpass=off"
SD="crop=704:480:8:0"   # 720 samples per line are made first; 704 is the center part
# clip <name> <source file> <progressive frames to use for 720p> (interlaced formats use half as many frames)
clip() {
  local n=$1 src=$D/$2 nf=$3
  [ -s ${n}_1080i.yuv ] || $FF -i $src -frames:v $((nf / 2)) -vf "scale=1920:1080:flags=lanczos,$IL,format=yuv420p" -f rawvideo ${n}_1080i.yuv
  [ -s ${n}_720p.yuv ]  || $FF -i $src -frames:v $nf -vf "scale=1280:720:flags=lanczos,format=yuv420p" -f rawvideo ${n}_720p.yuv
  [ -s ${n}_480i.yuv ]  || $FF -i $src -frames:v $((nf / 2)) -vf "scale=720:480:flags=lanczos,$IL,$SD,format=yuv420p" -f rawvideo ${n}_480i.yuv
  # 1080p: every second source frame, so the motion per frame is that of a 25-30 Hz camera
  [ -s ${n}_1080p.yuv ] || $FF -i $src -frames:v $((nf / 2)) -vf "select=not(mod(n\,2)),scale=1920:1080:flags=lanczos,format=yuv420p" -fps_mode passthrough -f rawvideo ${n}_1080p.yuv
}
# The first three keep the lengths of the first spike (crowd_run 500 source frames, pedestrian_area 375).
[ -s crowd_run_1080i.yuv ] || $FF -i $D/crowd_run_1080p50.y4m -vf "$IL,format=yuv420p" -f rawvideo crowd_run_1080i.yuv
[ -s crowd_run_720p.yuv ]  || $FF -i $D/crowd_run_1080p50.y4m -vf "scale=1280:720:flags=lanczos,format=yuv420p" -f rawvideo crowd_run_720p.yuv
[ -s crowd_run_480i.yuv ]  || $FF -i $D/crowd_run_1080p50.y4m -vf "scale=720:480:flags=lanczos,$IL,$SD,format=yuv420p" -f rawvideo crowd_run_480i.yuv
[ -s pedestrian_area_1080i.yuv ] || $FF -i $D/pedestrian_area_1080p25.y4m -vf "$IL,format=yuv420p" -f rawvideo pedestrian_area_1080i.yuv
[ -s pedestrian_area_720p.yuv ]  || $FF -i $D/pedestrian_area_1080p25.y4m -vf "scale=1280:720:flags=lanczos,format=yuv420p" -f rawvideo pedestrian_area_720p.yuv
[ -s pedestrian_area_480i.yuv ]  || $FF -i $D/pedestrian_area_1080p25.y4m -vf "scale=720:480:flags=lanczos,$IL,$SD,format=yuv420p" -f rawvideo pedestrian_area_480i.yuv
clip crowd_run crowd_run_1080p50.y4m 300          # only the 1080p file is new
clip pedestrian_area pedestrian_area_1080p25.y4m 300
clip park_joy park_joy_1080p50.y4m 300
clip fourpeople FourPeople_1280x720_60.y4m 300
# Synthetic (the source used for the estimate in docs/SPEC.md section 14).
[ -s testsrc2_1080i.yuv ] || $FF -f lavfi -i "testsrc2=size=1920x1080:rate=60000/1001" -frames:v 300 -vf "$IL,format=yuv420p" -f rawvideo testsrc2_1080i.yuv
[ -s testsrc2_720p.yuv ]  || $FF -f lavfi -i "testsrc2=size=1280x720:rate=60000/1001" -frames:v 300 -vf format=yuv420p -f rawvideo testsrc2_720p.yuv
[ -s testsrc2_480i.yuv ]  || $FF -f lavfi -i "testsrc2=size=720x480:rate=60000/1001" -frames:v 300 -vf "$IL,$SD,format=yuv420p" -f rawvideo testsrc2_480i.yuv
# Native interlaced 525-line material: 720x486, bottom field first (ffmpeg idet: 121 of 121 BFF). Cropping 4 lines at the top and 2 at the bottom keeps the field order (_bff);
# cropping 3 and 3 moves every line to the other field, which makes it top field first.
# The 4:2:2 to 4:2:0 conversion is done per field (scale interl=1).
for n in football mobile_calendar; do
  s=$D/${n}_422_ntsc.y4m; o=${n/mobile_calendar/mobcal}
  [ -s ${o}_480i.yuv ]     || $FF -i $s -vf "crop=704:480:8:3,scale=704:480:interl=1,format=yuv420p" -f rawvideo ${o}_480i.yuv
  [ -s ${o}_bff_480i.yuv ] || $FF -i $s -vf "crop=704:480:8:4,scale=704:480:interl=1,format=yuv420p" -f rawvideo ${o}_bff_480i.yuv
done
# Derived clips: scene cuts, fade, film grain, noise, still. See gate/derive_media.py.
python3 -I "$OLDPWD/gate/derive_media.py"
for f in 1080i:1920x1080:30 1080p:1920x1080:30 720p:1280x720:60 480i:704x480:30; do
  IFS=: read fmt size r <<<"$f"
  # fade: crowd_run fades to black over 1 s, black for 0.2 s, park_joy fades in over 1 s
  if [ ! -s fade_$fmt.yuv ]; then
    n=$((r * 5 / 2))
    $FF -f rawvideo -pix_fmt yuv420p -s $size -r $r -i crowd_run_$fmt.yuv -f rawvideo -pix_fmt yuv420p -s $size -r $r -i park_joy_$fmt.yuv \
      -filter_complex "[0:v]trim=end_frame=$n,fade=t=out:s=$((n - r - r / 5)):n=$r[a];[1:v]trim=end_frame=$n,fade=t=in:s=0:n=$r[b];[a][b]concat=n=2:v=1,format=yuv420p" -f rawvideo fade_$fmt.yuv
  fi
  # grain: pedestrian_area plus temporally varying noise (film-grain-like), strength 14
  [ -s grain_$fmt.yuv ] || $FF -f rawvideo -pix_fmt yuv420p -s $size -r $r -i pedestrian_area_$fmt.yuv -frames:v $((r * 5)) -vf "noise=alls=14:allf=t+u,format=yuv420p" -f rawvideo grain_$fmt.yuv
done
du -sh .; ls -la *.yuv
