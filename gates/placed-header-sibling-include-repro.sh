#!/usr/bin/env bash
# A PLACED HEADER'S QUOTED INCLUDE OF ITS OWN SIBLING.
#
# glslang's class: "a placed header's quoted include of its sibling", Common.h
# including "PoolAlloc.h". A quoted include resolves against the directory
# of the FILE THAT CONTAINS IT, so the sibling has to be staged beside it
# or cc1 has nowhere to look.
#
# FIVE GEOMETRIES, AND ONLY ONE FAILS, which is the whole point of running
# all of them: a single failing arm reports the error without saying which
# property produces it, and four of the obvious readings of the consumer's
# sentence build fine.
#
#   arm 1  the sibling is an ordinary source with bytes on disk
#          BUILDS. The scan opens the header and recurses.
#   arm 2  the sibling is a declared co-output of the generating edge
#          BUILDS. `co_outputs` carries it.
#   arm 3  the generator writes the sibling and no edge declares it
#          FAILS, with the consumer's own line.
#   arm 4  the headers sit above the build directory, reached through `..`
#          BUILDS. A relative build path traversing upward is placed adjacent.
#   arm 5  arm 1 with the -I one level up, so the quoted sibling has no
#          search-path fallback and must resolve beside a SYMLINKED includer
#          BUILDS. See the note at the arm: this is a retracted hypothesis
#          kept runnable, not a pin.
#
# So the reduced class is an UNDECLARED generated sibling, not a placement
# defect: at emission the file has no bytes, which is the second of the
# three points a generated file is reachable at, and nothing declares it so
# no other route carries it. The precedent with the same disposition is
# shaderc's `build-version.inc`, a package defect fixed by declaring the
# byproduct.
#
# WHAT THIS DOES NOT ESTABLISH: whether glslang's own tree has geometry 3.
# That needs glslang's build.ninja or the failing task's log. The gate
# bounds which geometry produces the reported line; it does not read the
# package.
#
# GLSLANG IS NOT ARM 3. The consumer holds the failing task's log and the
# package's headers are ordinary checked-in files, sixteen of them side by
# side in `glslang/Include`, so there is no byproduct to declare and no
# generating edge. The three arms below remain correct about what they test
# and are the wrong lead for that package; they are kept because they bound
# which geometries are safe.
#
# WHAT THE LOG SHOWS, and what it rules out. The includer is named at a
# STANDALONE CONTENT-HASHED STORE PATH (`/nix/store/<hash>-Common.h`, bytes
# identical to the source copy) while its own includer resolves inside the
# staged tree. Two facts about cpp, measured here rather than reasoned:
#
#   a header opened through a symlink has its quoted sibling searched in
#   the LINK's directory, so a symlink placement resolves the sibling and
#   builds;
#
#   when the sibling is missing, cpp names the includer by its SPELLED
#   path, never by the symlink's target.
#
# So the failure cannot come from a header placed as a symlink at its build
# path: that geometry either builds, or fails naming the build path. For cpp
# to name the store path, the path it resolved must have BEEN the store
# path. A fourth arm has to reproduce that, and what it needs is the compile
# command line, since the -I list the log records does not reach the store
# file and the spelling in the includer is `../Include/Common.h`.
#
# Configuration A, seconds. Blanket OFF, because the implicit-input blanket
# would supply the sibling and every arm would pass over a driver that
# never discovered it.
#   PASS    arms 1 and 2 build, arm 3 fails naming the sibling
#   FAIL    otherwise
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
  W=$(mktemp -d -t nn-sibinc-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-sibinc-XXXXXX); S=$D/src; mkdir -p "$S"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

cat > "$S/gen.sh" <<'GEN'
#!/bin/sh
set -e
mkdir -p "$1"
printf '#include "PoolAlloc.h"\nint common_v(void){ return pool_v(); }\n' > "$1/Common.h"
printf 'static int pool_v(void){ return 7; }\n' > "$1/PoolAlloc.h"
GEN
chmod +x "$S/gen.sh"
printf '#include "Common.h"\nint main(void){ return common_v() == 7 ? 0 : 1; }\n' > "$S/use.c"

export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0

COMPILE_RULE='rule c_COMPILER
  command = gcc -Iglslang/Include -MD -MQ $out -MF $out.d -o $out -c $in
  deps = gcc
  depfile = $out.d'

