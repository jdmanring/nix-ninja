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
#
# A SECOND SUBJECT (34038d2 arms, guarded at `e93c9da` and `425778a`; the
# fixture reads the same at all three): the same fixture now runs a REAL configure step,
# `cmake -GNinja -S <SOURCE_DIR> -B <BINARY_DIR>`, so it also witnesses the two
# arms that give an ExternalProject step its ninja and its binary dir. The
# configure step must carry ninja on its emitted PATH and declare BINARY_DIR as
# an output, and the build step must carry BINARY_DIR in its inputs. At the pin
# BOTH are absent and the configure step dies `CMAKE_MAKE_PROGRAM is not set`,
# which is the string the real curl-impersonate drv died on.
#
#   NN_GATE_SRC AND NN_PROBE_REV ARE MUTUALLY EXCLUSIVE, and NN_GATE_SRC WINS,
#   so setting both silently runs the control against the wrong tree. That is
#   not a warning: it produced a FAKE PASS on 2026-10-12, the control reporting
#   PASS with the SAME OUTPUT PATH as the head run. The gate now refuses when
#   both are set rather than preferring one.
set -u
# EVERY TEMP THIS GATE MAKES IS REMOVED, and it used to leak four files per run:
# CML/DATAF/PATCHF are created and never removed, so a corpus of runs left about
# a hundred orphans in /tmp (found 2026-10-12 while checking the tree was clean).
# The worktree trap below is a SEPARATE trap and replaces EXIT's old body, so
# this one accumulates into a list instead of being clobbered.
TMP_CLEAN=()
trap 'rm -rf "${TMP_CLEAN[@]}" 2>/dev/null || true' EXIT
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-epsrc-wt-XXXXXX); TMP_CLEAN+=("$W")
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true; rm -rf "${TMP_CLEAN[@]}" 2>/dev/null || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
if [ -n "${NN_GATE_SRC:-}" ] && [ -n "${NN_PROBE_REV:-}" ]; then
  echo "NO ANSWER: NN_GATE_SRC and NN_PROBE_REV are both set; NN_GATE_SRC would override the control worktree and the control would run the head tree (this produced a fake PASS with an identical output path on 2026-10-12)"; exit 2
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

