#!/usr/bin/env bash
# AN INPUT ADDED TO AN EDGE BETWEEN TWO DRIVER RUNS, WITH THE DISCOVERY CACHE
# PRESENT, MUST CHANGE THE TASK DERIVATION, AND A CACHE HIT MUST EMIT THE
# EXACTLY EQUAL DERIVATION TO THE FIRST RUN.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
SRC=${NN_GATE_SRC:-$REPO}
NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: nix-ninja did not build"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: nix-ninja-task did not build"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-dcadd-XXXXXX); S=$D/src; mkdir -p "$S/build" "$S/symbols" "$S/rules"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'a\n' > "$S/symbols/us"; printf 'b\n' > "$S/symbols/de"; printf 'c\n' > "$S/rules/base.xml"; printf 'd\n' > "$S/gen.py"
ninja() { printf 'rule gen\n  command = cat ../gen.py > $out\nbuild out.txt: gen ../gen.py%s\n' "$1" > "$S/build/build.ninja"; }
cd "$S/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
export NIX_NINJA_DISCOVERY_CACHE_DIR="$D/dc"
drv() {
  set +e; nix-ninja -t drv out.txt > "$D/drv$1.json" 2> "$D/drv$1.err"; rc=$?; set -e
  [ $rc -eq 0 ] || { echo "FAIL: run $1 -t drv rc=$rc"; tail -3 "$D/drv$1.err"; exit 1; }
  grep -oE '/nix/store/[a-z0-9]{32}-ninja-build\.drv' "$D/drv$1.json" | sort -u | head -1
}
ninja ""; h1=$(drv 1)
# Run two with cache active and same inputs (should hit cache)
h2=$(drv 2)
# Add input
ninja " | ../rules/base.xml ../symbols"; h3=$(drv 3)
rm -rf "$D/dc"
h4=$(drv 4)
if [ -n "$h1" ] && [ "$h1" = "$h2" ] && [ "$h3" != "$h1" ] && [ "$h3" = "$h4" ]; then
  echo "PASS: discovery cache hits produce equal derivations, and an added input re-keys"; exit 0
fi
echo "FAIL: h1=$h1 h2=$h2 h3=$h3 h4=$h4"; exit 1