# Runs one arm. Prints the TASK log on failure, since the driver aborts
# naming a derivation and the compiler's own line lives one log down.
run_arm() {
  local name=$1 dir=$D/$1
  cd "$dir"
  set +e
  nix-ninja use.c.o > "$dir/build.log" 2>&1; ARM_RC=$?
  set -e
  ARM_MSG=""
  if [ $ARM_RC -ne 0 ]; then
    local drv
    drv=$(grep -oE '/nix/store/[a-z0-9]{32}-ninja-build\.drv' "$dir/build.log" | tail -1)
    if [ -n "$drv" ]; then
      ARM_MSG=$(nix log --extra-experimental-features "$F" "$drv" 2>/dev/null \
                | grep -aoE "PoolAlloc\.h: No such file or directory" | head -1)
    fi
  fi
}

mkdir -p "$D/arm1/glslang/Include" "$D/arm2" "$D/arm3"

# arm 1: the sibling is an ordinary source, bytes on disk, no generator.
printf '#include "PoolAlloc.h"\nint common_v(void){ return pool_v(); }\n' > "$D/arm1/glslang/Include/Common.h"
printf 'static int pool_v(void){ return 7; }\n' > "$D/arm1/glslang/Include/PoolAlloc.h"
printf '%s\nbuild use.c.o: c_COMPILER ../src/use.c\n' "$COMPILE_RULE" > "$D/arm1/build.ninja"

# arm 2: the generator declares BOTH headers as outputs.
printf 'rule genhdr\n  command = ../src/gen.sh glslang/Include\n%s\nbuild glslang/Include/Common.h glslang/Include/PoolAlloc.h: genhdr ../src/gen.sh\nbuild use.c.o: c_COMPILER ../src/use.c | glslang/Include/Common.h\n' "$COMPILE_RULE" > "$D/arm2/build.ninja"

# arm 3: the generator writes both, ninja declares only the first.
printf 'rule genhdr\n  command = ../src/gen.sh glslang/Include\n%s\nbuild glslang/Include/Common.h: genhdr ../src/gen.sh\nbuild use.c.o: c_COMPILER ../src/use.c | glslang/Include/Common.h\n' "$COMPILE_RULE" > "$D/arm3/build.ninja"

mkdir -p "$D/arm4/glslang/Include" "$D/arm4/glslang/MachineIndependent" "$D/arm4/build"
printf '#include "PoolAlloc.h"\nint common_v(void){ return pool_v(); }\n' > "$D/arm4/glslang/Include/Common.h"
printf 'static int pool_v(void){ return 7; }\n' > "$D/arm4/glslang/Include/PoolAlloc.h"
printf '#include "../Include/Common.h"\nint sym_v(void){ return common_v(); }\n' > "$D/arm4/glslang/MachineIndependent/SymbolTable.h"
printf '#include "glslang/MachineIndependent/SymbolTable.h"\nint main(void){ return sym_v()==7?0:1; }\n' > "$D/arm4/use.c"
printf 'rule c_COMPILER\n  command = gcc -I.. -MD -MQ $out -MF $out.d -o $out -c $in\n  deps = gcc\n  depfile = $out.d\nbuild use.c.o: c_COMPILER ../use.c\n' > "$D/arm4/build/build.ninja"

# arm 5. ARM 1 WITH THE -I ONE LEVEL UP, which is the package's own shape:
# glslang compiles with -Iglslang/.., never -Iglslang/Include, so a quoted
# sibling has no -I fallback and must resolve beside the includer, which is
# placed as a SYMLINK to the store.
#
# IT BUILDS, AND IT IS THE CONTROL FOR ARM 6. The hypothesis this arm was
# written to pin, that cpp resolves a symlinked header's quoted sibling beside
# the link's TARGET, is true only under SYSTEM-HEADER STATUS, which this arm
# does not have: for an ordinary header cpp searches the directory it OPENED
# the includer at, so a symlink is transparent and removing the -I fallback
# changes nothing. Arm 6 adds the pragma and fails. Both the original claim
# and its first retraction were written unconditionally, and each was right
# about half the world. The fixture that appeared to show otherwise
# compiled a precompiled header to -o /dev/null, which cannot be written, and
# the resulting error carries an "In file included from" preamble that reads
# exactly like a sibling-resolution failure. The mechanism written from it was
# retracted; this arm is what keeps that correction re-runnable rather than a
# sentence, and it is what gives arm 6 its discriminating power. The real
# reproduction is glslang-sibling-local-repro.sh.
mkdir -p "$D/arm5/glslang/Include" "$D/arm5/build"
printf '#include "PoolAlloc.h"\nint common_v(void){ return pool_v(); }\n' > "$D/arm5/glslang/Include/Common.h"
printf 'static int pool_v(void){ return 7; }\n' > "$D/arm5/glslang/Include/PoolAlloc.h"
printf '#include "glslang/Include/Common.h"\nint main(void){ return common_v()==7?0:1; }\n' > "$D/arm5/use.c"
printf 'rule c_COMPILER\n  command = gcc -I.. -MD -MQ $out -MF $out.d -o $out -c $in\n  deps = gcc\n  depfile = $out.d\nbuild use.c.o: c_COMPILER ../use.c\n' > "$D/arm5/build/build.ninja"

