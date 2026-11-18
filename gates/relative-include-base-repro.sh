#!/usr/bin/env bash
# A QUOTED `../`-SPELLED INCLUDE, WHERE THE BASE OF THE SPELLING DECIDES.
#
# scipy's `scipy/optimize/cython_optimize/_zeros.pyx` generates a `.c` under
# `.../cython_optimize/_zeros.cpython-*.so.p/`, and that `.c` includes its
# sibling by `#include "../Zeros/zeros.h"`. scipy's error reads like a
# declaration failure and is not one: the failing task's own NIX_NINJA_INPUTS
# carries zeros.h at the right relative path. The file IS declared and IS
# placed, under a path the compiler never looks at.
#
# WHY. A QUOTED include is resolved by the preprocessor against the INCLUDING
# FILE'S OWN DIRECTORY before any -I is tried. The includer is placed under
# the build dir:
#
#   dest = prefix.join(build_path);  prefix = /build/source/build
#
# so a header whose build_path is `../scipy/optimize/Zeros/zeros.h` lands at
# `/build/source/scipy/optimize/Zeros/zeros.h` - the `..` CLIMBS OUT of
# `build`, beside the source tree - while the includer's own spelling
# `../Zeros/zeros.h` resolves to
#
#   /build/source/build/scipy/optimize/cython_optimize/Zeros/zeros.h
#
# The two differ by exactly the `build` component. Both spellings are correct
# relative to the CWD the driver assumed; neither is correct relative to the
# includer, which is what a quoted include actually uses.
#
# THE DEEPENING GUARD IS NOT THE MECHANISM, and this fixture shows it rather
# than asserts it: the build dir is left at the tool's own default
# (/build/source/build, below_build=2) while the deepest input climbs once
# (max_up=1), so `max_up > below_build` is false and `leading_ups` never
# deepens. A deepened tree is a different shape and is not under test here.
#
# WHAT THIS GATE DOES NOT CLAIM, added after reading scipy's real source and
# finding that this fixture's geometry is not scipy's. Real scipy has NO
# `Zeros` directory under `cython_optimize/`: that directory holds only
# `c_zeros.pxd`, `__init__.py`, `meson.build`, `_zeros.pxd`, `_zeros.pyx.in`,
# and `zeros.h` is at `scipy/optimize/Zeros/zeros.h`. So `-I<cython_optimize>`
# plus `/Zeros/zeros.h` does not resolve even in the SOURCE tree, and a
# `../Zeros/zeros.h` written from an includer under `cython_optimize/<mod>.so.p/`
# does not either. The route by which scipy's compiler was expected to reach
# that header is therefore NOT reproduced here, and the mechanism behind the
# consumer's failure is OPEN.
#
# What this DOES reproduce is the error LINE, from a geometry of its own: a
# header declared at a `../`-climbing build path lands outside the build dir,
# and the includer's own quoted spelling cannot reach it. Keep the arm for
# that contract; do NOT cite it as scipy's mechanism until a fixture built on
# scipy's actual directory layout reproduces the same line.
#
# TWO ARMS OVER ONE TASK BINARY. The subject is the SAME header at the SAME
# content, declared at two spellings:
#
#   arm 1  declared at the spelling scipy's driver emits: `../`-climbing
#          -> the header lands outside the build dir; the includer cannot
#             reach it by its own quoted spelling. MUST FAIL.
#   arm 2  declared at the includer-relative spelling
#          -> the header lands where the quoted include looks. MUST PASS.
#
# Arm 2 is the CONTROL: if it does not pass, the fixture is wrong about how a
# quoted include resolves and this gate says NO ANSWER rather than scoring.
#
#   PASS       arm 1 fails and arm 2 passes (the spelling is the difference)
#   FAIL       arm 1 passes, so this fixture does not model the class
#   NO ANSWER  arm 2 fails too, so neither spelling reaches the includer
#
# NO NIX AND NO DRIVER: the task binary is run directly against a hand-built
# NIX_NINJA_INPUTS, the way the failing task receives it. Seconds. Pass
# `--control <rev>` to build the task binary from a revision instead.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
b() { nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null | tail -1; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1"; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=${NN_GATE_SRC:-$REPO}
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-scipy-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: task binary built from $CONTROL_REV"
fi

TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the task binary did not build"; exit 2; }
TASKBIN="$TASK/bin/nix-ninja-task"
[ -x "$TASKBIN" ] || { echo "NO ANSWER: no task binary at $TASKBIN"; exit 2; }
CC=$(px gcc); CU=$(px coreutils); BASH=$(px bash)

D=$(mktemp -d -t nn-scipy-XXXXXX)
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

# The source tree mirrors scipy's geometry: the header is checked in at
# scipy/optimize/Zeros/, the includer is GENERATED under the build dir at
# scipy/optimize/cython_optimize/<module>.so.p/.
SRCROOT="$D/source"
mkdir -p "$SRCROOT/scipy/optimize/Zeros"
printf '#ifndef ZEROS_H\n#define ZEROS_H\nint zeros_seed(void);\n#endif\n' > "$SRCROOT/scipy/optimize/Zeros/zeros.h"
mkdir -p "$D/gen"
printf '#include "../Zeros/zeros.h"\nint zeros_user(void){ return zeros_seed(); }\n' > "$D/gen/_zeros.c"

# THE STORE PATHS THE INPUTS ARRIVE FROM. The header and the .c are made real
# files in a directory the task copies from, standing in for the store paths a
# NIX_NINJA_INPUTS entry names. `nix store add` would be truer and costs a
# store write per arm for content that is already on disk.
# REAL STORE PATHS, because the task binary parses each entry's first field as
# a store path and refuses one that is not in the store. `nix store add` with
# --mode=flat keeps the name; a content-hashed name would not matter here but
# the entry format carries the path's BASE NAME as the last field, so keeping
# both spellings of one file distinct is what makes the two arms differ.
HDR_STORE=$(nix --extra-experimental-features "$F" store add --name zeros.h "$SRCROOT/scipy/optimize/Zeros/zeros.h")
C_STORE=$(nix --extra-experimental-features "$F" store add --name _zeros.c "$D/gen/_zeros.c")
STORE="$D/store"; mkdir -p "$STORE"
[ -n "$HDR_STORE" ] && [ -n "$C_STORE" ] || { echo "NO ANSWER: nix store add produced no path"; exit 2; }

OUT="$D/out"; mkdir -p "$OUT"

run_arm() { # $1 = arm name, $2 = the header's build_path spelling
  local name=$1 hdrbp=$2
  local bd="$D/$name/build"
  rm -rf "$D/$name"; mkdir -p "$bd"
  # The generated .c's own build_path, relative to the build dir, exactly as
  # scipy's task carries it: under the module's .so.p directory.
  local cbp='scipy/optimize/cython_optimize/_zeros.cpython-314-x86_64-linux-gnu.so.p/_zeros.c'
  (
    cd "$bd"
    export PATH="$CC/bin:$CU/bin:$BASH/bin"
    NIX_NINJA_INPUTS="$HDR_STORE:$hdrbp:
$C_STORE:$cbp:" \
    NIX_NINJA_OUTPUTS="" \
    "$TASKBIN" --build-dir "$bd" \
      "gcc -I../scipy/optimize/cython_optimize -c $cbp -o $cbp.o" \
      > "$D/$name.log" 2>&1
    echo $? > "$D/$name.rc"
  )
  # WHERE THE HEADER ACTUALLY LANDED, read from disk rather than reasoned.
  echo "$name header placed at: $(cd "$bd" && find . -name zeros.h 2>/dev/null | head -1)"
}

run_arm arm1 '../scipy/optimize/Zeros/zeros.h'
run_arm arm2 'scipy/optimize/cython_optimize/Zeros/zeros.h'

a1rc=$(cat "$D/arm1.rc"); a2rc=$(cat "$D/arm2.rc")
echo "arm 1 (driver's ../ spelling) rc=$a1rc"
echo "arm 2 (includer-relative)     rc=$a2rc"
if [ -n "$(command grep -oE '\.\./Zeros/zeros\.h: No such file' "$D/arm1.log" | head -1)" ]; then
  echo "arm 1 reproduced scipy's line: ../Zeros/zeros.h: No such file or directory"
fi

if [ "$a2rc" -ne 0 ]; then
  echo "NO ANSWER: arm 2 (the includer-relative spelling) failed too, so neither spelling reaches the includer"
  tail -5 "$D/arm2.log" | sed 's/^/  /'
  exit 2
fi
if [ "$a1rc" -eq 0 ]; then
  echo "FAIL: arm 1 PASSED, so this fixture does not model the class"
  exit 1
fi
echo "PASS: the ../ spelling places the header outside the build dir and the quoted include cannot reach it"
exit 0
