#!/usr/bin/env bash
# A POST-BUILD SYMLINK WHOSE cd SITS IN THE MIDDLE OF THE COMMAND, on
# configuration A.
#
# glslang 16.2.0 attaches `add_custom_command(TARGET glslang-standalone
# POST_BUILD COMMAND cmake -E create_symlink glslang glslangValidator
# WORKING_DIRECTORY $<TARGET_FILE_DIR:...>)` with NO BYPRODUCTS, so cmake
# appends it to the target's own link rule and ninja declares no output for
# the link. The driver adds one, and it read only a LEADING `cd`: the base
# stayed at the build root, the link was declared there, and the task failed
# with "declared output glslangValidator was not written by the command"
# while the file sat one directory down.
#
# A LEADING cd IS THE GEOMETRY THAT ALREADY WORKED, and a first reduction
# built for exactly that reason. The cd must follow the link command.
#
# Configuration A, seconds. Needs a builder-rpc-v0 daemon.
#   PASS    the edge builds and the alias is placed beside its target
#   FAIL    otherwise
#   --control <rev>   expects the failure (any rev before 0232ed8)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=${NN_GATE_SRC:-$REPO}
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-pbsym-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc)
CMAKE=$(px cmake); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-pbsym-XXXXXX); S=$D/source
mkdir -p "$S/StandAlone" "$S/build/StandAlone"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'int main(void){return 0;}\n' > "$S/StandAlone/StandAlone.c"

# The cd FOLLOWS the link, which is what cmake emits for a POST_BUILD
# command, and the target is spelled relative so configuration A can reach
# the shape at all.
cat > "$S/build/build.ninja" <<NINJA
rule LINK
  command = \$COMMAND
build StandAlone/prog: LINK $S/StandAlone/StandAlone.c
  COMMAND = cc -o StandAlone/prog $S/StandAlone/StandAlone.c && cd StandAlone && $CMAKE/bin/cmake -E create_symlink prog progValidator
build all: phony StandAlone/prog
NINJA

cd "$S/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CMAKE/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja -t drv all > "$D/drv.json" 2>/dev/null
nix-ninja all > "$D/build.log" 2>&1; brc=$?
set -e

# THE VERDICT IS THE DECLARED SPELLING, because that is where the defect
# lives: the link is written under StandAlone/ and was declared at the build
# root. A status check alone is not enough in either direction, since a
# driver that declares the link nowhere at all also exits 0.
outs=$(grep -o '"NIX_NINJA_OUTPUTS": *"[^"]*"' "$D/drv.json" | head -1)
beside=0; bare=0
case $outs in *StandAlone/progValidator*) beside=1 ;; esac
case $outs in *:progValidator:*) bare=1 ;; esac

if [ -n "$CONTROL_REV" ]; then
  if [ "$bare" = 1 ] && [ "$beside" = 0 ] && [ $brc -ne 0 ] \
     && grep -q 'declared output progValidator was not written' "$D/build.log"; then
    echo "CONTROL PASS: $CONTROL_REV declares the link at the build root and fails (rc=$brc)"; exit 0
  fi
  echo "CONTROL FAILED: expected the bare declaration and the failure at $CONTROL_REV, rc=$brc beside=$beside bare=$bare"
  tail -5 "$D/build.log"; exit 1
fi
if [ $brc -eq 0 ] && [ "$beside" = 1 ] && [ "$bare" = 0 ]; then
  echo "PASS: a post-build symlink is declared beside its target and the edge builds"; exit 0
fi
echo "FAIL: rc=$brc beside=$beside bare=$bare"
echo "$outs" | head -1
grep -E 'ABORTED|declared output|Error' "$D/build.log" | head -5; exit 1
