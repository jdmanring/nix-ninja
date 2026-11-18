#!/usr/bin/env bash
# OPENCV'S KERNEL HEADER: AN UNDECLARED OUTPUT EVERY TU OF A MODULE INCLUDES.
#
# opencv 4.13.0 on B (KDE round, 2026-10-16), task `7i8rilmf`:
#   modules/core/src/directx.cpp:47:10: fatal error: opencl_kernels_core.hpp:
#   No such file or directory
# READ FROM THE PACKAGE: `OpenCVModule.cmake:871` declares only
# `OUTPUT opencl_kernels_<m>.cpp` ("don't add .hpp file here to optimize build
# process"); `cl2cpp.cmake:12` writes `<OUTPUT minus .cpp>.hpp` beside it. The
# kept build.ninja (`/nix/var/nix/builds/nix-25735-*`, lines 10125, 10415,
# 11596) has the compile reach the generator only through the target's
# order-only phony `cmake_object_order_depends_target_opencv_core`, which names
# the `.cpp`. Fixed by `cl2cpp_header` (undeclared output) riding the rule's
# co-outputs in `wait()`; the riding is what this gate places a break under.
#
# Configuration A, local build, seconds. Blanket OFF (the widest route that
# could supply the file). The generator is REAL cmake running a script that
# writes both files exactly as opencv's does, with the real argv shape.
#   control   k.o (compiles the DECLARED generated .cpp) builds
#   subject   a.o (includes the UNDECLARED .hpp) builds
#   pin       the same fixture under c91bbb1's driver FAILS naming k.hpp
#   PASS (0) / FAIL (1) / NO ANSWER (2)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
PIN_REV=$(git -C "$REPO" rev-parse c91bbb1)

NN=$(b "$REPO#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build"; exit 2; }
PIN_NN=$(b "git+file://$REPO?rev=$PIN_REV#nix-ninja") || { echo "NO ANSWER: the pin driver did not build"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep); CM=$(px cmake)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-cl2cpp-XXXXXX)
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

# fixture <dir>: one source tree and one build dir, opencv's layout
fixture() {
  local R=$1 S=$1/source
  mkdir -p "$S/cmake" "$S/modules/core/src" "$S/build/modules/core"
  cat > "$S/cmake/cl2cpp.cmake" <<'CM'
file(WRITE "${OUTPUT}" "int k(void) { return 1; }\n")
string(REGEX REPLACE "\\.cpp$" ".hpp" OUTPUT_HPP "${OUTPUT}")
file(WRITE "${OUTPUT_HPP}" "#define K_OK 1\n")
CM
  printf '#include "k.hpp"\nint a(void) { return K_OK; }\n' > "$S/modules/core/src/a.cpp"
  cat > "$S/build/build.ninja" <<NINJA
rule CUSTOM_COMMAND
  command = \$COMMAND
rule cxx
  command = g++ -I$S/build/modules/core -MD -MQ \$out -MF \$out.d -o \$out -c \$in
  deps = gcc
  depfile = \$out.d
build modules/core/k.cpp | $S/build/modules/core/k.cpp: CUSTOM_COMMAND $S/cmake/cl2cpp.cmake
  COMMAND = cd $S/build/modules/core && $CM/bin/cmake -DMODULE_NAME=core -DOUTPUT=$S/build/modules/core/k.cpp -P $S/cmake/cl2cpp.cmake
build order_core: phony || modules/core/k.cpp
build k.o: cxx $S/build/modules/core/k.cpp || order_core
build a.o: cxx $S/modules/core/src/a.cpp || order_core
NINJA
}
# run <driver> <dir> <target>: rc of a local drive, log at <dir>/<target>.log
run() {
  ( cd "$2/source/build"
    PATH="$1/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin:$CM/bin" \
    NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    nix-ninja "$3" > "$2/$3.log" 2>&1 )
}
# leafs <log>: the driver log plus every failed leaf's own log
leafs() {
  cat "$1"
  command grep -oE '/nix/store/[a-z0-9]{32}-[^ '"'"']*\.drv' "$1" | sort -u | while read -r d; do
    nix log --extra-experimental-features "$F" "$d" 2>/dev/null || true
  done
}

fixture "$D/head"
run "$NN" "$D/head" k.o; rc_k=$?
if [ $rc_k -ne 0 ]; then
  echo "NO ANSWER: the control k.o (declared generated .cpp) did not build (rc=$rc_k)"
  tail -4 "$D/head/k.o.log" | sed 's/^/  /'; exit 2
fi
echo "control OK: the DECLARED generated .cpp reached its compile"
run "$NN" "$D/head" a.o; rc_a=$?
leafs "$D/head/a.o.log" > "$D/head/a.all"

fixture "$D/pin"
run "$PIN_NN" "$D/pin" a.o; rc_p=$?
leafs "$D/pin/a.o.log" > "$D/pin/a.all"
P_NAMED=$(command grep -c 'k\.hpp: No such file' "$D/pin/a.all" || true)
echo "subject a.o rc=$rc_a; pin a.o rc=$rc_p, pin names k.hpp missing: $P_NAMED"

if [ $rc_p -eq 0 ] || [ "${P_NAMED:-0}" -lt 1 ]; then
  echo "FAIL: the pin does not fail on the missing k.hpp, so the fixture does not model the class"
  exit 1
fi
if [ $rc_a -ne 0 ]; then
  echo "FAIL: the undeclared kernel header did not reach the compile that includes it"
  command grep -m2 -E 'k\.hpp|ABORTED' "$D/head/a.all" | sed 's/^/  /'
  exit 1
fi
echo "PASS: the cl2cpp header rides the generator's co-outputs to its includer, and the pin fails without it"
