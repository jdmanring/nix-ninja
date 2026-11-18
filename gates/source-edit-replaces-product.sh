#!/usr/bin/env bash
# A SOURCE EDITED BETWEEN TWO DRIVER PASSES IN ONE BUILD DIRECTORY MUST
# REPLACE THE PLACED PRODUCT.
#
# meson-python's suite rebuilds an editable wheel in place through the ninja
# drop-in: pass one builds, the test edits a source, pass two rebuilds, and
# the import returned the pre-edit value. The first pass leaves the product
# on disk as a regular file (`refresh_placed_mtimes`), and the second pass
# has three readers that can take a file on disk for the answer: the walk,
# the resolve cache (fingerprint is count, total size, max mtime), and the
# placed product itself. The edit here keeps the SIZE of the source so only
# the mtime and the bytes change.
#
# Configuration A, seconds. Needs a builder-rpc-v0 daemon.
#   PASS    pass two emits a different task derivation and the product on
#           disk holds the edited bytes
#   FAIL    same derivation, or the product still holds the first bytes
#   --control <rev>   builds the driver and task binary from <rev>
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=${NN_GATE_SRC:-$REPO}
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-srcedit-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-srcedit-XXXXXX); S=$D/src; mkdir -p "$S/build"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'value=1\n' > "$S/v.txt"
printf 'rule gen\n  command = cat $in > $out\nbuild v.out: gen ../v.txt\n' > "$S/build/build.ninja"
cd "$S/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
# The resolve cache is .nix-ninja-resolve-cache.v1 in the build dir, so pass two reads pass one's entries.

pass() {
  set +e
  nix-ninja -t drv v.out > "$D/drv$1.json" 2> "$D/drv$1.err"; rc=$?
  set -e
  [ $rc -eq 0 ] || { echo "FAIL: pass $1 -t drv rc=$rc"; tail -3 "$D/drv$1.err"; exit 1; }
  set +e
  nix-ninja v.out > "$D/build$1.log" 2>&1; brc=$?
  set -e
  [ $brc -eq 0 ] || { echo "FAIL: pass $1 build rc=$brc"; grep -E "ABORTED|error" "$D/build$1.log" | head -3; exit 1; }
}
pass 1
got1=$(cat v.out)
# Same size, different bytes, later mtime (the filesystem's clock, not touch).
printf 'value=2\n' > "$S/v.txt"
pass 2
got2=$(cat v.out)
d1=$(grep -oE '/nix/store/[a-z0-9]{32}-ninja-build\.drv' "$D/drv1.json" | sort -u | head -1)
d2=$(grep -oE '/nix/store/[a-z0-9]{32}-ninja-build\.drv' "$D/drv2.json" | sort -u | head -1)

if [ -n "$CONTROL_REV" ]; then echo "control at $CONTROL_REV: drv1=$d1 drv2=$d2 out1='$got1' out2='$got2'"; fi
if [ "$got1" = "value=1" ] && [ "$got2" = "value=2" ] && [ -n "$d1" ] && [ "$d1" != "$d2" ]; then
  echo "PASS: a source edit between passes re-keys the task and replaces the placed product"; exit 0
fi
echo "FAIL: out1='$got1' out2='$got2' drv1=$d1 drv2=$d2"; exit 1
