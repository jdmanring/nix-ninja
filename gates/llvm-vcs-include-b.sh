#!/usr/bin/env bash
# LLVM'S VCSRevision.h EDGE, DRIVEN FROM THE REAL 22.1.8 SCRIPTS ON
# CONFIGURATION B.
#
# `GenerateVersionFromVCS.cmake` reaches its sibling `VersionFromVCS.cmake`
# by bare name: it derives its own directory from CMAKE_SCRIPT_MODE_FILE at
# runtime, appends that to CMAKE_MODULE_PATH, and calls
# `include(VersionFromVCS)`. No configure-time variable names the module and
# the target's DEPENDS carries only the script, so nothing declared the
# module as an input and the task died:
#
#   CMake Error at .../GenerateVersionFromVCS.cmake:16 (include):
#     include could not find requested file:
#       VersionFromVCS
#
# 3169 llvm tasks stood behind that one header. `cmake_script_included_modules`
# resolves `include(<Name>)` beside the script, one level deep.
#
# WHAT IS REAL HERE AND WHAT IS NOT. The two cmake modules are copied
# verbatim from the store's llvm-src-22.1.8, and the command line is the one
# the consumer's failing task actually ran, taken from
# `nix log /nix/store/lfif2pbz9yw7b5km7g36l2ms4cwiwrsb-ninja-build.drv`, not
# composed here. What is synthetic is the surrounding tree: one ninja edge in
# llvm's directory layout rather than a full cmake configure of llvm. So this
# gate proves the reader carries the module and the header is written; it
# does not prove anything about llvm's other 3168 tasks.
#
# Configuration B (ordinary recursive-nix, /build mirror), minutes, not in
# run-all. Blanket off, so the reader is the only route that can supply the
# module.
# THE VERDICT IS THE BUILD, NOT AN INPUT COUNT. `-t drv` aborts on this
# edge ("a single derivation, but 2 targets were requested"), so its stdout
# is empty and any grep over it counts zero whatever the driver carried. The
# edge building and the header holding llvm's own two #undef lines is the
# property that matters, and it is exactly what failed before the fix.
#   PASS   the edge builds AND VCSRevision.h is written
#   FAIL   otherwise
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
#                        (3301b15 is the last revision before the fix)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
# BY ATTRIBUTE, NEVER A LITERAL STORE PATH: a literal is collected by the next
# GC and the arm answers NO ANSWER from then on (2026-10-03).
nnpkg() { nix build --no-link --print-out-paths --impure --expr \
  "(builtins.getFlake \"$(cd "$(dirname "$0")/../.." && pwd)\").inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}.$1" | tail -1; }
LLVM=${NN_LLVM_SRC:-$(nnpkg llvmPackages_22.llvm.src)}
MODULES=$LLVM/llvm/cmake/modules
for m in GenerateVersionFromVCS.cmake VersionFromVCS.cmake; do
  [ -r "$MODULES/$m" ] || { echo "FAIL: no $m under $MODULES; set NN_LLVM_SRC to an llvm source tree" >&2; exit 2; }
done

FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-llvmvcs-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-llvm-vcs-include-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf p.cmake ];
  llvmSrc = builtins.storePath \"$LLVM\";
  dontUnpack = true; dontConfigure = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    S=\$NIX_BUILD_TOP/llvm-src-22.1.8/llvm
    mkdir -p \$S/cmake/modules \$S/build/include/llvm/Support
    cp \$llvmSrc/llvm/cmake/modules/GenerateVersionFromVCS.cmake \$llvmSrc/llvm/cmake/modules/VersionFromVCS.cmake \$S/cmake/modules/
    CM=\$(command -v cmake)
    # The command line the consumer's failing task ran, verbatim.
    printf 'rule CUSTOM_COMMAND\n  command = \$COMMAND\nbuild include/llvm/Support/VCSRevision.h: CUSTOM_COMMAND\n  COMMAND = cd %s/build/include/llvm/Support && %s -DNAMES=LLVM -DLLVM_SOURCE_DIR=%s -DHEADER_FILE=%s/build/include/llvm/Support/VCSRevision.h -DLLVM_FORCE_VC_REVISION= -DLLVM_FORCE_VC_REPOSITORY= -P %s/cmake/modules/GenerateVersionFromVCS.cmake\n' \\
      \"\$S\" \"\$CM\" \"\$S\" \"\$S\" \"\$S\" > \$S/build/build.ninja
    cd \$S/build
    nix-ninja -t drv include/llvm/Support/VCSRevision.h > \$NIX_BUILD_TOP/drv.json 2>\$NIX_BUILD_TOP/drv.err || true
    if nix-ninja include/llvm/Support/VCSRevision.h > \$NIX_BUILD_TOP/build.log 2>&1; then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
    # The header is a declared output, so a successful run places it here.
    if [ -f include/llvm/Support/VCSRevision.h ]; then cp include/llvm/Support/VCSRevision.h \$NIX_BUILD_TOP/VCSRevision.h; fi
  '';
  installPhase = ''
    mkdir -p \$out
    cp \$NIX_BUILD_TOP/drv.json \$NIX_BUILD_TOP/verdict \$NIX_BUILD_TOP/build.log \$out/
    [ -f \$NIX_BUILD_TOP/VCSRevision.h ] && cp \$NIX_BUILD_TOP/VCSRevision.h \$out/ || true
  '';
}"
log=$(mktemp -t nn-llvmvcs-XXXXXX)
echo "driving llvm's VCSRevision.h edge on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths \
        --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
rc=$?
if [ $rc -ne 0 ] || [ -z "$OUT" ]; then
  echo "FAIL: the derivation did not build, so NOTHING WAS MEASURED. Tail:"
  tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"

verdict=$(cat "$OUT/verdict")
echo "result   $OUT"
echo "edge     $verdict"
if [ -f "$OUT/VCSRevision.h" ]; then
  echo "header:"; sed 's/^/  /' "$OUT/VCSRevision.h"
fi
if [ "$verdict" = built ] && [ -f "$OUT/VCSRevision.h" ]; then
  echo "PASS: the module included by bare name is carried and llvm's version header is written"
  exit 0
fi
echo "FAIL: verdict=$verdict header=$([ -f "$OUT/VCSRevision.h" ] && echo yes || echo no)"
grep -E 'could not find requested file|VersionFromVCS|ABORTED' "$OUT/build.log" | head -5
exit 1
