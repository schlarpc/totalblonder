#!/usr/bin/env bash
# Streams of our encoder on the CPU reference adapter (no GPU), for the verifier.
#
#   ref_cases.sh ENCODER OUTDIR
#
# ENCODER is the tb-m2venc binary. Groups:
#   OUTDIR/clean/      small test pictures in the configurations the encoder is meant for, under
#                      the profile h262 (they are in no broadcast table)
#   OUTDIR/broadcast/  formats of A/53 Part 4 Table 6.2 under the default profile
#                      (scte43-256qam), a few pictures each
#   OUTDIR/refused.txt every configuration that must be refused when the session opens, with the
#                      encoder's message; the script fails if one is accepted or if the message
#                      does not name the rule
# The clips come from the encoder's own generator (--make-clip), so nothing else is needed.
set -euo pipefail
enc=$1
out=$2
mkdir -p "$out/clips" "$out/clean" "$out/broadcast"
c=$out/clips
"$enc" --make-clip 176 144 30 1 > "$c/p.yuv"
"$enc" --make-clip 176 144 24 2 noisy > "$c/n.yuv"
"$enc" --make-clip 176 144 24 4 interlaced > "$c/i.yuv"
"$enc" --make-clip 352 240 20 5 interlaced > "$c/i2.yuv"
"$enc" --make-clip 352 240 20 6 noisy > "$c/n2.yuv"
"$enc" --make-clip 184 144 12 3 > "$c/odd.yuv"
"$enc" --make-clip 720 576 6 9 > "$c/pal.yuv"
"$enc" --make-clip 704 480 8 11 interlaced > "$c/sd.yuv"
"$enc" --make-clip 640 480 6 12 > "$c/vga.yuv"
"$enc" --make-clip 1280 720 6 13 > "$c/hd720.yuv"
"$enc" --make-clip 1920 1080 4 14 interlaced > "$c/hd1080.yuv"

run() { # run GROUP NAME args...   (clean: profile h262; broadcast: the default)
  local dir=$out/$1 name=$2 prof=()
  [ "$1" = broadcast ] || prof=(--conformance h262)
  shift 2
  "$enc" --device ref "${prof[@]}" "$@" --output "$dir/$name" > "$dir/$name.log" 2>&1 || {
    echo "ref_cases: $name failed" >&2
    tail -5 "$dir/$name.log" >&2
    exit 1
  }
}
: > "$out/refused.txt"
refused() { # refused NAME PATTERN args...: the encoder must refuse, and say PATTERN
  local name=$1 pat=$2 log
  shift 2
  if log=$("$enc" --device ref "$@" --output "$out/refused-$name" 2>&1); then
    echo "ref_cases: $name was accepted, it must be refused" >&2
    exit 1
  fi
  if ! grep -q -- "$pat" <<< "$log"; then
    echo "ref_cases: $name was refused without naming its rule ($pat):" >&2
    tail -3 <<< "$log" >&2
    exit 1
  fi
  if ls "$out"/refused-"$name"*.m2v > /dev/null 2>&1; then
    echo "ref_cases: $name was refused but wrote a stream" >&2
    exit 1
  fi
  echo "$name: $(grep -- "$pat" <<< "$log" | tail -1)" >> "$out/refused.txt"
}
P="--input $c/p.yuv --size 176x144 --fps 30000/1001"
N="--input $c/n.yuv --size 176x144 --fps 30000/1001"
I="--input $c/i.yuv --size 176x144 --fps 30000/1001 --interlaced"
I2="--input $c/i2.yuv --size 352x240 --fps 30000/1001 --interlaced"
N2="--input $c/n2.yuv --size 352x240 --fps 30000/1001"