# THE DEPENDENCY'S OWN CMakeLists IS INLINE, NOT A FILE IN THE PRIVATE TREE.
# It used to be read from `local/gates/fixtures/`, which a clone does not
# carry: the path reached `builtins.path` inside the derivation expression, so
# a clone got a nix eval stack trace from constructing the expression, before
# the NO ANSWER guard below could run. Measured 2026-11-18 by audit.
DEPCML=$(mktemp -t nn-epsrc-depcml-XXXXXX); TMP_CLEAN+=("$DEPCML")
cat > "$DEPCML" <<'DEPEOF'
cmake_minimum_required(VERSION 3.20)
project(dep C)
file(WRITE ${CMAKE_BINARY_DIR}/seen2.txt "ninja=${CMAKE_MAKE_PROGRAM}")
DEPEOF
CML=$(mktemp -t nn-epsrc-cml-XXXXXX); TMP_CLEAN+=("$CML")
cat > "$CML" <<'EOF'
cmake_minimum_required(VERSION 3.20)
project(ep NONE)
include(ExternalProject)
ExternalProject_Add(dep
  URL "file://@ARCHIVE@"
  SOURCE_DIR "${CMAKE_BINARY_DIR}/deps/src/dep"
  PATCH_COMMAND patch -p1 -i "${CMAKE_SOURCE_DIR}/patches/dep.patch"
  CONFIGURE_COMMAND ${CMAKE_COMMAND} -GNinja -DCMAKE_C_COMPILER=@CC@ -S <SOURCE_DIR> -B <BINARY_DIR>
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
  depCml = builtins.path { path = $DEPCML; name = \"dep-CMakeLists.txt\"; };
  ccb = p.gcc;
  depPatch = builtins.path { path = $PATCHF; name = \"dep.patch\"; };
  cmakelists = builtins.path { path = $CML; name = \"CMakeLists.txt\"; };
  archive = p.runCommand \"dep-1.0.tar.gz\" { nativeBuildInputs = [ p.gnutar p.gzip ]; } ''
    mkdir dep-1.0; cp \${dataTxt} dep-1.0/data.txt
    cp \${depCml} dep-1.0/CMakeLists.txt
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
    sed -e \"s|@ARCHIVE@|\${archive}|\" -e \"s|@CC@|\${ccb}/bin/gcc|\" \${cmakelists} > \$S/CMakeLists.txt
    cd \$S/build
    cmake -G Ninja .. > \$NIX_BUILD_TOP/configure.log 2>&1 || { echo configure-failed > \$NIX_BUILD_TOP/verdict; }
    nix-ninja -t drv dep-prefix/src/dep-stamp/dep-download > \$NIX_BUILD_TOP/dl.json 2>\$NIX_BUILD_TOP/dl.err || true
    nix-ninja -t drv dep-prefix/src/dep-stamp/dep-build > \$NIX_BUILD_TOP/build-step.json 2>/dev/null || true
    nix-ninja -t drv dep-prefix/src/dep-stamp/dep-configure > \$NIX_BUILD_TOP/cfg.json 2>/dev/null || true
    if [ ! -e \$NIX_BUILD_TOP/verdict ]; then
      if nix-ninja seen.txt > \$NIX_BUILD_TOP/build.log 2>&1; then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
    fi
    cat seen.txt > \$NIX_BUILD_TOP/seen 2>/dev/null || true
  '';
  installPhase = ''mkdir -p \$out; cp \$NIX_BUILD_TOP/verdict \$NIX_BUILD_TOP/configure.log \$NIX_BUILD_TOP/dl.json \$NIX_BUILD_TOP/dl.err \$NIX_BUILD_TOP/build-step.json \$NIX_BUILD_TOP/cfg.json \$out/; cp \$NIX_BUILD_TOP/build.log \$NIX_BUILD_TOP/seen \$out/ 2>/dev/null || true'';
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
# THE TWO ExternalProject ARMS, read off the emitted derivations rather than inferred.
# configure: BINARY_DIR declared as an output, ninja on the emitted PATH.
# build step: BINARY_DIR carried in its inputs.
read -r cfg_bindir cfg_ninja bst_carries <<EOF
$(python3 -c "
import json
def load(p):
    try: return json.load(open(p))
    except Exception: return {}
cfg=load('$OUT/cfg.json'); bst=load('$OUT/build-step.json')
o=n=c=0
for v in cfg.values():
    e=v['env']
    if any(l.split(':')[1]=='dep-prefix/src/dep-build' for l in e.get('NIX_NINJA_OUTPUTS','').split(chr(10)) if l): o=1
    if any('ninja-1.13' in x for x in e.get('PATH','').split(':')): n=1
for v in bst.values():
    if any(l.split(':')[1]=='dep-prefix/src/dep-build' for l in v['env'].get('NIX_NINJA_INPUTS','').split(chr(10)) if l): c=1
print(o,n,c)")
EOF
echo "arms at this revision: configure declares BINARY_DIR=${cfg_bindir} ninja-on-PATH=${cfg_ninja} build-step carries BINARY_DIR=${bst_carries}"
if [ "$seen" = "marker=patched" ]; then
  if [ "${cfg_bindir}" = 1 ] && [ "${cfg_ninja}" = 1 ] && [ "${bst_carries}" = 1 ]; then
    echo "PASS: the SOURCE_DIR tree reaches the later steps, and both ExternalProject arms fire on B"; exit 0
  fi
  echo "FAIL: the build succeeded but an arm did not fire (BINARY_DIR=${cfg_bindir} ninja=${cfg_ninja} carrier=${bst_carries})"; exit 1
fi

grep -E "can't find|ABORTED|FAILED|File not found" "$OUT/build.log" 2>/dev/null | head -4 | sed 's/^/  /'
exit 1
