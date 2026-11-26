#!/usr/bin/env bash
# prep_film.sh   (nix run .#gate-prep -- film; inside the memory cap)
# Film-like material for the film-mode gate and measurements (gate/film_gate.py), derived from
# the corpus of prep_media.sh and written under $M2V_FILM (default $M2V_WORK/film; never into
# the corpus itself; about 2.5 GB):
#
#   <clip>_film480.yuv, <clip>_film720.yuv   the frames of <clip>_1080p.yuv (150 progressive
#       frames each, from the Xiph 1080p50 clips) scaled to 704x480 and 1280x720 with Lanczos:
#       the same film frames at the three picture sizes (1080: the corpus clip itself). The gate
#       shows each frame for three and two fields, so the motion is that of 25 Hz material
#       played at 23.976 Hz, 4 % slow, as a film transferred to 25 Hz would be played back.
#   in/<clip>_2398.mkv     the 1080p frames as H.264 at 24000/1001 Hz, 20 Mbit/s (two passes of
#       the clip: 12.5 s), the pipeline's film inputs
#   in/pedestrian_area_24.mkv, _25.mkv, _2997.mkv   the same frames labeled 24.000, 25 and
#       29.97 Hz (cadences that shift, and progressive video)
#   in/cc_2398.ts          1080p23.976 MPEG-2 with caption user data in every picture, made by
#       the encoder under test (`--cc`: a CEA-608 pair per field and a DTVCC pair per frame) and
#       wrapped by ffmpeg: a film source with captions for the pipeline
set -euo pipefail
cd "$(dirname "$0")/.."
. gate/env.sh
FILM=${M2V_FILM:-$WORK/film}
mkdir -p "$FILM/in"
FF="ffmpeg -hide_banner -loglevel error -nostdin -y"
raw() { echo "-f rawvideo -pix_fmt yuv420p -s $1 -r $2"; }
X264="-c:v libx264 -preset veryfast -profile:v high -bf 2"
mk() { [ -s "$1" ] || { echo "making $1"; shift; "$@"; }; }
for c in pedestrian_area crowd_run park_joy cuts grain; do
  P=$MEDIA/${c}_1080p.yuv
  mk "$FILM/${c}_film480.yuv" $FF $(raw 1920x1080 24) -i "$P" -vf scale=704:480:flags=lanczos -f rawvideo -pix_fmt yuv420p "$FILM/${c}_film480.yuv"
  mk "$FILM/${c}_film720.yuv" $FF $(raw 1920x1080 24) -i "$P" -vf scale=1280:720:flags=lanczos -f rawvideo -pix_fmt yuv420p "$FILM/${c}_film720.yuv"
  mk "$FILM/in/${c}_2398.mkv" $FF -stream_loop 1 $(raw 1920x1080 24000/1001) -i "$P" $X264 -g 48 -b:v 20M -maxrate 30M -bufsize 40M "$FILM/in/${c}_2398.mkv"
done
P=$MEDIA/pedestrian_area_1080p.yuv
mk "$FILM/in/pedestrian_area_24.mkv" $FF -stream_loop 1 $(raw 1920x1080 24) -i "$P" $X264 -g 48 -b:v 20M -maxrate 30M -bufsize 40M "$FILM/in/pedestrian_area_24.mkv"
mk "$FILM/in/pedestrian_area_25.mkv" $FF -stream_loop 1 $(raw 1920x1080 25) -i "$P" $X264 -g 50 -b:v 20M -maxrate 30M -bufsize 40M "$FILM/in/pedestrian_area_25.mkv"
mk "$FILM/in/pedestrian_area_2997.mkv" $FF -stream_loop 1 $(raw 1920x1080 30000/1001) -i "$P" $X264 -g 60 -b:v 20M -maxrate 30M -bufsize 40M "$FILM/in/pedestrian_area_2997.mkv"
if [ ! -s "$FILM/in/cc_2398.ts" ]; then
  echo "making $FILM/in/cc_2398.ts"
  "$M2VENC" --input "$P" --size 1920x1080 --fps 24000/1001 --gop 12 --bframes 2 --bitrate 18000000 --vbv 7487488 --cc \
    --output "$FILM/in/cc_2398" > /dev/null
  # (an elementary stream has no time stamps: ffmpeg makes them from the frame rate)
  $FF -fflags +genpts -f mpegvideo -framerate 24000/1001 -i "$FILM/in/cc_2398.m2v" -c copy -f mpegts "$FILM/in/cc_2398.ts"
  rm -f "$FILM/in/cc_2398.m2v"
fi
ls -la "$FILM" "$FILM/in"
