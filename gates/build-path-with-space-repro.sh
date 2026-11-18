#!/usr/bin/env bash
# AN OUTPUT DIRECTORY WHOSE NAME CONTAINS A SPACE.
#
# libepoxy 1.5.10, compiler route, consumer round 7. meson names a target
# directory after its target, so `test/khronos typedefs.p` is an ordinary
# directory name, and the compile writes
# `test/khronos typedefs.p/khronos_typedefs.c.o`. The driver's encoded lists
# (NIX_NINJA_INPUTS, NIX_NINJA_OUTPUTS, NIX_NINJA_ALIASES, NIX_NINJA_MAKE_DIRS)
# joined their ENTRIES with a space, so one entry split into two and the tail
# reached the decoder alone:
#
#   Error: Parsing encoded store path
#   Caused by: parse error typedefs.p/khronos_typedefs.c.o,
#              non-absolute store path "typedefs.p/khronos_typedefs.c.o"
#
# THAT MESSAGE NAMES THE STORE-PATH PARSE AND THE SPLIT IS ONE LAYER ABOVE IT,
# which is why the class was filed against the shim for as long as it was.
#
# WHY A UNIT TEST DOES NOT COVER THIS. `split_encoded_list` has its own tests
# and a mutant restoring `" "` fails them, but nothing there constructs a
# derivation. Measured: reverting the NIX_NINJA_OUTPUTS writer alone to
# `join(" ")` SURVIVES the whole workspace suite, all 22 targets. The rule is
# pinned and the wiring is not, which is this tree's standing shape, so the
# reduction is the only instrument for the emit-and-read path.
#
# THE FIXTURE ESCAPES THE SPACE, AND THAT IS THE WHOLE POINT. ninja needs `$ `
# for a space inside a build statement, and `nn-cc-shim.sh` line 135 now emits
# that. An unescaped fixture reproduces the SHIM defect instead, which ninja
# rejects too, and would report a red that says nothing about the driver. REAL
# NINJA IS THE CONTROL: if it cannot build this graph the fixture is broken and
# the gate reports NO ANSWER rather than a verdict.
#
# The `-I` naming the same directory is deliberate: that spelling travels in
# NIX_NINJA_MAKE_DIRS, a different one of the four lists, so a fix that
# repaired only the input list still fails here.
#
# Configuration A, seconds. Blanket OFF, since the implicit-input blanket is
# the widest carrier and would supply files for the wrong reason.
#   PASS       the object builds at its space-named path
#   FAIL       otherwise
#   NO ANSWER  real ninja cannot build the fixture, or the daemon aborted
#   --control <rev>   expects the failure and names which defect: the
#                     encoded-entry split (before f241387) or the shell
#                     split of an unquoted \$out (before 81243e2)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-4}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=$REPO
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-space-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || exit 2
TASK=$(b "$SRC#nix-ninja-task") || exit 2
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NINJA=$(px ninja)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-space-XXXXXX); S=$D/src
# THE SPACE IS IN THE DIRECTORY NAME, as meson writes it.
DIR='khronos typedefs.p'
mkdir -p "$S" "$D/build/test/$DIR"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf '#ifndef SEEDH\n#define SEEDH\nint seed(void);\n#endif\n' > "$D/build/test/$DIR/seed.h"
printf '#include "seed.h"\nint user(void){ return seed(); }\n' > "$S/khronos_typedefs.c"

# `$ ` is ninja's escape for a space inside a build statement, which is what
# the shim emits since its escaping patch. The `-I` is quoted for the shell
# the command runs under, and names the same space-bearing directory.
cat > "$D/build/build.ninja" <<NINJA
rule c_COMPILER
  command = gcc '-Itest/$DIR' -MD -MQ \$out -MF \$out.d -o \$out -c \$in
  deps = gcc
  depfile = \$out.d
build test/khronos\$ typedefs.p/khronos_typedefs.c.o: c_COMPILER ../src/khronos_typedefs.c
NINJA

OBJ="test/$DIR/khronos_typedefs.c.o"
cd "$D/build"

# CONTROL: real ninja must build this graph, or the fixture is malformed and a
# red verdict below would be about the file rather than about the driver.
set +e
PATH="$CC/bin:$CU/bin:$BASH/bin" "$NINJA/bin/ninja" "$OBJ" > "$D/ninja.log" 2>&1; nrc=$?
set -e
if [ $nrc -ne 0 ] || [ ! -f "$OBJ" ]; then
  echo "NO ANSWER: real ninja could not build the fixture (rc=$nrc), so it is"
  echo "  malformed and says nothing about the driver. The space must be"
  echo "  escaped as \$(printf '$ ') in the build statement."
  tail -3 "$D/ninja.log"
  exit 2
fi
rm -f "$OBJ" "$OBJ.d"

export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja "$OBJ" > "$D/build.log" 2>&1; brc=$?
set -e

if command grep -q 'DaemonStalled' "$D/build.log"; then
  echo "NO ANSWER: the driver aborted DaemonStalled, so this run measured nothing"
  echo "  Read the worker crash delta before concluding the machine was busy:"
  echo "    grep -c 'daemon worker .* crashed' /var/log/nix-daemon/current"
  exit 2
fi

# TWO DEFECTS SHARE THIS FIXTURE AND EACH HAS ITS OWN SENTENCE. Before
# f241387 the task's decoder split the encoded entry and printed the
# store-path parse error. From f241387 to 81243e2 the task set up fine and
# the COMMAND split at the space instead: n2 expanded `$out` bare where
# ninja shell-quotes it, and gcc refused `-o` with two files. The first
# run of this gate (2026-09-19, on 8a786a1) found the second by being red
# after the first was closed. The control names which one it reproduced.
msg=$(command grep -oE 'non-absolute store path "[^"]*"' "$D/build.log" | head -1)
# gcc DECORATES its own sentence: the `-o` is wrapped in colour codes and
# an OSC 8 hyperlink even with no tty, so a grep for the literal `'-o'`
# misses it. Anchor on the two plain phrases either side of the decoration.
# The exported PATH carries the store tools alone, so no sed: the two plain
# phrases are the match and the decorated middle is not printed.
split=$(command grep -oE "cannot specify .*with multiple files" "$D/build.log" | head -1 | command grep -oE "^cannot specify|with multiple files$" | tr '\n' ' ')

if [ -n "$CONTROL_REV" ]; then
  if [ $brc -ne 0 ]; then
    if [ -n "$msg" ]; then
      echo "CONTROL PASS: $CONTROL_REV fails the compile (rc=$brc), encoded-entry split (before f241387)"
      echo "  $msg"; exit 0
    fi
    if [ -n "$split" ]; then
      echo "CONTROL PASS: $CONTROL_REV fails the compile (rc=$brc), unquoted \$out at the shell (before 81243e2)"
      echo "  $split"; exit 0
    fi
    echo "CONTROL FAILED: $CONTROL_REV fails the compile (rc=$brc) but with NEITHER known sentence; read $D/build.log"
    exit 1
  fi
  echo "CONTROL FAILED: expected the compile to fail at $CONTROL_REV"; exit 1
fi

if [ $brc -eq 0 ] && [ -f "$OBJ" ]; then
  echo "PASS: a build path containing a space survives the encoded lists"
  exit 0
fi
echo "FAIL: rc=$brc obj=$([ -f "$OBJ" ] && echo y || echo n)"
[ -n "$msg" ] && echo "  $msg"
command grep -E 'ABORTED|Error:|Caused by' "$D/build.log" | head -4
exit 1