# ---- clean: rate control at several rates, GOP shapes, tools on and off
run clean p-cbr          $P --bitrate 400000
run clean p-low          $P --bitrate 140000
run clean p-starved      $N --bitrate 135000
run clean p-ipp          $P --bitrate 400000 --bframes 0
run clean p-b1-gop6      $P --bitrate 400000 --bframes 1 --gop 6
run clean p-intra        $P --bitrate 1500000 --gop 1 --bframes 0
run clean p-noaq         $P --bitrate 400000 --aq 0
run clean p-ivlc0        $P --bitrate 400000 --intra-vlc 0
run clean p-altscan      $P --bitrate 400000 --alt-scan 1
run clean p-range2       $P --bitrate 400000 --range 2
run clean p-range16      $P --bitrate 400000 --range 16
run clean p-forcekey     $P --bitrate 400000 --force-key 7,8,20
run clean p-rateplan     $P --bitrate 400000 --max-bitrate 800000 --rate-plan 10:150000,20:700000
run clean p-ratewalk     $P --bitrate 300000 --max-bitrate 800000 --rate-walk 140000:800000:7
run clean p-cc           $P --bitrate 400000 --cc
run clean n-cbr          $N --bitrate 800000
run clean n-vbvsmall     $N --bitrate 800000 --vbv 163840
run clean i-cbr          $I --bitrate 500000
run clean i-bff          $I --bitrate 500000 --bff
run clean i-noft         $I --bitrate 500000 --field-pred 0 --field-dct 0
run clean i-fp-only      $I --bitrate 500000 --field-dct 0
run clean i-zigzag       $I --bitrate 500000 --alt-scan 0
run clean i-cc           $I --bitrate 500000 --cc
run clean i2-cbr         $I2 --bitrate 1500000
run clean i2-low         $I2 --bitrate 600000
run clean n2-cbr         $N2 --bitrate 2500000
run clean p-2sessions    $P --bitrate 400000 --sessions 2 --stagger

# every frame rate of H.262 Table 6-4, with captions at the cc_count of the rate
run clean fps-23976      $P --fps 24000/1001 --bitrate 400000 --cc
run clean fps-24         $P --fps 24/1 --bitrate 400000 --cc
run clean fps-25         $P --fps 25/1 --bitrate 400000
run clean fps-30         $P --fps 30/1 --bitrate 400000 --cc
run clean fps-50         $P --fps 50/1 --bitrate 400000
run clean fps-5994       $P --fps 60000/1001 --bitrate 400000 --cc
run clean fps-60         $P --fps 60000/1000 --bitrate 400000
# the largest search ranges that the vertical f_code of the level can code (Table 8-8)
run clean range-30       $P --bitrate 400000 --range 30
run clean range-19-i     $I --bitrate 500000 --range 19
run clean n-range-30     $N --bitrate 800000 --range 30
# a size that is not a multiple of 16; 576 lines at 29.97 Hz (High-1440 level, not Main)
run clean odd-184        --input $c/odd.yuv --size 184x144 --fps 30000/1001 --bitrate 300000
run clean h14-576-30     --input $c/pal.yuv --size 720x576 --fps 30000/1001 --bitrate 4000000
run clean ml-576-25      --input $c/pal.yuv --size 720x576 --fps 25/1 --bitrate 4000000 --interlaced

# the macroblock size rule (H.262 Table 8-6): white noise and a noise patch at rates that give
# the finest quantizers
"$enc" --make-clip 176 144 12 7 white > "$c/white.yuv"
"$enc" --make-clip 176 144 12 8 patch > "$c/patch.yuv"
run clean mb-white       --input $c/white.yuv --size 176x144 --bitrate 15000000
run clean mb-patch       --input $c/patch.yuv --size 176x144 --bitrate 8000000
run clean mb-patch-i     --input $c/patch.yuv --size 176x144 --bitrate 8000000 --interlaced --bframes 0

# ---- broadcast: the formats of A/53 Part 4 Table 6.2 under the default profile
SD="--input $c/sd.yuv --size 704x480 --interlaced"
run broadcast 480i        $SD --bitrate 4000000 --cc
run broadcast 480i-169    $SD --bitrate 4000000 --aspect 3
run broadcast 480p5994    --input $c/sd.yuv --size 704x480 --fps 60000/1001 --bitrate 6000000 --cc
run broadcast 480p23976   --input $c/sd.yuv --size 704x480 --fps 24000/1001 --bitrate 3000000 --cc
run broadcast vga-30      --input $c/vga.yuv --size 640x480 --fps 30/1 --bitrate 3000000
run broadcast 720p5994    --input $c/hd720.yuv --size 1280x720 --fps 60000/1001 --bitrate 12000000 --cc
run broadcast 720p2997    --input $c/hd720.yuv --size 1280x720 --fps 30000/1001 --bitrate 8000000
run broadcast 720p24      --input $c/hd720.yuv --size 1280x720 --fps 24/1 --bitrate 8000000
run broadcast 1080i       --input $c/hd1080.yuv --size 1920x1080 --interlaced --bitrate 15000000 --cc
run broadcast 1080p23976  --input $c/hd1080.yuv --size 1920x1080 --fps 24000/1001 --bitrate 12000000
run broadcast 1080p30-388 --input $c/hd1080.yuv --size 1920x1080 --fps 30/1 --bitrate 38800000

