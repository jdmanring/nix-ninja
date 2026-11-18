#!/usr/bin/env bash
# A CUSTOM TARGET THAT cd's TO THE SOURCE TREE AND RUNS A SCRIPT THERE BY
# ABSOLUTE PATH, on configuration B.
#
# shaderc's `check-copyright` target (WORKING_DIRECTORY is the source dir)
# becomes `cd /build/source && python3 /build/source/utils/add_copyright.py
# --check`; DEPENDS names nothing. The consumer's task derivation carried
# no input naming the script and python died "No such file". On A the
# same edge is rewritten to `cd .. && python3 ../utils/add_copyright.py`,
# which resolves the script one directory too high, and the script is not
# uploaded there either.
#
# Runs the edge inside an ordinary recursive-nix derivation rooted at
# /build, build dir /build/source/build, blanket off. Emission is read
# from `-t drv`, then the edge is built.
#   PASS   the task's inputs name add_copyright.py AND the edge built
#   FAIL   otherwise
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-cdparent-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-cd-parent-script-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf p.python3 ];
  dontUnpack = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    S=\$NIX_BUILD_TOP/source; mkdir -p \$S/utils \$S/build/CMakeFiles
    printf 'print(\"copyright ok\")\n' > \$S/utils/add_copyright.py
    PY=\$(command -v python3)
    printf 'rule CUSTOM_COMMAND\n  command = \$COMMAND\nbuild CMakeFiles/check-copyright: CUSTOM_COMMAND\n  COMMAND = cd %s && %s %s/utils/add_copyright.py --check\nbuild check-copyright: phony CMakeFiles/check-copyright\n' \"\$S\" \"\$PY\" \"\$S\" > \$S/build/build.ninja
    cd \$S/build
    nix-ninja -t drv check-copyright > \$NIX_BUILD_TOP/drv.json
    if nix-ninja check-copyright > \$NIX_BUILD_TOP/build.log 2>&1; then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
  '';
  installPhase = ''mkdir -p \$out; cp \$NIX_BUILD_TOP/drv.json \$NIX_BUILD_TOP/verdict \$NIX_BUILD_TOP/build.log \$out/'';
}"
log=$(mktemp -t nn-cdparent-XXXXXX)
echo "building the cd-to-source shape on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
if [ $? -ne 0 ] || [ -z "$OUT" ]; then
  echo "FAIL: the derivation did not build, so NOTHING WAS MEASURED. Tail:"; tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"
carried=$(grep -o 'NIX_NINJA_INPUTS[^}]*' "$OUT/drv.json" | grep -c 'add_copyright\.py'); carried=${carried:-0}
verdict=$(cat "$OUT/verdict")
cmd=$(grep -o 'cd [^"]*add_copyright[^"]*' "$OUT/drv.json" | head -1)
echo "emitted command: ${cmd:-<not found>}"
if [ "$carried" -gt 0 ] && [ "$verdict" = built ]; then
  echo "PASS: the script is carried and the cd-to-source edge built on configuration B"; exit 0
fi
echo "FAIL: carried=$carried build=$verdict (configuration B)"; grep -E 'No such file|ABORTED' "$OUT/build.log" | head -2; exit 1
