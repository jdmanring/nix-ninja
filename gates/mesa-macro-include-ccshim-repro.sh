#!/usr/bin/env bash
# REFUTED HYPOTHESIS, PUBLISHED AS ONE. THIS GATE IS NOT EVIDENCE FOR THE MACRO
# CLASS AND ITS PASS IS NOT A RESULT ABOUT THIS TREE.
#
# It was written to test whether the compiler drop-in's NIX_NINJA_NO_BUILD_DIR_SCAN
# flag explains mesa, and the measurement REFUTED that. It is kept because the
# fixture geometry is worth having: a bare `gcc -c` with `deps = gcc`, a header
# in the translation unit's own directory, one level up.
#
# WHY ITS PASS PROVES NOTHING FOR THE CLASS: the real defect is a macro defined
# once per `#if` arm where the compiler takes the first and the walk kept the
# last. This fixture defines its macro ONCE, so there is no arm to lose. A
# registered arm would therefore be a permanent false green, which is why
# `local/gates/registered.sh` excludes it with this reason and CLAUDE.md lists it
# as excluded rather than registered.
#
# Before it can become an arm: declare the `#if` arms in the fixture, then drive
# a real macro-class package on the branch and confirm the old driver fails it.
#
# A MACRO-NAMED HEADER IN THE TU'S OWN DIRECTORY, UNDER THE COMPILER DROP-IN.
#
# mesa 25.x builds `src/mesa/glapi/shared-glapi/core.c`, which includes its own
# directory's header by a literal quoted path and that header expands a macro
# into a second header in the same directory:
#
#   core.c:8          #include "glapi_priv.h"
#   glapi_priv.h:17   #define _GLAPI_ENTRY_ARCH_TLS_H "glapi/entry_x86_tls.h"
#
# The consumer's round 20260920-040429 failed that task with `fatal error:
# glapi/entry_x86_tls.h: No such file or directory`, and the task's own
# NIX_NINJA_INPUTS carried 27 entries, none of them `glapi_priv.h`. So the
# literal quoted include above the macro was not declared either.
#
# WHAT IS ACTUALLY TESTED HERE IS THE COMPILER DROP-IN'S FLAG, not mesa. The
# cc shim runs one driver per `cc -c` with NIX_NINJA_NO_BUILD_DIR_SCAN=1, and
# the driver's own comment records why: a gcc task takes its headers from
# deps = gcc, and one build-dir walk per compile under make -j is both the
# dominant cost and a race (libtool's transient x.loT vanished between the
# walk listing it and canonicalize). This fixture asks whether a header in the
# TU's own directory is carried WITHOUT the walk, which is the consumer's
# exact geometry.
#
# THE SCAN CANNOT SEE IT BY DESIGN, and that is why this is not a scanner
# defect. `retrieve_c_includes_checked` expands a macro-named include only
# from a path-shaped define on the command line (`8a786a1`, the qemu fix).
# This macro's value is not path-shaped, so the expansion is out of reach and
# the ONLY carriers left are the implicit-input blanket and the build-dir
# walk. The blanket is gated `!is_gcc_task` and this edge is `deps = gcc`, so
# the walk is the only one, and the shim turns it off.
#
# TWO ARMS OVER ONE FIXTURE. Arm 1 is the shim's invocation (walk off). Arm 2
# is the same compile with the walk on. Arm 2 is the CONTROL: if it does not
# carry the header either, the failure is not about the flag and this gate
# says so rather than scoring.
#
#   PASS    arm 1 builds the object
#   FAIL    arm 1 fails and arm 2 builds (the flag is the difference)
#   NO ANSWER  arm 2 fails too, or the driver aborts DaemonStalled
#
# Configuration A, seconds. The blanket limit is set to 0 in BOTH arms so the
# walk is the only variable: with the blanket live, a passing arm 1 would be
# evidence about the blanket rather than about the walk.
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
  W=$(mktemp -d -t nn-mesa-ccshim-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  GIT=$(command -v git)
  trap '"$GIT" -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-mesa-ccshim-XXXXXX)
# ONE EXIT TRAP CARRIES BOTH CLEANUPS: bash keeps only the last one armed, so a
# second trap here once dropped the control worktree removal armed above. Each
# action ends `|| true` because errexit applies inside the trap, and git is the
# path resolved above because PATH no longer holds it when the trap runs.
trap '[ "${KEEP:-}" = 1 ] || rm -rf "$D" || true; [ -z "${W:-}" ] || "$GIT" -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
# The real geometry, one directory deep exactly as mesa has it.
mkdir -p "$D/src/lib/glapi" "$D/build"
printf '#ifndef GLAPI_PRIV_H\n#define GLAPI_PRIV_H\n#define ENTRY_TLS_H "glapi/entry_x86_tls.h"\n#include ENTRY_TLS_H\nint glapi_priv_seed(void);\n#endif\n' > "$D/src/lib/glapi_priv.h"
printf '#ifndef ENTRY_X86_TLS_H\n#define ENTRY_X86_TLS_H\nint entry_tls_seed(void);\n#endif\n' > "$D/src/lib/glapi/entry_x86_tls.h"
printf '#include "glapi_priv.h"\nint core_user(void){ return glapi_priv_seed() + entry_tls_seed(); }\n' > "$D/src/lib/core.c"
# ONE EDGE, the shape the shim emits: a bare gcc -c with deps = gcc, the
# object in the build dir, the source and its headers one level up.
cat > "$D/build/build.ninja" <<'NINJA'
rule cc
  command = gcc -I../src/lib -MD -MQ $out -MF $out.d -o $out -c $in
  deps = gcc
  depfile = $out.d
build glapi/core.c.o: cc ../src/lib/core.c
NINJA

run_arm() { # $1 = arm name, $2 = 1 to enable the walk
  local name=$1 walk=$2
  ( cd "$D/build"
    rm -f glapi/core.c.o glapi/core.c.o.d
    export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
    export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    if [ "$walk" = 0 ]; then export NIX_NINJA_NO_BUILD_DIR_SCAN=1; else unset NIX_NINJA_NO_BUILD_DIR_SCAN; fi
    nix-ninja glapi/core.c.o > "$D/$name.log" 2>&1
    echo $? > "$D/$name.rc"
  )
}

run_arm arm1 0
a1rc=$(cat "$D/arm1.rc")
run_arm arm2 1
a2rc=$(cat "$D/arm2.rc")
a1obj=n; [ -f "$D/build/glapi/core.c.o" ] && a1obj=y
rm -f "$D/build/glapi/core.c.o"

# A DAEMON ABORT IS NO ANSWER ABOUT THE CLASS, never a red verdict.
for l in arm1 arm2; do
  if command grep -q 'DaemonStalled' "$D/$l.log"; then
    echo "NO ANSWER: $l aborted DaemonStalled, so this run measured nothing"
    command grep -m1 'ABORTED' "$D/$l.log"
    exit 2
  fi
done

if [ -n "$CONTROL_REV" ]; then
  if [ "$a1rc" -ne 0 ]; then
    echo "CONTROL PASS: $CONTROL_REV fails arm 1 (walk off, rc=$a1rc)"
    command grep -oE '[a-z_]+\.h: No such file or directory' "$D/arm1.log" | head -1
    exit 0
  fi
  echo "CONTROL FAILED: expected arm 1 to fail at $CONTROL_REV"; exit 1
fi

if [ "$a2rc" -ne 0 ]; then
  echo "NO ANSWER: arm 2 (the walk ON) failed too, so the walk is not the difference"
  command grep -oE '[a-z_]+\.h: No such file or directory' "$D/arm2.log" | head -1
  exit 2
fi

if [ "$a1rc" -eq 0 ] && [ "$a1obj" = y ]; then
  echo "PASS: a macro-named header in the TU's own directory is carried with the build-dir walk off"
  exit 0
fi

echo "FAIL: arm 1 rc=$a1rc object=$a1obj (arm 2 rc=$a2rc, the walk ON, built it)"
echo "  the compiler drop-in's NIX_NINJA_NO_BUILD_DIR_SCAN=1 is the difference"
command grep -oE '[a-z_]+\.h: No such file or directory' "$D/arm1.log" | head -1
exit 1