# arm 6. THE CONJUNCTION, and the arm that closes the class. glslang's real
# failure needs BOTH of these and neither alone reproduces it:
#   - the includer is placed as a SYMLINK into the store, which is what the
#     task binary does for a header, and
#   - the file is a SYSTEM HEADER, because CMake's generated PCH wrapper
#     carries `#pragma GCC system_header` and that status is inherited by
#     everything it includes.
# For an ordinary header gcc opens a quoted sibling relative to the path it
# OPENED the includer at, so a symlink is transparent and arm 5 builds. Under
# system-header status it resolves against the symlink's TARGET directory
# instead, which is a store path holding that one header, so the sibling is
# not there. Printed rather than reasoned: `-H` names the includer at
# `<build>/MachineIndependent/SymbolTable.h` without the pragma and at
# `/nix/store/<hash>-SymbolTable.h` with it.
# glslang has no -I fallback that could rescue it, since it compiles with
# `-Iglslang/..` and the spelling is `../Include/Common.h`, which resolves
# through that -I to a directory one level above the source root.
# BOTH COMPILES RUN HERE ON PURPOSE. The failing one alone cannot say which
# property produced it, which is the mistake that cost six readings; the
# pragma is the only difference between them.
mkdir -p "$D/arm6/store" "$D/arm6/src/glslang/MachineIndependent" \
         "$D/arm6/src/glslang/Include" "$D/arm6/src/build"
printf 'static int common_v(void){ return 7; }\n' > "$D/arm6/src/glslang/Include/Common.h"
printf '#include "../Include/Common.h"\nstatic int sym_v(void){ return common_v(); }\n' > "$D/arm6/store/SymbolTable.h"
printf '#include "SymbolTable.h"\n' > "$D/arm6/store/pch.h"
ln -s "$D/arm6/store/SymbolTable.h" "$D/arm6/src/glslang/MachineIndependent/SymbolTable.h"
ln -s "$D/arm6/store/pch.h" "$D/arm6/src/glslang/MachineIndependent/pch.h"
printf 'int main(void){ return sym_v()==7?0:1; }\n' > "$D/arm6/src/tu.cpp"
printf '/* generated by CMake */\n#pragma GCC system_header\n#include "%s/src/glslang/MachineIndependent/pch.h"\n' \
  "$D/arm6" > "$D/arm6/src/build/wrap.hxx"
# The control is WRITTEN, never derived from the subject with a text tool:
# this gate runs with a PATH that need not carry sed, and deriving it there
# left the control file empty, which reads as the pragma not discriminating.
printf '/* generated by CMake */\n#include "%s/src/glslang/MachineIndependent/pch.h"\n' \
  "$D/arm6" > "$D/arm6/src/build/wrap-nopragma.hxx"
# "SEARCH" HERE MEANS THE QUOTED-SIBLING LOOKUP, NEVER THE `-I` PATH, and
# the distinction is not pedantic: a B fixture built twice on the `-I`
# reading returned one verdict on both sides of the fix, because a header
# found through `-I` is opened at the path the search produced and relocates
# nothing. What relocates is pch.h, and SymbolTable.h is then found beside it
# in the store directory.
# THE THIRD CONJUNCT'S CONTROL: the same header, same symlink, same pragma,
# but named ABSOLUTELY by the wrapper instead of being reached by a search
# from pch.h. It builds, and that is what makes "found by search" a term in
# the rule rather than an incidental property of the chain.
printf '/* generated by CMake */\n#pragma GCC system_header\n#include "%s/src/glslang/MachineIndependent/SymbolTable.h"\n' \
  "$D/arm6" > "$D/arm6/src/build/wrap-direct.hxx"
printf 'int main(void){ return sym_v()==7?0:1; }\n' > "$D/arm6/src/tu-direct.cpp"

fails=0
run_arm arm1
if [ $ARM_RC -eq 0 ] && [ -f "$D/arm1/use.c.o" ]; then
  echo "  arm 1 OK  a source sibling with bytes on disk is scanned and carried"
