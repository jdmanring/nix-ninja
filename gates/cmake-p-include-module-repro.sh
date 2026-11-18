#!/usr/bin/env bash
# A `cmake -P` SCRIPT THAT INCLUDES A SIBLING MODULE BY BARE NAME, and
# (second edge, 2026-10-09) a `cmake -P` SCRIPT NO EDGE DECLARES.
#
# curl-impersonate 2.1.1's ExternalProject mkdir edges run
# `cmake -P <build>/zlib-prefix/tmp/zlib-mkdirs.cmake`, a file CMake WROTE
# at configure time and no ninja edge names: the task arrived with ZERO
# inputs and cmake died `Not a file`. The -P reader now pushes the script
# itself as a candidate (the module reader already carried it where an edge
# declared it, which is why the first edge alone could not see this).
#
# LLVM's VCSRevision.h edge runs `cmake -P GenerateVersionFromVCS.cmake`,
# whose first statements put the script's own directory on
# CMAKE_MODULE_PATH and `include(VersionFromVCS)`. DEPENDS names only the
# script, so with the build-dir blanket off the task carried the script
# and not the module, and cmake stopped with
# `include could not find requested file: VersionFromVCS`. clang's
# lib/Basic has the same edge. The driver's -P reader now resolves the
# bare name beside the script (`cmake_script_included_modules`).
#
# Configuration A, seconds. Blanket OFF so nothing else can supply the
# module. Needs a builder-rpc-v0 daemon.
#   PASS    the edge builds and the header holds the module's define
#   FAIL    otherwise
#   --control <rev>   expects the failure (any rev before 992fd66)
#
# THE SECOND EDGE MUST BE NAMED ON THE COMMAND LINE. The first draft of the
# extension added the fixture but not the target, so the build ran only the
# original edge and the gate printed PASS having never exercised the new
# case - a green from a fixture that was never built, which is the shape
# this corpus has shipped before. If you add an edge here, add its target
# and check the build's task count moved.
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
  W=$(mktemp -d -t nn-pinc-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CMAKE=$(px cmake); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-pinc-XXXXXX); S=$D/src; mkdir -p "$S/cmake/modules" "$D/build/include"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
# LLVM's shape: the module is a sibling, reached through the script's own
# directory, and named by nothing on the edge.
cat > "$S/cmake/modules/GenerateVersionFromVCS.cmake" <<'CMK'
get_filename_component(LLVM_CMAKE_DIR "${CMAKE_SCRIPT_MODE_FILE}" PATH)
list(APPEND CMAKE_MODULE_PATH "${LLVM_CMAKE_DIR}")
include(VersionFromVCS)
file(WRITE "${HEADER_FILE}" "#define LLVM_REVISION \"${REV}\"\n")
CMK
cat > "$S/cmake/modules/VersionFromVCS.cmake" <<'CMK'
set(REV "from-the-module")
CMK
cat > "$D/build/build.ninja" <<NINJA
rule CUSTOM_COMMAND
  command = \$COMMAND
build include/VCSRevision.h: CUSTOM_COMMAND $S/cmake/modules/GenerateVersionFromVCS.cmake
  COMMAND = cd $D/build/include && cmake -DHEADER_FILE=$D/build/include/VCSRevision.h -P $S/cmake/modules/GenerateVersionFromVCS.cmake
NINJA
# THE EXTERNALPROJECT SHAPE: the script is on the COMMAND LINE ALONE. It
# is a configure-written file in the build tree, so nothing scans it and no
# edge declares it; the reader under test is what must carry it. Written
# AFTER the ninja file with a QUOTED heredoc: this block is inside the
# ninja heredoc in spirit but must not expand ${T} at write time.
cat > "$D/build/mkdirs.cmake" <<'CMK'
file(WRITE "${T}" "mkdirs-ran\n")
CMK
cat >> "$D/build/build.ninja" <<NINJA
build made-by-script: CUSTOM_COMMAND
  COMMAND = cd $D/build && cmake -DT=$D/build/made-by-script -P $D/build/mkdirs.cmake
NINJA
cd "$D/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CMAKE/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja include/VCSRevision.h made-by-script > "$D/build.log" 2>&1; brc=$?
set -e
have=0
[ -f include/VCSRevision.h ] && have=$(grep -c 'from-the-module' include/VCSRevision.h) || true
mkdir_ran=0
[ -f "$D/build/made-by-script" ] && mkdir_ran=1 || true
if [ -n "$CONTROL_REV" ]; then
  # The control predates the script-input reader, so the second edge fails
  # there; it may also predate the module reader (first edge), and the
  # control passes if EITHER shape fails, since each is the defect alone.
  if [ $brc -ne 0 ] || [ "$mkdir_ran" = 0 ]; then echo "CONTROL PASS: $CONTROL_REV cannot carry a -P target (rc=$brc module=$have script=$mkdir_ran)"; exit 0; fi
  echo "CONTROL FAILED: expected a -P edge to fail at $CONTROL_REV, rc=$brc module=$have script=$mkdir_ran"; exit 1
fi
if [ $brc -eq 0 ] && [ "$have" = 1 ] && [ "$mkdir_ran" = 1 ]; then
  echo "PASS: -P targets are carried, declared or not, and the module is read"; exit 0
fi
echo "FAIL: rc=$brc have=$have"; grep -E 'ABORTED|could not find|error' "$D/build.log" | head -3; exit 1
