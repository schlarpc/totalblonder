#!/usr/bin/env bash
# Build the Nios II SLEIGH spec and install the module as a Ghidra user extension.
#
# Usage (from anywhere):
#   nix develop ~/re-shell -c "$REPO"/tools/ghidra-nios2/build.sh   ($REPO: the checkout)
#
# Env overrides:
#   GHIDRA_INSTALL_DIR   Ghidra install (set by the re-shell dev shell)
#   NIOS2_BUILD_DIR      staging dir   (default: work/ghidra/build/Nios2 in the checkout)
#   NIOS2_EXT_DIR        install dir   (default: ~/.config/ghidra/ghidra_<ver>_<rel>/Extensions/Nios2)
set -euo pipefail

SRC_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
: "${GHIDRA_INSTALL_DIR:?run inside 'nix develop ~/re-shell' (GHIDRA_INSTALL_DIR unset)}"

props="$GHIDRA_INSTALL_DIR/Ghidra/application.properties"
ver=$(sed -n 's/^application.version=//p' "$props")
rel=$(sed -n 's/^application.release.name=//p' "$props")

BUILD_DIR="${NIOS2_BUILD_DIR:-$(cd "$SRC_DIR/../.." && pwd)/work/ghidra/build/Nios2}"
EXT_DIR="${NIOS2_EXT_DIR:-$HOME/.config/ghidra/ghidra_${ver}_${rel}/Extensions/Nios2}"

# The Ghidra launcher honors _JAVA_OPTIONS tmpdir from the dev shell; make sure it exists.
mkdir -p "$PWD/tmp/jtmp" 2>/dev/null || true

echo "[*] staging module in $BUILD_DIR"
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"
cp -r "$SRC_DIR/data" "$SRC_DIR/ghidra_scripts" "$SRC_DIR/Module.manifest" \
      "$SRC_DIR/extension.properties" "$SRC_DIR/README.md" "$BUILD_DIR/"
sed -i "s/^version=.*/version=${ver}/" "$BUILD_DIR/extension.properties"
rm -f "$BUILD_DIR"/data/languages/*.sla

echo "[*] compiling nios2.slaspec"
"$GHIDRA_INSTALL_DIR/support/sleigh" \
    "$BUILD_DIR/data/languages/nios2.slaspec" "$BUILD_DIR/data/languages/nios2.sla" 2>&1 \
    | grep -v -e '^Picked up _JAVA_OPTIONS' -e '^INFO ' || true
[ -s "$BUILD_DIR/data/languages/nios2.sla" ] || { echo "[!] sleigh compile failed" >&2; exit 1; }

echo "[*] installing to $EXT_DIR"
rm -rf "$EXT_DIR"
mkdir -p "$(dirname "$EXT_DIR")"
cp -r "$BUILD_DIR" "$EXT_DIR"
echo "[+] done. Language id: Nios2:LE:32:default"
