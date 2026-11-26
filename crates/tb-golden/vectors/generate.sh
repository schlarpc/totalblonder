#!/usr/bin/env bash
# Regenerate the golden vectors with the Rust encoder's recorder (tb-m2venc --golden) on the
# reference adapter: no GPU, any machine with Nix.
#
#   ./golden/generate.sh                 # writes crates/tb-golden/vectors/*.jsonl and crates/tb-golden/vectors/*.sha256
#   ./golden/generate.sh --check DIR     # writes into DIR instead, and compares with crates/tb-golden/vectors/
#   ./golden/generate.sh --device cuda --check DIR    # the same cases on the card (nixark):
#       every stream and every record must be the reference adapter's, except the records
#       that hold measured times (tick_end, shed_hist, shed_budget: the card's clock is real),
#       the command line in the first record, and the case `deadline`, whose decisions follow
#       the device's clock (nix run .#gate-golden)
#
# The binaries are built by Nix from the tree (or given: TB_M2VENC, TB_PIPE), so the vectors
# always come from the committed sources. Clips come from `tb-m2venc --make-clip` (the port of
# make_clip.py); nothing else is read. Each case runs twice and must write the same bytes: the
# reference adapter's clock counts executed steps, so even the measured records (`tick_end`,
# `shed_hist`, `shed_budget`) are deterministic here. The flake check `golden/fresh` runs this
# script with --check.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$here check= device=ref
if [ "${1:-}" = "--device" ]; then
  device=${2:?usage: generate.sh [--device ref|cuda] [--check DIR]}
  shift 2
fi
if [ "${1:-}" = "--check" ]; then
  out=${2:?usage: generate.sh [--device ref|cuda] [--check DIR]}
  check=1
  mkdir -p "$out"
fi
if [ "$device" != ref ] && [ -z "$check" ]; then
  echo "generate.sh: the committed vectors come from the reference adapter; --device $device needs --check" >&2
  exit 2
