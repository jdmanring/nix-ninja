#!/usr/bin/env bash
# `python3 -m pkg.mod` NAMES NO FILE, so nothing seeds the sibling closure.
#
# The driver uploads a python script's siblings by seeding from INPUTS whose
# build_path ends in `.py` (task.rs, the py_dirs loop). A `-m` invocation
# names a MODULE and Python resolves it from PYTHONPATH, so there is no `.py`
# token anywhere on the command line, py_dirs stays empty, and the module
# never reaches the sandbox:
#
#     No module named rules.generator
#
# xkeyboard-config, round 19. Both tokens ARE on the command line - the
# PYTHONPATH assignment and the dotted name - so unlike dtc's compiled
# generator this is statically reachable.
#
#   pre-fix driver -> No module named gen.emit
#   fixed driver   -> the module is declared, the header is generated
#
# THE ASSIGNMENT GOES THROUGH `env`, NOT AS A LEADING `VAR=value` TOKEN. The
# first version of this reduction wrote `PYTHONPATH=../src python3 -m ...`
# and never reached the class: the driver resolves the command's FIRST TOKEN
# as a binary and answered `Failed to find PYTHONPATH=../src: cannot find
# binary path`. That is a separate gap and possibly a real one, since ninja
# runs commands through /bin/sh where the assignment is valid - but the
# package this gate is written for gets past it, so reproducing xkeyboard's
# failure means not tripping over it first.
#
# STATUS 2026-09-04: THE REDUCTION REPRODUCES THE CLASS AND THE FIX PASSES.
# Configuration A (the driver run here outside any derivation). Control on
# a528e0a: `No module named 'gen'` from a RUNNING generator task, CONTROL
# PASS. Subject on 24978af: PASS, the generated value reached the binary.
# Wiring mutated out (`py_dirs.extend(py_module_dirs)` dropped): FAIL. Two
# instrument defects fixed first: the driver took `env` as the program and
# left python3 off the sandbox PATH (a528e0a), and task_log read only the
# dependent's log while the class message sits in the dependency's. The
# package (xkeyboard-config) is unverified on B until a round drives it.
#
# HISTORY: the reduction did not reproduce the class for three attempts,
# each stopped by something that is not the subject:
#
#   1. `PYTHONPATH=../src python3 -m ...` as the command
#      -> "Failed to find PYTHONPATH=../src: cannot find binary path".
#         The driver resolves the FIRST TOKEN as a binary. ninja runs
#         commands through /bin/sh where a leading assignment is valid, so
#         this may be a real second gap; the package this gate is for gets
#         past it, so it is not the subject here.
#         CLOSED on main at 66b39a0: the program pick steps over leading
#         VAR=value tokens (command_program, task.rs), pinned by a unit
#         test and four caught mutants. Attempt 1's shape would now reach
#         the generator task; the reduction has not been re-run.
#   2. Routed through `env`
#      -> the generator edge produced NO DERIVATION at all, and the run
#         died as "1 dependency failed" naming the CONSUMER.
#   3. With NIX_NINJA_KEEP_GOING=1 and every task log read
#      -> the failing task's log is "not available", so the message that
#         would name the cause cannot be retrieved after the fact.
#
# What that BOUNDS: the consumer's failure is `No module named
# rules.generator`, which means THEIR generator task RAN and python failed
# inside it. Mine never gets a generator task built, so whatever this
# reproduces is upstream of the class and a fix fitted to it would be fitted
# to the wrong failure. Do not write the `-m` resolver against this gate
# until the gate reaches a RUNNING generator task.
#
# The mechanism itself is confirmed by reading rather than by this script:
# the sibling closure is seeded only from inputs whose build_path ends in
# `.py` (task.rs, the py_dirs loop), and a `-m` invocation names no file.
#
# Run from a checkout. Needs a builder-rpc-v0 daemon.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=$REPO
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-pygate-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
PY=$(px python3)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-pydashm-XXXXXX)
mkdir -p "$D/build" "$D/src/gen"
# A PACKAGE, reached only as `-m gen.emit`. The sibling import is what makes
# the whole directory matter rather than one file.
: > "$D/src/gen/__init__.py"
printf 'VALUE = 4242\n' > "$D/src/gen/table.py"
cat > "$D/src/gen/emit.py" <<'EOF'
from gen.table import VALUE
import sys
open(sys.argv[1], "w").write("#define GENERATED_VALUE %d\n" % VALUE)
EOF
printf '#include "gen.h"\n#include <stdio.h>\nint main(void){return GENERATED_VALUE==4242?0:1;}\n' > "$D/src/main.c"

cat > "$D/build/build.ninja" <<'EOF'
rule gen
  command = env PYTHONPATH=../src python3 -m gen.emit gen.h
rule cc
  command = gcc -I. -c $in -o $out
rule link
  command = gcc $in -o $out
build gen.h: gen
build main.o: cc ../src/main.c || gen.h
build app: link main.o
default app
EOF

cd "$D/build"
set +e
env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$PY/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $F" \
  NIX_NINJA_KEEP_GOING=1 \
  "$NN/bin/nix-ninja" app > "$D/log" 2>&1
rc=$?
set -e
echo "driver rc=$rc"

# EVERY task drv the run names, not the last one. A failure arrives as
# "1 dependency failed" naming the CONSUMER, and the diagnosis is in the
# producer's log - taking `tail -1` read the consumer and printed nothing.
# The driver names the DEPENDENT derivation (app) and nix reports "1
# dependency failed"; the message that names the class is in the failing
# DEPENDENCY's log. Re-asking nix to build the named derivation with -L
# prints every dependency's log, which `nix log <named>` never does.
task_log() {
  grep -oE '/nix/store/[a-z0-9]{32}-ninja-build\.drv(\.drv)?' "$D/log" | sort -u |
    while IFS= read -r drv; do
      nix build --extra-experimental-features "$F" -L --no-link "$drv^*" 2>&1 || true
    done
}

if [ -n "$CONTROL_REV" ]; then
  if [ "$rc" != 0 ] && { grep -q "No module named" "$D/log" || task_log | grep -q "No module named"; }; then
    echo "CONTROL PASS: the module is named by no token the seeder reads, so"
    echo "  it never reaches the sandbox."
    rm -rf "$D"; exit 0
  fi
  echo "CONTROL FAILED: expected 'No module named', got rc=$rc"
  tail -4 "$D/log"; task_log | tail -4; echo "  kept: $D"; exit 1
fi

[ "$rc" = 0 ] || { echo "FAIL: rc=$rc"; tail -6 "$D/log"; task_log | tail -6; echo "kept: $D"; exit 1; }
# THE BINARY IS THE ASSERTION, not the header's existence: a generated header
# with the wrong value compiles and returns 1, and a driver that declared the
# module but not its sibling would produce exactly that.
[ -e "$D/build/app" ] || { echo "FAIL: no binary"; echo "kept: $D"; exit 1; }
if "$D/build/app"; then
  echo "PASS: -m resolved its module AND its sibling import; the generated"
  echo "  value reached the binary."
  rm -rf "$D"; exit 0
fi
echo "FAIL: the binary ran and disagreed, so the header was generated from an"
echo "  incomplete module set."
echo "kept: $D"; exit 1
