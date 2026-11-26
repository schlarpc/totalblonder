#!/usr/bin/env bash
# Headless import + analysis of a Nios II binary (bflt.py-produced ELF, or a raw image).
#
# Usage:
#   nix develop ~/re-shell -c tools/ghidra-nios2/nios2-import.sh [options] <file>
#
# Options:
#   -p DIR        Ghidra project directory   (default: work/ghidra/projects in the checkout)
#   -n NAME       Ghidra project name        (default: nios2)
#   -d DIR        also export decompiled C of all functions to DIR (must be under work/)
#   -l FILE       also dump a linear-sweep listing (validation format) to FILE
#   -g VALUE      force gp value (e.g. 0x1001bef0) instead of auto-detecting it
#   -r BASE       raw binary: import with -processor Nios2:LE:32:default at load address BASE
#   -k            keep existing program in project (default: -overwrite)
#
# The program is imported, gp is set with SetNios2GP.java *before* auto-analysis
# (so constant/reference analysis sees it), then analyzed and optionally exported.
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(cd "$SRC_DIR/../.." && pwd)/work"
PROJ_DIR="$WORK/ghidra/projects"
PROJ_NAME="nios2"
DECOMP_DIR=""
LIST_FILE=""
GP=""
RAW_BASE=""
OVERWRITE="-overwrite"

while getopts "p:n:d:l:g:r:k" opt; do
    case $opt in
        p) PROJ_DIR=$OPTARG ;;
        n) PROJ_NAME=$OPTARG ;;
        d) DECOMP_DIR=$OPTARG ;;
        l) LIST_FILE=$OPTARG ;;
        g) GP=$OPTARG ;;
        r) RAW_BASE=$OPTARG ;;
        k) OVERWRITE="" ;;
        *) sed -n '2,20p' "$0"; exit 2 ;;
    esac
done
shift $((OPTIND - 1))
[ $# -eq 1 ] || { sed -n '2,20p' "$0"; exit 2; }
INPUT=$(readlink -f "$1")

case "$(readlink -f "${DECOMP_DIR:-$WORK/x}")" in
    "$WORK"/firmware/*) echo "refusing to write into $WORK/firmware" >&2; exit 1 ;;
esac

: "${GHIDRA_INSTALL_DIR:?run inside 'nix develop ~/re-shell'}"
mkdir -p "$PROJ_DIR" "$PWD/tmp/jtmp" 2>/dev/null || true

args=("$PROJ_DIR" "$PROJ_NAME" -import "$INPUT" $OVERWRITE
      -scriptPath "$SRC_DIR/ghidra_scripts"
      -preScript SetNios2GP.java ${GP:+"$GP"})
if [ -n "$RAW_BASE" ]; then
    args+=(-loader BinaryLoader -loader-baseAddr "$RAW_BASE"
           -processor Nios2:LE:32:default -cspec default)
fi
args+=(-postScript Nios2CreateMissingFunctions.java)
if [ -n "$DECOMP_DIR" ]; then
    mkdir -p "$DECOMP_DIR"
    args+=(-postScript ExportNios2Decompiled.java "$(readlink -f "$DECOMP_DIR")")
fi
if [ -n "$LIST_FILE" ]; then
    mkdir -p "$(dirname "$LIST_FILE")"
    args+=(-postScript DumpNios2Listing.java "$(readlink -f "$(dirname "$LIST_FILE")")/$(basename "$LIST_FILE")")
fi

exec ghidra-analyzeHeadless "${args[@]}"
