#!/usr/bin/env bash
# Builds the validation streams of the checker from independent muxers:
# ffmpeg's mpegts muxer (CBR, VBR, flag variants) and an ATSC MPTS with PSIP made by
# TSDuck's inject plugin (the spike of docs/notes/software-survey.md section 2.6, to a file).
#
# usage: make_streams.sh OUTDIR [SECONDS]
# needs: ffmpeg on PATH; TSDuck's tsp (TSDUCK=/path/to/tsduck/bin, or found with
#        nix build --inputs-from <repo> nixpkgs#tsduck); python3.
set -euo pipefail
out=${1:?usage: make_streams.sh OUTDIR [SECONDS]}
secs=${2:-20}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
if [ -z "${TSDUCK:-}" ]; then
  repo=$(cd "$here/../../.." && pwd)
  TSDUCK=$(nix build --no-link --print-out-paths --inputs-from "$repo" nixpkgs#tsduck)/bin
fi
tsp="$TSDUCK/tsp"

src=(-f lavfi -i "testsrc2=size=1280x720:rate=60000/1001"
     -f lavfi -i "sine=frequency=1000:sample_rate=48000"
     -f lavfi -i "smptehdbars=size=1280x720:rate=60000/1001"
     -f lavfi -i "anullsrc=channel_layout=stereo:sample_rate=48000"
     -map 0:v -map 1:a -map 2:v -map 3:a -t "$secs")
enc=(-c:v mpeg2video -pix_fmt yuv420p -g 30 -bf 2
     -b:v 8M -minrate 8M -maxrate 8M -bufsize 7340032
     -c:a ac3 -b:a 192k -ac 2
     -program program_num=1:title=ONE:st=0:st=1
     -program program_num=2:title=TWO:st=2:st=3
     -streamid 0:0x101 -streamid 1:0x104 -streamid 2:0x201 -streamid 3:0x204)
ff() { ffmpeg -hide_banner -loglevel error -y "${src[@]}" "${enc[@]}" "$@"; }

# (1) CBR at 38.8 Mbit/s, the project's carrier rate, with the survey's settings.
ff -f mpegts -mpegts_transport_stream_id 0x0101 -mpegts_pmt_start_pid 0x30 \
   -muxrate 38800000 -pcr_period 30 -pat_period 0.1 -sdt_period 3600 "$out/ff_cbr.ts"
# (2) No -muxrate: ffmpeg's variable-rate output.
ff -f mpegts -mpegts_transport_stream_id 0x0101 -mpegts_pmt_start_pid 0x30 "$out/ff_vbr.ts"
# (3) Flag variants at CBR.
ff -f mpegts -muxrate 20000000 -mpegts_flags +resend_headers+pat_pmt_at_frames \
   -mpegts_pmt_start_pid 0x30 "$out/ff_flags.ts"
ff -f mpegts -muxrate 20000000 -mpegts_flags +system_b -mpegts_pmt_start_pid 0x30 \
   "$out/ff_system_b.ts"
ff -f mpegts -muxrate 20000000 -mpegts_m2ts_mode 0 -mpegts_copyts 0 -pcr_period 20 \
   -max_delay 100000 -mpegts_pmt_start_pid 0x30 "$out/ff_delay100ms.ts"

# (4) ATSC MPTS with PSIP: ffmpeg at 37 Mbit/s, TSDuck drops the SDT and injects MGT,
# CVCT, STT and EIT-0 into the null packets; the STT is then advanced with the bytes.
x="$out/xml"
mkdir -p "$x"
cat > "$x/psip.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8"?>
<tsduck>
  <CVCT version="0" transport_stream_id="0x0101">
    <channel short_name="TEST1" major_channel_number="50" minor_channel_number="1"
             modulation_mode="256-QAM" channel_TSID="0x0101" program_number="1"
             service_type="dtv" source_id="1"/>
    <channel short_name="TEST2" major_channel_number="50" minor_channel_number="2"
             modulation_mode="256-QAM" channel_TSID="0x0101" program_number="2"
             service_type="dtv" source_id="2"/>
  </CVCT>
</tsduck>
EOF
now_utc=$(date -u +%s)
win=$(( (now_utc - 315964800) / 10800 * 10800 + 315964800 ))
win_str=$(date -u -d "@$win" "+%Y-%m-%d %H:%M:%S")
cat > "$x/eit0.xml" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<tsduck>
  <ATSC_EIT version="0" source_id="1">
    <event event_id="1" start_time="$win_str" ETM_location="0" length_in_seconds="10800">
      <title_text><string language="eng" text="Test pattern one"/></title_text>
    </event>
  </ATSC_EIT>
  <ATSC_EIT version="0" source_id="2">
    <event event_id="1" start_time="$win_str" ETM_location="0" length_in_seconds="10800">
      <title_text><string language="eng" text="Test pattern two"/></title_text>
    </event>
  </ATSC_EIT>
</tsduck>
EOF
cv=$("$TSDUCK/tstabcomp" "$x/psip.xml" -o "$x/psip.bin" && stat -c %s "$x/psip.bin")
ei=$("$TSDUCK/tstabcomp" "$x/eit0.xml" -o "$x/eit0.bin" && stat -c %s "$x/eit0.bin")
cat > "$x/mgt.xml" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<tsduck>
  <MGT version="0">
    <table type="CVCT-current" PID="0x1FFB" version_number="0" number_bytes="$cv"/>
    <table type="EIT-0" PID="0x1D00" version_number="0" number_bytes="$ei"/>
  </MGT>
</tsduck>
EOF
gps0=$(( now_utc - 315964800 + 18 ))
cat > "$x/stt.xml" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<tsduck><STT system_time="$gps0" GPS_UTC_offset="18" DS_status="false"/></tsduck>
EOF
ff -f mpegts -mpegts_transport_stream_id 0x0101 -mpegts_pmt_start_pid 0x30 \
   -muxrate 37000000 -pcr_period 30 -pat_period 0.1 -sdt_period 3600 -max_delay 500000 \
   "$out/ff_37m.ts"
"$tsp" -I file "$out/ff_37m.ts" \
  -P filter --negate --pid 0x11 --stuffing \
  -P inject --pid 0x1FFB --bitrate 60000 --stuffing \
       "$x/mgt.xml=150" "$x/psip.xml=400" "$x/stt.xml=1000" \
  -P inject --pid 0x1D00 --bitrate 20000 --stuffing "$x/eit0.xml=500" \
  -O file "$out/atsc_fixed_stt.ts"
python3 -I "$here/tsmangle.py" "$out/atsc_fixed_stt.ts" "$out/atsc.ts" stt-advance \
  --rate 37000000 --gps0 "$gps0"
ls -l "$out"