else
  echo "FAIL arm 1: a source sibling on disk did not reach the compile (rc=$ARM_RC)"; fails=1
fi

run_arm arm2
if [ $ARM_RC -eq 0 ] && [ -f "$D/arm2/use.c.o" ]; then
  echo "  arm 2 OK  a sibling declared as a co-output is carried"
else
  echo "FAIL arm 2: a declared co-output sibling did not reach the compile (rc=$ARM_RC)"; fails=1
fi

run_arm arm3
if [ $ARM_RC -ne 0 ] && [ -n "$ARM_MSG" ]; then
  echo "  arm 3 OK  an undeclared generated sibling fails: $ARM_MSG"
elif [ $ARM_RC -ne 0 ]; then
  echo "FAIL arm 3: failed (rc=$ARM_RC) but not on the sibling"; fails=1
else
  echo "FAIL arm 3: an undeclared generated sibling built, so the reduction no longer holds"; fails=1
fi

# arm 4 lives one level down, since its build dir is under the source root.
D_ARM4=$D/arm4; ( cd "$D_ARM4/build" && exec true )
cd "$D/arm4/build"
set +e
nix-ninja use.c.o > "$D/arm4/build.log" 2>&1; a4=$?
set -e
if [ $a4 -eq 0 ] && [ -f "$D/arm4/build/use.c.o" ]; then
  echo "  arm 4 OK  headers declared above the build dir through .. are placed adjacent"
else
  echo "FAIL arm 4: a build path traversing above the working directory did not place (rc=$a4)"
  fails=1
fi

cd "$D/arm5/build"
set +e
nix-ninja use.c.o > "$D/arm5/build.log" 2>&1; a5=$?
set -e
if [ $a5 -eq 0 ] && [ -f "$D/arm5/build/use.c.o" ]; then
  echo "  arm 5 OK  a placed header with no -I fallback still finds its sibling"
else
  echo "FAIL arm 5: this geometry compiled when the mechanism was retracted and"
  echo "  no longer does (rc=$a5), so something moved and the retraction needs re-reading"
  fails=1
fi

cd "$D/arm6/src"
set +e
a6=$(g++ -std=c++17 -I"$D/arm6/src/glslang/.." -include "$D/arm6/src/build/wrap.hxx" \
       -o "$D/arm6/with.o" -c tu.cpp 2>&1); a6rc=$?
a6n=$(g++ -std=c++17 -I"$D/arm6/src/glslang/.." -include "$D/arm6/src/build/wrap-nopragma.hxx" \
       -o "$D/arm6/without.o" -c tu.cpp 2>&1); a6nrc=$?
set -e
set +e
a6d=$(g++ -std=c++17 -I"$D/arm6/src/glslang/.." -include "$D/arm6/src/build/wrap-direct.hxx" \
       -o "$D/arm6/direct.o" -c tu-direct.cpp 2>&1); a6drc=$?
set -e
a6msg=$(printf '%s' "$a6" | grep -oE 'store/SymbolTable[.]h:[0-9]+' | head -1)
if [ $a6rc -ne 0 ] && [ -n "$a6msg" ] && [ $a6nrc -eq 0 ] && [ $a6drc -eq 0 ]; then
  echo "  arm 6 OK  a symlinked header REACHED AS A QUOTED SIBLING of a relocated one"
  echo "            is relocated to its LINK TARGET: $a6msg. Without the pragma it"
  echo "            builds, and named absolutely rather than searched it builds too"
elif [ $a6drc -ne 0 ]; then
  echo "FAIL arm 6: the absolutely-named control failed too (rc=$a6drc), so search"
  echo "  is not a term in the rule and this gate records the mechanism too widely"
  fails=1
elif [ $a6rc -eq 0 ]; then
  echo "FAIL arm 6: the conjunction built, so the mechanism this gate records no"
  echo "  longer holds on this compiler and the class needs re-reading"
  fails=1
elif [ $a6nrc -ne 0 ]; then
  echo "FAIL arm 6: the control failed too (rc=$a6nrc), so the pragma is not the"
  echo "  discriminator here and this arm has no discriminating power"
  fails=1
else
  echo "FAIL arm 6: failed (rc=$a6rc) but not on the sibling"; fails=1
fi

if [ $fails -eq 0 ]; then
  echo "PASS: a relocated header loses the sibling it names, under system-header status"
  echo "      arms 1, 2, 4 and 5 are the geometries that reach the compile; arm 3 is"
  echo "      an undeclared generated sibling, a package defect, not a placement one"
  exit 0
fi
echo "FAIL: the three-geometry reduction does not hold as recorded"
exit 1
