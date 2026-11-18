#!/usr/bin/env bash
# REAL NINJA ASKED ABOUT A DRIVER-BUILT TREE, on configuration B (the
# consumer's route). The A gate `ninja-state-dry-run-repro.sh` asks the same
# questions outside any derivation, where paths are relativised; here the tree
# is /build/source/build inside ordinary recursive-nix, so a depfile spelling
# that only a sandbox produces would show as a dirty fresh tree.
#   PASS       fresh build clean, touched header dirty, rebuild clean
#   FAIL       any of those inverted (`-d explain` is in $out)
#   NO ANSWER  the derivation or a step inside it did not complete
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-ninjastate-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-ninja-state-dry-run-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf p.meson p.ninja ];
  dontUnpack = true; dontFixup = true; dontUseMesonConfigure = true; dontUseNinjaBuild = true;
  dontUseNinjaInstall = true; dontUseNinjaCheck = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    S=\$NIX_BUILD_TOP/source; mkdir -p \$S/inc
    printf \"project('nnstate', 'c')\nlib = static_library('greet', 'greet.c', include_directories: 'inc')\nexecutable('hello', 'main.c', include_directories: 'inc', link_with: lib)\n\" > \$S/meson.build
    printf '#define GREETING \"hi\"\nconst char *greet(void);\n' > \$S/inc/greet.h
    printf '#include \"greet.h\"\nconst char *greet(void) { return GREETING; }\n' > \$S/greet.c
    printf '#include <stdio.h>\n#include \"greet.h\"\nint main(void) { puts(greet()); return 0; }\n' > \$S/main.c
    R=\$NIX_BUILD_TOP/r; mkdir -p \$R
    NINJA=\$(command -v nix-ninja) meson setup \$S/build \$S > \$R/setup.log 2>&1 || { echo setup-failed > \$R/verdict; exit 0; }
    cd \$S/build
    drive() { NIX_NINJA_MATERIALIZE_ALL=1 nix-ninja all > \$R/\$1.log 2>&1; }
    ask() { ninja -n > \$R/\$1.n 2>&1; ninja -n -d explain > \$R/\$1.explain 2>&1 || true; }
    drive build1 || { echo build-failed > \$R/verdict; exit 0; }
    ask fresh
    sleep 1; touch \$S/inc/greet.h
    ask touched
    drive build2 || { echo rebuild-failed > \$R/verdict; exit 0; }
    ask rebuilt
    echo completed > \$R/verdict
  '';
  installPhase = ''mkdir -p \$out; cp -r \$NIX_BUILD_TOP/r/. \$out/'';
}"
log=$(mktemp -t nn-ninjastate-b-XXXXXX)
echo "building the meson shape on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
if [ $? -ne 0 ] || [ -z "$OUT" ]; then
  echo "NO ANSWER: the derivation did not build. Tail:"; tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"
v=$(cat "$OUT/verdict")
[ "$v" = completed ] || { echo "NO ANSWER: $v"; tail -8 "$OUT"/*.log 2>/dev/null | sed 's/^/  /'; exit 2; }
grep -h 'recorded .* edge' "$OUT/build1.log"
clean() { [ "$(cat "$OUT/$1.n")" = "ninja: no work to do." ]; }
fails=0
if clean fresh; then echo "fresh:   clean"; else echo "fresh:   DIRTY"; head -8 "$OUT/fresh.explain"; fails=$((fails+1)); fi
if clean touched; then echo "touched: CLEAN (unconditional)"; fails=$((fails+1)); else echo "touched: dirty"; fi
if clean rebuilt; then echo "rebuilt: clean"; else echo "rebuilt: DIRTY"; head -8 "$OUT/rebuilt.explain"; fails=$((fails+1)); fi
if [ "$fails" -eq 0 ]; then
  echo "PASS: on configuration B real ninja reads the driver-built tree clean, a touched header dirty, a rebuild clean"; exit 0
fi
echo "FAIL: $fails arm(s) inverted (configuration B)"; exit 1