fi
out=$(cd "$out" && pwd)
enc=${TB_M2VENC:-$(cd "$here/../../.." && nix build .#tb-m2venc --no-link --print-out-paths)/bin/tb-m2venc}
pipe=${TB_PIPE:-$(cd "$here/../../.." && nix build .#tb-pipe --no-link --print-out-paths)/bin/tb-pipe}
tmp=$(mktemp -d "${TMPDIR:-/tmp}/tb-golden.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

clip() {  # name width height frames seed [interlaced|noisy]
  [ -s "$tmp/$1.yuv" ] || "$enc" --make-clip "${@:2}" > "$tmp/$1.yuv"
}
case_() {  # name clip args...
  local name=$1 clip=$2
  shift 2
  for pass in a b; do
    mkdir -p "$tmp/$pass"
    (cd "$tmp/$pass" && "$enc" --device "$device" --conformance h262 --input "../$clip.yuv" "$@" --output "$name" --golden "$name.jsonl" > /dev/null)
    [ "$device" = ref ] || break   # measured records differ between runs on the card
  done
  if [ "$device" = ref ] && ! cmp -s "$tmp/a/$name.jsonl" "$tmp/b/$name.jsonl"; then
    echo "golden case $name is not deterministic:" >&2
    diff "$tmp/a/$name.jsonl" "$tmp/b/$name.jsonl" | head -6 | cut -c1-300 >&2
    exit 1
  fi
  cp "$tmp/a/$name.jsonl" "$out/$name.jsonl"
  # The coded streams are the last word; keep their digests next to the records.
  (cd "$tmp/a" && sha256sum "$name"*.m2v) > "$out/$name.sha256"
  echo "$name: $(wc -l < "$out/$name.jsonl") records, $(stat -c %s "$out/$name.jsonl") bytes"
}

clip qcif 176 144 30 1
clip qcif-noisy 176 144 24 2 noisy
clip w184 184 144 12 3
clip qcif-i 176 144 24 4 interlaced

"$enc" --golden-tables "$out/tables.jsonl"
echo "tables: $(stat -c %s "$out/tables.jsonl") bytes"
"$pipe" --golden-taps "$out/taps.jsonl"
echo "taps: $(stat -c %s "$out/taps.jsonl") bytes"

# The cases first recorded by the C++ encoder (crates/tb-golden/vectors/README.md, "Provenance"):
# rate control with rate changes at key frames, captions.
case_ cbr qcif --size 176x144 --gop 6 --bframes 2 --bitrate 300000 --max-bitrate 600000 --cc \
  --rate-plan 2:150000,3:500000
# A rate at the floor of what the buffer guarantee allows, the smallest buffer, noise: correction
# passes, the ladder.
case_ ladder qcif-noisy --size 176x144 --gop 6 --bframes 2 --bitrate 135000 --vbv 16384
# Fixed quantizer, a width that is not a multiple of 16 (column padding, 2-D upload), a forced key frame.
case_ fixedq w184 --size 184x144 --gop 6 --bframes 2 --q 6 --force-key 7
# Interlaced (row padding to a multiple of 32), overlapped upload, captions, a rate change inside a GOP.
case_ interlaced qcif-i --size 176x144 --interlaced --gop 6 --bframes 2 --bitrate 400000 --max-bitrate 500000 \
  --overlap --cc --rate-now 8:250000
# Two staggered sessions with pictures given up at random and one session held for six frames.
case_ shed qcif --size 176x144 --gop 6 --bframes 2 --bitrate 300000 --sessions 2 --stagger --write 2 \
  --force-shed 0.3:7 --hold 0:1:10:16

# Cases added with the Rust recorder:
# the last rung of the ladder (all-skipped pictures) and I pictures replaced by P pictures:
# noise at the lowest rate the buffer guarantee allows, the smallest buffer, a key frame every
# second picture.
case_ skip qcif-noisy --size 176x144 --gop 2 --bframes 0 --bitrate 132000 --vbv 16384
# The deadline policy with a real deadline (a test switch: 1.025 ms of the reference adapter's
# step clock per tick, 1 ms of it the margin): three sessions, pictures given up by budget.
case_ deadline qcif --size 176x144 --gop 6 --bframes 2 --bitrate 300000 --sessions 3 --stagger --write 3 \
  --test-deadline 1025000
# Sessions opened and closed at run time beside a running one: batches made in the arena and
# given back (reaped), at two rates.
case_ churn qcif --size 176x144 --gop 6 --bframes 2 --bitrate 300000 --frames 24 \
  --group --input ../qcif-i.yuv --size 176x144 --interlaced --gop 6 --bframes 2 --bitrate 300000 \
  --sessions 3 --pool --churn 5:15 --churn-chunk 2 --reap-after 3 --write 3
# Frames written on the device (the driver's copies from the clip held there), padded columns.
case_ devin w184 --size 184x144 --gop 6 --bframes 2 --bitrate 300000 --input-mode dev
# Two formats in one encoder: two batches in every phase.
case_ twofmt qcif --size 176x144 --gop 6 --bframes 2 --bitrate 300000 --cc \
  --group --input ../qcif-i.yuv --size 176x144 --interlaced --gop 6 --bframes 2 --bitrate 400000 --frames 24

# Film mode (frames shown for other than one frame period; the records carry the display
# fields `start`, `units`, `top_first`, `time`, `tff`, `rff`, `gop_frames`, `frames` only for
# such frames): 3:2 pulldown on an interlaced session with a cut in the cadence, video pictures
# in between, a still, captions of 20 and 30 constructs, a rate change, a hold.
case_ film qcif --size 176x144 --interlaced --gop 15 --bframes 2 --bitrate 400000 --max-bitrate 600000 \
  --frames 40 --cc --film 3,2,3,2,3,2,2,i,i,3,3,2 --rate-now 17:300000 --hold 0:0:26:30
# A progressive session at 59.94 Hz with frames shown one, two and three times, pictures given
# up at random, the overlapped upload.
case_ film60 qcif --size 176x144 --fps 60000/1001 --gop 30 --bframes 2 --bitrate 400000 --max-bitrate 600000 \
  --frames 40 --cc --film 3,2,3,2,3,2,1,1,2,3 --force-shed 0.2:3 --overlap

if [ -n "$check" ]; then
  status=0
  # on the card: the records without the measured ones and without the command line
  same() {
    if [ "$device" = ref ]; then cmp -s "$1" "$2"; return; fi
    case $1 in
      *.jsonl) cmp -s <(grep -v '"k":"golden"\|"k":"tick_end"\|"k":"shed_hist"\|"k":"shed_budget"' "$1") \
                      <(grep -v '"k":"golden"\|"k":"tick_end"\|"k":"shed_hist"\|"k":"shed_budget"' "$2") ;;
      *) cmp -s "$1" "$2" ;;
    esac
  }
  for f in "$out"/*.jsonl "$out"/*.sha256; do
    b=$(basename "$f")
    if [ "$device" != ref ] && [ "${b%.*}" = deadline ]; then
      # its deadlines are on the device's clock, which counts steps only on the reference
      # adapter: on the card the policy decides by the real time of each tick
      echo "crates/tb-golden/vectors/$b: not compared on the $device device (decisions depend on its clock)"
      continue
    fi
    if ! same "$f" "$here/$b"; then
      echo "crates/tb-golden/vectors/$b differs from what generate.sh writes on the $device device" >&2
      status=1
    else
      echo "crates/tb-golden/vectors/$b: same ($device)"
    fi
  done
  n_out=$(find "$out" -maxdepth 1 -name '*.jsonl' -o -maxdepth 1 -name '*.sha256' | wc -l)
  n_here=$(find "$here" -maxdepth 1 -name '*.jsonl' -o -maxdepth 1 -name '*.sha256' | wc -l)
  [ "$n_out" -eq "$n_here" ] || { echo "crates/tb-golden/vectors/ has $n_here vector files, generate.sh writes $n_out" >&2; status=1; }
  exit $status
fi
