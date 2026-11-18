#!/usr/bin/env bash
# THE COMPILER DROP-IN'S SHAPE ON CONFIGURATION B: a header reached through
# an ABSOLUTE symlink in build/include, an ABSOLUTE -I, the walk off.
#
# rdma-core links every public header into build/include at configure
# time (`rdma_create_symlink`, absolute targets) and compiles with
# `-I/build/source/build/include`. Under the exact mirror the driver keeps
# that -I verbatim, and the scanner declared a header's spelled path only
# when it was relative, so the task carried `../ccan/str.h` and never
# `include/ccan/str.h`; cc1 died `ccan/str.h: No such file or directory`.
# The same command on configuration A has its -I relativised and carries
# both spellings, which is why no A reduction reached the class.
#
# Runs the one-edge ninja file the way nn-cc-shim.sh writes it, inside an
# ordinary recursive-nix derivation rooted at /build, with
# NIX_NINJA_NO_BUILD_DIR_SCAN=1. Emission is read from `-t drv`, then the
# object is built.
#   PASS   inputs carry include/ccan/str.h AND ../ccan/str.h, object built
#   FAIL   either spelling missing or the compile failed
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
#                        (a control at 41de370 or earlier shows the class)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-shimlink-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-shim-link-include-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf ];
  dontUnpack = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    S=\$NIX_BUILD_TOP/source; mkdir -p \$S/ccan \$S/build/include/ccan \$S/build/ccan/CMakeFiles/x.dir \$S/.nn-cc
    printf 'static inline int f(void){return 1;}\n' > \$S/ccan/str.h
    ln -s \$S/ccan/str.h \$S/build/include/ccan/str.h
    printf '#include <ccan/str.h>\nint g(void){return f();}\n' > \$S/ccan/str.c
    GCC=\$(command -v gcc)
    printf 'rule cc\n  command = %s -I%s/build/include -O2 -fPIC -o ccan/CMakeFiles/x.dir/str.c.o -c %s/ccan/str.c\n  deps = gcc\nbuild ccan/CMakeFiles/x.dir/str.c.o: cc %s/ccan/str.c\n' \"\$GCC\" \"\$S\" \"\$S\" \"\$S\" > \$S/.nn-cc/1.ninja
    cd \$S/build
    NIX_NINJA_NO_BUILD_DIR_SCAN=1 nix-ninja -C \$PWD -f \$S/.nn-cc/1.ninja -t drv ccan/CMakeFiles/x.dir/str.c.o > \$NIX_BUILD_TOP/drv.json
    if NIX_NINJA_NO_BUILD_DIR_SCAN=1 nix-ninja -C \$PWD -f \$S/.nn-cc/1.ninja ccan/CMakeFiles/x.dir/str.c.o; then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
  '';
  installPhase = ''mkdir -p \$out; cp \$NIX_BUILD_TOP/drv.json \$NIX_BUILD_TOP/verdict \$out/'';
}"
log=$(mktemp -t nn-shimlink-XXXXXX)
echo "building the shim shape on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
if [ $? -ne 0 ] || [ -z "$OUT" ]; then
  echo "FAIL: the derivation did not build, so NOTHING WAS MEASURED. Tail:"; tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"
spelled=$(grep -c ':include/ccan/str\.h:' "$OUT/drv.json"); spelled=${spelled:-0}
canon=$(grep -c ':\.\./ccan/str\.h:' "$OUT/drv.json"); canon=${canon:-0}
verdict=$(cat "$OUT/verdict")
if [ "$spelled" -gt 0 ] && [ "$canon" -gt 0 ] && [ "$verdict" = built ]; then
  echo "PASS: the linked header reaches the task at both spellings on configuration B and the object built"; exit 0
fi
echo "FAIL: spelled=$spelled canonical=$canon compile=$verdict (configuration B)"; exit 1