# ---- refused: nothing of this may open a session (the default profile unless one is named)
H="--conformance h262"
# frame rates outside H.262 Table 6-4, and those the broadcast tables do not have
refused fps-2997-100   "Table 6-4" $H $P --fps 2997/100 --bitrate 400000
refused fps-15         "Table 6-4" $H $P --fps 15/1 --bitrate 400000
refused sd-25          "allow \[1, 2, 4, 5, 7, 8\]" --input $c/sd.yuv --size 704x480 --fps 25/1 --bitrate 4000000
refused sd-i-5994      "allow \[4, 5\]" $SD --fps 60000/1001 --bitrate 4000000
refused hd-i-50        "exceeds Main Profile at High level" --input $c/hd1080.yuv --size 1920x1080 --interlaced --fps 60/1 --bitrate 15000000
# sizes: a multiple of 4096, above High level, in no broadcast table, in SCTE 43 Table 3 only
"$enc" --make-clip 4096 32 2 9 > "$c/wide.yuv"
refused wide-4096      "shall not be zero" $H --input $c/wide.yuv --size 4096x32 --bitrate 4000000
refused above-hl       "exceeds Main Profile at High level" $H --input $c/hd1080.yuv --size 1920x1090 --bitrate 4000000
refused qcif           "not a compression format of A/53 Part 4 Table 6.2" $P --bitrate 400000
refused pal            "not a compression format of A/53 Part 4 Table 6.2" --input $c/pal.yuv --size 720x576 --fps 30000/1001 --bitrate 4000000
refused 720i           "not a compression format of A/53 Part 4 Table 6.2" --input $c/hd720.yuv --size 1280x720 --interlaced --bitrate 8000000
refused cable-720x480  "SCTE 54-2024" --input $c/sd.yuv --size 720x480 --interlaced --bitrate 4000000
refused cable-1440     "SCTE 54-2024" --input $c/hd1080.yuv --size 1440x1080 --interlaced --bitrate 12000000
refused cable-528      "SCTE 54-2024" --input $c/sd.yuv --size 528x480 --interlaced --bitrate 3000000
# aspect ratio codes
refused aspect-4       "Main Profile allows 1, 2 and 3" $H $P --bitrate 400000 --aspect 4
refused sd-square      "allow \[2, 3\]" $SD --bitrate 4000000 --aspect 1
refused vga-169        "allow \[1, 2\]" --input $c/vga.yuv --size 640x480 --bitrate 3000000 --aspect 3
# bit rate and buffer: the level, the profile, the units, the one-second transport limit
refused ml-20mbit      "Table 8-13" $H $N --bitrate 20000000
refused ml-vbv-4m      "Table 8-14" $H $P --bitrate 400000 --max-bitrate 4000000 --vbv 4014080
refused h14-vbv        "Table 8-14" --input $c/hd720.yuv --size 1280x720 --bitrate 20000000 --vbv 7995392
refused vbv-units      "units of 16384" $H $P --bitrate 400000 --vbv 200000
refused vbv-489        "vbv_buffer_size_value 488" --input $c/hd1080.yuv --size 1920x1080 --interlaced --bitrate 15000000 --vbv 8011776
refused a53-20mbit     "a53 ceiling" --conformance a53 --input $c/hd1080.yuv --size 1920x1080 --interlaced --bitrate 20000000
refused qam64-30mbit   "scte43-64qam ceiling" --conformance scte43-64qam --input $c/hd1080.yuv --size 1920x1080 --interlaced --bitrate 30000000
refused qam256-40mbit  "scte43-256qam ceiling" --input $c/hd1080.yuv --size 1920x1080 --interlaced --bitrate 40000000
refused delay-1s       "H.222.0" $SD --bitrate 1500000 --vbv 1835008
# search ranges that need a vertical f_code above 5
refused range-31       "Table 8-8" $H $P --bitrate 400000 --range 31
refused range-40       "Table 8-8" $H $P --bitrate 400000 --range 40
refused range-20-i     "Table 8-8" $H $I --bitrate 500000 --range 20
refused range-sd       "Table 8-8" $SD --bitrate 4000000 --range 24

# ---- fixed quantizer: a legal mode since the buffer model runs for it (the quantizer stands
# while the picture fits the declared buffer and the macroblock size rule; then the rows get
# coarser, then the ladder). Quantizer 1 on noise used to lose macroblocks.
run clean q-fixed4       $N --q 4
run clean q-fixed1       $N2 --q 1
run clean q-fixed31      $P --q 31
run clean q-fixed2-i     $I2 --q 2
run clean q-fixed1-white --input $c/white.yuv --size 176x144 --q 1
run clean q-fixed-small  --input $c/white.yuv --size 176x144 --q 2 --max-bitrate 400000 --vbv 65536
run broadcast 480i-q2    $SD --q 2
rm -rf "$c"
