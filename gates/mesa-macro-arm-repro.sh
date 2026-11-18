#!/usr/bin/env bash
# A MACRO DEFINED ONCE PER ARM, WHERE THE COMPILER TAKES THE FIRST AND THE
# SCANNER KEPT THE LAST.
#
# THIS GATE EXISTS BECAUSE THE ONE BESIDE IT DOES NOT REPRODUCE THE CLASS.
# `mesa-macro-include-ccshim-repro.sh` defines its macro ONCE, so there is no
# arm to lose and it passes at `37672a4` and every revision after it. A gate
# that passes on the defect it models asserts nothing, which is why that one
# is EXCLUDED rather than registered.
#
# THE DEFECT, in the two commits that fix it, one predicate in two crates:
#
#   04539e8  deps-infer/src/c_include_parser.rs  (main)
#     "scan_directives collected same-file defines into a map keyed by name
#      and inserted, so the last #define of a name replaced the earlier ones"
#   e4b2223  crates/nix-ninja/src/task.rs        (branch)
#     "that traversal keeps one value per name ... the compiler takes the x86
#      arm, the walk kept the ppc64le one that follows it"
#
#   PASS       the arm the compiler takes is carried
#   FAIL       the carried arm is the other one
#   NO ANSWER  neither arm is carried, or the driver aborts DaemonStalled
#
# CONFIGURATION A, seconds. THE WALK IS OFF IN BOTH ARMS and the blanket limit
# is 0, so the SCANNER'S DECLARATION is the only carrier of a header. That is
# the point: with the walk on, a build-dir walk supplies both headers and the
# arm the scanner kept stops mattering, which is how a broken fixture reads as
# a pass.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-4}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=${NN_GATE_SRC:-$REPO}
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-macro-arm-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-macro-arm-XXXXXX)
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

# THE REAL GEOMETRY: mesa's `src/mesa/glapi/glapi_priv.h` defines
# `_GLAPI_ENTRY_ARCH_TLS_H` once per `#if DETECT_ARCH_*` arm, and
# `shared-glapi/core.c` includes the header by the macro.
mkdir -p "$D/src/glapi/shared-glapi" "$D/build"
cat > "$D/src/glapi/glapi_priv.h" <<'H'
#ifndef GLAPI_PRIV_H
#define GLAPI_PRIV_H
#if defined(DETECT_X86)
#define ENTRY_TLS_H "glapi/entry_x86_tls.h"
#elif defined(DETECT_PPC64LE)
#define ENTRY_TLS_H "glapi/entry_ppc64le_tls.h"
#endif
#include ENTRY_TLS_H
int glapi_priv_seed(void);
#endif
H
# BOTH ARM HEADERS ARE CHECKED IN, which is what makes this fixture measure
# the ARM CHOICE rather than whether a generated file was produced. mesa's
# x86 arm is generated and its ppc64le arm is checked in; taking that shape
# would fold a second class into this one.
printf '#ifndef ENTRY_X86_TLS_H\n#define ENTRY_X86_TLS_H\nint entry_x86_seed(void);\n#endif\n' > "$D/src/glapi/entry_x86_tls.h"
printf '#ifndef ENTRY_PPC64LE_TLS_H\n#define ENTRY_PPC64LE_TLS_H\nint entry_ppc64le_seed(void);\n#endif\n' > "$D/src/glapi/entry_ppc64le_tls.h"
# The compiler takes the FIRST arm; the scan kept the LAST.
printf '#include "glapi_priv.h"\nint core_user(void){ return glapi_priv_seed() + entry_x86_seed(); }\n' > "$D/src/glapi/shared-glapi/core.c"

cat > "$D/build/build.ninja" <<'NINJA'
rule cc
  command = gcc -DDETECT_X86 -I../src -I../src/glapi -MD -MQ $out -MF $out.d -o $out -c $in
  deps = gcc
  depfile = $out.d
build glapi/shared-glapi/core.c.o: cc ../src/glapi/shared-glapi/core.c
NINJA

run_arm() { # $1 = arm name, $2 = 1 to enable the walk
  local name=$1 walk=$2
  ( cd "$D/build"
    rm -f glapi/shared-glapi/core.c.o glapi/shared-glapi/core.c.o.d
    export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
    export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    if [ "$walk" = 0 ]; then export NIX_NINJA_NO_BUILD_DIR_SCAN=1; else unset NIX_NINJA_NO_BUILD_DIR_SCAN; fi
    nix-ninja glapi/shared-glapi/core.c.o > "$D/$name.log" 2>&1
    echo $? > "$D/$name.rc"
  )
}

run_arm arm1 0
a1rc=$(cat "$D/arm1.rc")
run_arm arm2 1
a2rc=$(cat "$D/arm2.rc")

for a in arm1 arm2; do
  if command grep -q 'DaemonStalled' "$D/$a.log"; then
    echo "NO ANSWER: $a aborted DaemonStalled, so this run measured nothing"
    command grep -m1 'ABORTED' "$D/$a.log"
    exit 2
  fi
done

# WHICH ARM WAS CARRIED, read from the failing text rather than assumed.
want=$(command grep -oE "glapi/entry_[a-z0-9]+_tls\.h: No such file" "$D/arm1.log" | head -1)
if [ -n "$want" ]; then
  echo "arm 1 carried the OTHER arm: $want"
fi

if [ -n "$CONTROL_REV" ]; then
  if [ "$a1rc" -ne 0 ]; then
    echo "CONTROL PASS: $CONTROL_REV fails arm 1 (rc=$a1rc), so the fixture reproduces the class"
    exit 0
  fi
  echo "CONTROL FAILED: arm 1 PASSED at $CONTROL_REV, so this fixture does not reproduce the class"
  exit 1
fi

if [ "$a2rc" -ne 0 ]; then
  echo "NO ANSWER: arm 2 (the walk ON) failed too, so the arm choice is not the difference"
  command grep -m1 'error' "$D/arm2.log"
  exit 2
fi

if [ "$a1rc" -eq 0 ]; then
  echo "PASS: the arm the compiler takes is carried with the build-dir walk off"
  exit 0
fi

echo "FAIL: arm 1 rc=$a1rc (arm 2, the walk ON, built it)"
echo "  the scanner declared one arm of a multi-arm macro and the compiler took the other"
exit 1
