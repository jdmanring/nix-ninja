#!/usr/bin/env bash
# A DECLARED OUTPUT THE COMMAND NEVER WRITES: the task binary's message.
#
# raptor2's src/CMakeLists.txt declares `turtle_parser.tab.c` as the OUTPUT
# of a bison rule whose command writes `-o turtle_parser.c`, and the
# fix-bison edge rewrites turtle_parser.c in place. Real ninja tolerates a
# missing custom-command output (the edge is merely always dirty); a task
# derivation cannot, and the failure read `canonicalize(...): No such
# file`, which blamed materialisation. That is a PACKAGE defect and this
# gate does not fix it: it pins the message that names it.
#
# The sibling shape that MUST succeed: a custom TARGET (cmake's stamp
# output under CMakeFiles/) with a BYPRODUCT the command never writes.
# doxygen's check_git_repository writes generated_src/git_state only on
# a run that finds the previous run's product, and real ninja tolerates
# the absent byproduct because the edge re-runs every build.
#
# Configuration A, seconds. Needs a builder-rpc-v0 daemon.
#   PASS    the bison task fails with "declared output ... was not written"
#           AND the custom-target edge builds with both outputs placed
#   FAIL    either half
#   --control <rev>   expects the canonicalize wording (any rev before
#                     41de370) and does not run the custom-target half
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
  W=$(mktemp -d -t nn-declout-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep); BISON=$(px bison)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-declout-XXXXXX); S=$D/src; mkdir -p "$S/src" "$D/build/src"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
cat > "$S/src/turtle_parser.y" <<'EOF'
%{
int yylex(void){return 0;} void yyerror(const char*s){(void)s;}
%}
%token X
%%
start: X | ;
%%
EOF
# raptor2's shape: OUTPUT names the .tab.c, the command writes the .c.
cat > "$D/build/build.ninja" <<EOF
rule BISON
  command = cd $D/build/src && bison -o $D/build/src/turtle_parser.c $S/src/turtle_parser.y
build src/turtle_parser.tab.c: BISON $S/src/turtle_parser.y
EOF
cd "$D/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$BISON/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
set +e
nix-ninja src/turtle_parser.tab.c > "$D/build.log" 2>&1; brc=$?
set -e
[ $brc -ne 0 ] || { echo "FAIL: the edge built (rc=0); the package shape is not what this gate expects"; exit 1; }
# The message is in the TASK's log, not the driver's.
msg=""
for drv in $(grep -oE '/nix/store/[a-z0-9]{32}-ninja-build\.drv' "$D/build.log" | sort -u); do
  msg="$msg$(nix log "$drv" 2>/dev/null | grep -E 'declared output|canonicalize' || true)"
done
if [ -n "$CONTROL_REV" ]; then
  case "$msg" in *"canonicalize(src/turtle_parser.tab.c)"*) echo "CONTROL PASS: $CONTROL_REV names canonicalize, not the output"; exit 0;; esac
  echo "CONTROL FAILED: expected the canonicalize wording at $CONTROL_REV, got: $msg"; exit 1
fi
case "$msg" in
  *"declared output src/turtle_parser.tab.c was not written by the command"*) ;;
  *) echo "FAIL: task message was: ${msg:-<none found>}"; exit 1;;
esac
# doxygen's shape: stamp plus a byproduct the command does not write.
mkdir -p "$D/build/generated_src" "$D/build/libversion/CMakeFiles"
cat >> "$D/build/build.ninja" <<EOF
rule CUSTOM_COMMAND
  command = \$COMMAND
build libversion/CMakeFiles/check_git generated_src/gitversion.cpp generated_src/git_state: CUSTOM_COMMAND
  COMMAND = cd $D/build && echo 'int v;' > generated_src/gitversion.cpp
build libversion/check_git: phony libversion/CMakeFiles/check_git generated_src/gitversion.cpp generated_src/git_state
EOF
set +e
nix-ninja libversion/check_git > "$D/build2.log" 2>&1; brc2=$?
set -e
if [ $brc2 -eq 0 ] && [ -f generated_src/gitversion.cpp ] && [ -f generated_src/git_state ] && [ ! -s generated_src/git_state ]; then
  echo "PASS: an unwritten declared output is named as such by the task binary; a custom target's unwritten byproduct is created empty"; exit 0
fi
echo "FAIL: custom-target half rc=$brc2 gitversion=$([ -f generated_src/gitversion.cpp ] && echo yes || echo no) git_state=$([ -f generated_src/git_state ] && echo yes || echo no)"; grep -E "ABORTED|not written|error" "$D/build2.log" | head -3; exit 1
