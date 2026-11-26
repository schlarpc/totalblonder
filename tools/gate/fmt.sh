# Format table shared by the benchmark scripts: sets SIZE FPS GOP BITRATE VBV IL for $1.
fmt_args() {
  case "$1" in
    480i)  SIZE=720x480;   FPS=30000/1001; GOP=15; BITRATE=4000000;  VBV=1835008; IL=--interlaced ;;
    720p)  SIZE=1280x720;  FPS=60000/1001; GOP=30; BITRATE=12000000; VBV=5996544; IL= ;;
    1080i) SIZE=1920x1080; FPS=30000/1001; GOP=15; BITRATE=15000000; VBV=7487488; IL=--interlaced ;;
    *) echo "unknown format $1" >&2; return 1 ;;
  esac
}
