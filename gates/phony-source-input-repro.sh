#!/usr/bin/env bash
# A SOURCE FILE MESON MARKS AS A PHONY OUTPUT IS DROPPED FROM EVERY TASK
# THAT DECLARES IT.
#
# meson emits `build ../src/meson.build ../src/rules/base.xml ...: phony`
# (no inputs) for every file its regeneration edge reads, which ninja takes
# as "this path is present". The driver recorded every phony output as an
# alias of its inputs, so an alias with NO inputs expanded to the empty set
# and a task declaring that file (`depend_files`, an implicit input) ran
# without it. xkeyboard-config's rules generator opened base.xml and found
# nothing; the driver never named the file it dropped.
#
# Fix: a phony with no inputs over a file that exists is not recorded
# (Runner::start); the input then resolves as the source file it is. A
# phony over no file (`build all: phony` in a header-only project) is still
# recorded, so the CLI boundary keeps telling a no-op from a failure.
#
# Configuration A, seconds. Needs a builder-rpc-v0 daemon.
#   PASS    both declared files reach the task and the output holds both
#   FAIL    the marked file is missing from the task's inputs or output
#   --control <rev>   builds the driver from <rev> and expects the drop
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
  W=$(mktemp -d -t nn-phony-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  GIT=$(command -v git)
  trap '"$GIT" -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-phony-XXXXXX); S=$D/src; mkdir -p "$S/a" "$S/build"
# ONE EXIT TRAP CARRIES BOTH CLEANUPS: bash keeps only the last one armed, so a
# second trap here once dropped the control worktree removal armed above. Each
# action ends `|| true` because errexit applies inside the trap, and git is the
# path resolved above because PATH no longer holds it when the trap runs.
trap '[ "${KEEP:-}" = 1 ] || rm -rf "$D" || true; [ -z "${W:-}" ] || "$GIT" -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
printf 'hello\n' > "$S/a/x.txt"; printf 'unlisted\n' > "$S/a/y.txt"
# x.txt is marked the way meson marks a regeneration input; y.txt is the
# same file shape without the mark, so a drop of x.txt alone is the class.
printf 'rule gen\n  command = cat ../a/x.txt ../a/y.txt > $out\nbuild out.txt: gen | ../a/x.txt ../a/y.txt\nbuild ../a/x.txt: phony\n' > "$S/build/build.ninja"
cd "$S/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"

set +e
nix-ninja -t drv out.txt > "$D/drv.json" 2> "$D/drv.err"; rc=$?
set -e
[ $rc -eq 0 ] || { echo "FAIL: -t drv rc=$rc"; tail -3 "$D/drv.err"; exit 1; }
# The inputs list is in the derivation's env; count the two build paths.
# `grep -c` prints 0 and exits 1 on no match; capture the count, drop the
# status, and default to 0 so a missing grep cannot read as a match.
has_x=$(grep -c ':\.\./a/x\.txt:' "$D/drv.json"); has_x=${has_x:-0}
has_y=$(grep -c ':\.\./a/y\.txt:' "$D/drv.json"); has_y=${has_y:-0}
set +e
nix-ninja out.txt > "$D/build.log" 2>&1; brc=$?
set -e
got=$(cat out.txt 2>/dev/null | tr '\n' ' ')

if [ -n "$CONTROL_REV" ]; then
  if [ "$has_x" -eq 0 ] && [ "$has_y" -gt 0 ]; then
    echo "CONTROL PASS: the phony-marked file was dropped at $CONTROL_REV (x absent, y present; build rc=$brc)"; exit 0
  fi
  echo "CONTROL FAILED: expected the drop at $CONTROL_REV, got x=$has_x y=$has_y"; exit 1
fi
if [ "$has_x" -gt 0 ] && [ "$has_y" -gt 0 ] && [ $brc -eq 0 ] && [ "$got" = "hello unlisted " ]; then
  echo "PASS: a phony-marked source input reaches the task (x and y present, output holds both)"; exit 0
fi
echo "FAIL: x=$has_x y=$has_y build rc=$brc output='$got'"; grep -E "ABORTED|No such" "$D/build.log" | head -3; exit 1
