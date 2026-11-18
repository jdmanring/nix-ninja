#!/usr/bin/env bash
# A CUSTOM TARGET THAT cd's TO THE BUILD DIR'S PARENT AND RUNS A SCRIPT
# THERE BY ABSOLUTE PATH, on configuration A.
#
# shaderc's check-copyright target has WORKING_DIRECTORY at the source
# dir, and the tree is configured at <src>/build, so CMake emits
# `cd <src> && python3 <src>/utils/add_copyright.py --check` with nothing
# in DEPENDS. The driver rewrote the cd target to a bare `..`, which the
# ancestor test (looking for `../`) did not recognise, so the command
# took the descent branch: `cd .. && python3 ../utils/add_copyright.py`,
# one directory too high, and the script was never uploaded. The B twin
# is `cd-parent-script-b.sh`.
#
# Configuration A, seconds. Blanket off. Needs a builder-rpc-v0 daemon.
#   PASS    the edge builds and its task carries the script
#   FAIL    otherwise
#   --control <rev>   expects the failure (any rev before 19d92b9)
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
  W=$(mktemp -d -t nn-cdparent-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); PY=$(px python3); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-cdparent-XXXXXX); S=$D/source; mkdir -p "$S/utils" "$S/build/CMakeFiles"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'print("copyright ok")\n' > "$S/utils/add_copyright.py"
cat > "$S/build/build.ninja" <<NINJA
rule CUSTOM_COMMAND
  command = \$COMMAND
build CMakeFiles/check-copyright: CUSTOM_COMMAND
  COMMAND = cd $S && python3 $S/utils/add_copyright.py --check
build check-copyright: phony CMakeFiles/check-copyright
NINJA
cd "$S/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$PY/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja -t drv check-copyright > "$D/drv.json" 2>/dev/null
carried=$(grep -o 'NIX_NINJA_INPUTS[^}]*' "$D/drv.json" | grep -c 'add_copyright\.py'); carried=${carried:-0}
nix-ninja check-copyright > "$D/build.log" 2>&1; brc=$?
set -e
if [ -n "$CONTROL_REV" ]; then
  if [ $brc -ne 0 ] && [ "$carried" = 0 ]; then echo "CONTROL PASS: $CONTROL_REV neither carries the script nor builds (rc=$brc)"; exit 0; fi
  echo "CONTROL FAILED: expected the failure at $CONTROL_REV, rc=$brc carried=$carried"; exit 1
fi
if [ $brc -eq 0 ] && [ "$carried" -gt 0 ]; then
  echo "PASS: a cd to the build dir's parent carries the script it names and the edge builds"; exit 0
fi
echo "FAIL: rc=$brc carried=$carried"; grep -E 'ABORTED|No such file' "$D/build.log" | head -3; exit 1
