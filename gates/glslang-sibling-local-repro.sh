#!/usr/bin/env bash
# GLSLANG'S SIBLING FAILURE, REPRODUCED WITH NO DAEMON AND NO SANDBOX.
#
# The failing task derivation is self contained, so it can be rebuilt with
# --keep-failed and its tree copied out. Once the generated PCH's absolute
# /build/source spellings are rewritten to the copy, the compile fails
# OUTSIDE nix entirely, deterministically, in about a second:
#
#   /nix/store/<h>-SymbolTable.h:68:10: fatal error:
#     ../Include/Common.h: No such file or directory
#
# WHAT MAKES THIS WORTH KEEPING. Every input is placed correctly: pch.h,
# SymbolTable.h and all twelve headers under Include/ are present as symlinks
# at their declared build paths, adjacent, exactly as the derivation declares
# them. Yet a header found by search is named at its STORE path and its own
# relative include then resolves from /nix/store.
#
# WHAT IS NOT YET KNOWN: why. Five hypotheses have been tested against
# synthetic fixtures and ALL FIVE BUILD, so none of them is the mechanism:
#   a parent reference in the include spelling
#   a symlinked header
#   a symlinked INCLUDER
#   a header found by directory search rather than named absolutely
#   the compiler version (host and the sandbox's own gcc agree)
# The gap between this reproduction and every synthetic one is the thing to
# find, and this script exists so the next session starts from the failure
# rather than from the six wrong readings that preceded it.
#
# BEWARE ONE FIXTURE FAULT THAT COST HOURS HERE: a PCH cannot be written to
# /dev/null, and `-o /dev/null` with `-x c++-header` fails with a message
# whose "In file included from" preamble reads exactly like a sibling
# resolution failure. Always give it a real output path.
#
#   PASS  the compile fails naming the includer at a /nix/store path
#   FAIL  it builds, or fails some other way
set -u
DRV=${NN_GLSLANG_DRV:-/nix/store/547bxb2gri5ss5s0wcb79mci42z26f2p-ninja-build.drv}
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
[ -e "$DRV" ] || { echo "NO ANSWER: $DRV is not in the store"; exit 2; }

KEPT=$(nix build --extra-experimental-features "$F" --keep-failed --no-link \
        --max-jobs 1 --cores 2 "$DRV^*" 2>&1 | grep -oE '/nix/var/nix/builds/[^"]*/build' | head -1)
[ -n "$KEPT" ] || { echo "NO ANSWER: the rebuild kept no directory"; exit 2; }

T=$(mktemp -d -t glslang-repro-XXXXXX)
cp -a "$KEPT/source" "$T/" || { echo "NO ANSWER: could not copy the kept tree"; exit 2; }
P=$T/source/build/glslang/CMakeFiles/glslang.dir/cmake_pch.hxx
[ -f "$P" ] || { echo "NO ANSWER: no generated pch in the kept tree"; exit 2; }
sed "s|/build/source|$T/source|g" "$P" > "$P.local"

CXX=$(sed -n 's/.*"\(\/nix\/store\/[a-z0-9]*-gcc-wrapper[^"]*\)\/bin.*/\1/p' "$DRV" | head -1)/bin/g++
[ -x "$CXX" ] || CXX=g++
cd "$T/source/build" || exit 2
out=$("$CXX" -std=c++17 -x c++-header -I"$T/source/glslang/.." -include "$P.local" \
        -o "$T/out.gch" -c "$T/source/build/glslang/CMakeFiles/glslang.dir/cmake_pch.hxx.cxx" 2>&1 \
      | sed -r 's/\x1b\[[0-9;]*[A-Za-z]//g')
inc=$(printf '%s' "$out" | grep -oE '^/nix/store/[a-z0-9]{32}-[A-Za-z_]+\.h:[0-9]+' | head -1)
echo "kept tree: $T"
if [ -n "$inc" ]; then
  echo "  includer named at a store path: $inc"
  echo "PASS: the sibling failure reproduces with no daemon and no sandbox"
  exit 0
fi
echo "FAIL: no store-path includer in the output"
printf '%s\n' "$out" | head -6
exit 1
