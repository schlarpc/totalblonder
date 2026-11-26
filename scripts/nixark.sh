#!/usr/bin/env bash
# nixark.sh command [args...]
# Run a command of this repository on nixark, the machine with the card, from bagel:
# copies the tracked tree to ~/totalblonder-rs there (never touches ~/totalblonder-spike) and
# runs the command in that directory inside the memory-capped scope (scripts/capped.sh,
# MemoryMax 10G by default; CAP_MEM=6G to change). Examples:
#
#   scripts/nixark.sh nix run .#gpu-plumbing        # GPU tests of the CUDA adapter
#   scripts/nixark.sh nix flake check               # the sandbox checks, on nixark
#   scripts/nixark.sh nix run .#tb-m2venc -- plumb cuda
#
# nixark's login shell is fish, so the remote side is a script piped to `bash -s`.
# Nix builds on nixark are large the first time; `nix copy --no-check-sigs --to ssh://nixark
# <store paths>` from bagel first is cheaper (docs/ARCHITECTURE.md §6.4).
# NIXARK_DIR=name copies to ~/name instead of ~/totalblonder-rs (a worktree of its own).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
host=${NIXARK:-nixark}
dir=${NIXARK_DIR:-totalblonder-rs}
# In a git worktree .git is a file naming a path on this machine; nix on nixark would fail on it.
skipgit=(); [ -f "$root/.git" ] && skipgit=(--exclude /.git)
rsync -a --delete --exclude work --exclude node_modules --exclude target --exclude '/result' --exclude '/result-*' \
  --exclude .direnv --exclude wordlists --exclude tmp --exclude golden/out \
  --exclude /media \
  "${skipgit[@]}" "$root"/ "$host":"$dir"/
printf -v cmd '%q ' "$@"
ssh "$host" 'bash -s' <<EOF2
set -euo pipefail
cd ~/$dir
free_gb=\$(df --output=avail -BG / | tail -1 | tr -dc 0-9)
if [ "\$free_gb" -lt 40 ]; then echo "nixark: only \${free_gb} GB free on / (< 40); not starting" >&2; exit 75; fi
CAP_MEM=\${CAP_MEM:-${CAP_MEM:-10G}} ./scripts/capped.sh $cmd
EOF2
