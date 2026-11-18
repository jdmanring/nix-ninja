#!/usr/bin/env bash
# AN EXTERNALPROJECT'S DOWNLOAD STEP EXTRACTS INTO SOURCE_DIR AND DECLARES
# ONLY ITS STAMP, on configuration B (the consumer's route).
#
# curl-impersonate 2.1.1 (XFCE round at pin 16a3e01, drv xhfqi93w): every
# file of a valid patch failed `can't find file to patch`; the kept tree had
# `deps/src/boringssl/` empty while the download stamp was in the store. The
# steps ran in order. The extracted tree was discarded because a task's store
# output holds declared outputs only.
#
# WHY B AND NOT A: the archive reaches the download step only because the
# uploaded verify/extract scripts name it and an upload made INSIDE a
# derivation is reference-scanned (`nix-builder-rpc-client`, the
# in-derivation branch). On A the client passes an empty reference set, so
# the archive never reaches the sandbox and the step fails before the class
# is reached. A real CMake ExternalProject with a store-path archive (as
# nixpkgs rewrites curl-impersonate's URLs), a PATCH_COMMAND that edits a file
# from the archive, and a BUILD_COMMAND that copies the patched file out.
# Scored on the BYTES the build step produced.
#   PASS   the patched marker reaches the build step
#   FAIL   otherwise
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
set -u
# EVERY TEMP THIS GATE MAKES IS REMOVED, and it used to leak four files per run:
# CML/DATAF/PATCHF are created and never removed, so a corpus of runs left about
# a hundred orphans in /tmp (found 2026-10-12 while checking the tree was clean).
# The worktree trap below is a SEPARATE trap and replaces EXIT's old body, so
# this one accumulates into a list instead of being clobbered.
TMP_CLEAN=()
trap 'rm -rf "${TMP_CLEAN[@]}" 2>/dev/null || true' EXIT
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-epsrc-wt-XXXXXX); TMP_CLEAN+=("$W")
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true; rm -rf "${TMP_CLEAN[@]}" 2>/dev/null || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

CML=$(mktemp -t nn-epsrc-cml-XXXXXX); TMP_CLEAN+=("$CML")
cat > "$CML" <<'EOF'
cmake_minimum_required(VERSION 3.20)
project(ep NONE)
include(ExternalProject)
ExternalProject_Add(dep
  URL "file://@ARCHIVE@"
  SOURCE_DIR "${CMAKE_BINARY_DIR}/deps/src/dep"
  PATCH_COMMAND patch -p1 -i "${CMAKE_SOURCE_DIR}/patches/dep.patch"
  CONFIGURE_COMMAND ""
  BUILD_COMMAND ${CMAKE_COMMAND} -E copy <SOURCE_DIR>/data.txt ${CMAKE_BINARY_DIR}/seen.txt
  BUILD_BYPRODUCTS ${CMAKE_BINARY_DIR}/seen.txt
  INSTALL_COMMAND ""
)
EOF
DATAF=$(mktemp -t nn-epsrc-data-XXXXXX); TMP_CLEAN+=("$DATAF")
printf 'marker=original\n' > "$DATAF"
PATCHF=$(mktemp -t nn-epsrc-patch-XXXXXX); TMP_CLEAN+=("$PATCHF")
printf -- '--- a/data.txt\n+++ b/data.txt\n@@ -1 +1 @@\n-marker=original\n+marker=patched\n' > "$PATCHF"
EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
  dataTxt = builtins.path { path = $DATAF; name = \"data.txt\"; };
  depPatch = builtins.path { path = $PATCHF; name = \"dep.patch\"; };
  cmakelists = builtins.path { path = $CML; name = \"CMakeLists.txt\"; };
  archive = p.runCommand \"dep-1.0.tar.gz\" { nativeBuildInputs = [ p.gnutar p.gzip ]; } ''
    mkdir dep-1.0; cp \${dataTxt} dep-1.0/data.txt
    tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -cf - dep-1.0 | gzip -n > \$out
  '';
in p.stdenv.mkDerivation {
  pname = \"nn-external-project-source-dir-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf p.cmake p.ninja p.gnupatch ];
  dontUnpack = true; dontFixup = true; dontUseCmakeConfigure = true; dontUseNinjaBuild = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    S=\$NIX_BUILD_TOP/source; mkdir -p \$S/patches \$S/build
    cp \${depPatch} \$S/patches/dep.patch
    sed \"s|@ARCHIVE@|\${archive}|\" \${cmakelists} > \$S/CMakeLists.txt
    cd \$S/build
    cmake -G Ninja .. > \$NIX_BUILD_TOP/configure.log 2>&1 || { echo configure-failed > \$NIX_BUILD_TOP/verdict; }
    nix-ninja -t drv dep-prefix/src/dep-stamp/dep-download > \$NIX_BUILD_TOP/dl.json 2>\$NIX_BUILD_TOP/dl.err || true
    nix-ninja -t drv dep-prefix/src/dep-stamp/dep-build > \$NIX_BUILD_TOP/build-step.json 2>/dev/null || true
    if [ ! -e \$NIX_BUILD_TOP/verdict ]; then
      if nix-ninja seen.txt > \$NIX_BUILD_TOP/build.log 2>&1; then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
    fi
    cat seen.txt > \$NIX_BUILD_TOP/seen 2>/dev/null || true
  '';
  installPhase = ''mkdir -p \$out; cp \$NIX_BUILD_TOP/verdict \$NIX_BUILD_TOP/configure.log \$NIX_BUILD_TOP/dl.json \$NIX_BUILD_TOP/dl.err \$NIX_BUILD_TOP/build-step.json \$out/; cp \$NIX_BUILD_TOP/build.log \$NIX_BUILD_TOP/seen \$out/ 2>/dev/null || true'';
}"
log=$(mktemp -t nn-epsrc-XXXXXX); TMP_CLEAN+=("$log")
echo "building an ExternalProject on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
if [ $? -ne 0 ] || [ -z "$OUT" ]; then
  echo "NO ANSWER: the derivation did not build, so nothing was measured. Tail:"; tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"
echo "output: $OUT"
verdict=$(cat "$OUT/verdict")
seen=$(cat "$OUT/seen" 2>/dev/null)
declared=$(python3 -c '
import json,sys
try: d=json.load(open(sys.argv[1]))
except Exception: sys.exit(0)
for v in d.values():
    for e in v["env"].get("NIX_NINJA_OUTPUTS","").split("\n"):
        if e: print(e.split(":")[1])' "$OUT/dl.json" | tr '\n' ' ')
echo "verdict=$verdict seen='$seen' download step declares: $declared"
if [ "$verdict" = configure-failed ]; then
  echo "NO ANSWER: cmake configure failed inside the derivation"; tail -5 "$OUT/configure.log" | sed 's/^/  /'; exit 2
fi
if [ "$seen" = "marker=patched" ]; then
  echo "PASS: the extracted SOURCE_DIR reaches the patch and build steps on B"; exit 0
fi
echo "FAIL: the extracted tree did not reach the later steps"
grep -E "can't find|ABORTED|FAILED|File not found" "$OUT/build.log" 2>/dev/null | head -4 | sed 's/^/  /'
exit 1
