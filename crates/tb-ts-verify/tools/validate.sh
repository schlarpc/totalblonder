#!/usr/bin/env bash
# Damages a stream in known ways (tools/tsmangle.py) and runs tb-ts-verify and TSDuck's
# nearest equivalents on each result, as a comparison table.
#
# usage: validate.sh STREAM RATE OUTDIR
#   STREAM  a clean-ish stream with PIDs 0x0101 (video, PCR) and 0x0104 (AC-3), such as
#           atsc.ts of make_streams.sh; RATE its rate in bit/s.
# needs: TSDUCK=/path/to/tsduck/bin (or nix), python3, a release build of tb-ts-verify.
set -euo pipefail
in=${1:?usage: validate.sh STREAM RATE OUTDIR}
rate=${2:?}
out=${3:?}
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../../.." && pwd)
if [ -z "${TSDUCK:-}" ]; then
  TSDUCK=$(nix build --no-link --print-out-paths --inputs-from "$repo" nixpkgs#tsduck)/bin
fi
bin=${TB_TS_VERIFY:-$(find "$repo/target" -path '*release/tb-ts-verify' -type f | head -1)}
mkdir -p "$out"
mangle() { python3 -I "$here/tsmangle.py" "$in" "$out/$1.ts" "${@:2}"; }

# Counts of the given rules in tb-ts-verify's JSON report.
mine() {
  local f=$1
  shift
  "$bin" --json --rate "$rate" --fail-on none "$f" | python3 -I -c '
import json, sys
d = json.load(sys.stdin)
c = d["counts"]
print(" ".join(f"{r}={c.get(r, 0)}" for r in sys.argv[1:]))
' "$@"
}
cont() { "$TSDUCK/tsp" -I file "$1" -P continuity -O drop 2>&1 | grep -c "continuity:" || true; }
pcrv() {
  "$TSDUCK/tsp" -I file "$1" -P pcrverify --absolute --jitter-max 13 -O drop 2>&1 \
    | grep -c "PCR jitter" || true
}
ana() { "$TSDUCK/tsanalyze" "$1" | grep -E "$2" | head -1 | sed 's/|//g; s/  */ /g'; }
intervals() {
  "$TSDUCK/tsp" --atsc -I file "$1" -P tables --all-sections --packet-index --log \
    --pid "$2" -O drop 2>&1 | python3 -I "$here/tsduck_intervals.py" "$rate"
}

row() { printf '| %s | %s | %s |\n' "$1" "$2" "$3"; }
echo "| case | tb-ts-verify | TSDuck |"
echo "|---|---|---|"
mangle clean copy
row clean "$(mine "$out/clean.ts" CC-ERROR SECT-CRC PCR-ACCURACY PCR-DISCONTINUITY TS-TEI TS-SYNC-BYTE)" \
  "continuity $(cont "$out/clean.ts"); pcrverify >13 ticks $(pcrv "$out/clean.ts")"
mangle drop drop --pid 0x101 --nth 5000 --count 3
row "3 video packets dropped" "$(mine "$out/drop.ts" CC-ERROR)" "continuity $(cont "$out/drop.ts")"
mangle cc cc --pid 0x104 --nth 1000 --delta 5
row "audio CC +5 in one packet" "$(mine "$out/cc.ts" CC-ERROR)" "continuity $(cont "$out/cc.ts")"
mangle crc crc --pid 0 --nth 50
row "PAT CRC damaged" "$(mine "$out/crc.ts" SECT-CRC PAT-INTERVAL PAT-INTERVAL-TR)" \
  "PAT sections decoded: $(intervals "$out/crc.ts" 0 | sed 's/.*: //')"
mangle delaypat delay-pat --from 5 --dur 0.6 --rate "$rate"
row "no PAT 5.0-5.6 s" "$(mine "$out/delaypat.ts" PAT-INTERVAL-TR PAT-INTERVAL CC-ERROR)" \
  "$(intervals "$out/delaypat.ts" 0 | sed 's/.*: //'); continuity $(cont "$out/delaypat.ts")"
mangle jitter pcr-jitter --pid 0x101 --ns 2000 --every 10
row "PCR +-2 us on every 10th" "$(mine "$out/jitter.ts" PCR-ACCURACY PCR-ACCURACY-NOMINAL)" \
  "pcrverify >13 ticks $(pcrv "$out/jitter.ts")"
mangle jitter300 pcr-jitter --pid 0x101 --ns 300 --every 10
row "PCR +-300 ns on every 10th" "$(mine "$out/jitter300.ts" PCR-ACCURACY PCR-ACCURACY-NOMINAL)" \
  "pcrverify >13 ticks $(pcrv "$out/jitter300.ts")"
mangle jump pcr-jump --pid 0x101 --nth 300 --ms 200
row "PCR +200 ms from the 300th" "$(mine "$out/jump.ts" PCR-DISCONTINUITY PCR-ACCURACY)" \
  "pcrverify >13 ticks $(pcrv "$out/jump.ts"); $(ana "$out/jump.ts" 'Leaps')"
mangle tei tei --nth 100000
row "TEI in one packet" "$(mine "$out/tei.ts" TS-TEI)" "$(ana "$out/tei.ts" 'transport error')"
mangle sync sync --nth 100000 --count 3
row "3 sync bytes corrupted" "$(mine "$out/sync.ts" TS-SYNC-BYTE TS-SYNC-LOSS CC-ERROR)" \
  "$(ana "$out/sync.ts" 'invalid sync'); continuity $(cont "$out/sync.ts")"
mangle scr scramble --pid 0x101 --nth 1000 --count 10
row "10 video packets scrambled" "$(mine "$out/scr.ts" CAT-ERROR)" "$(ana "$out/scr.ts" 'Scrambled')"
