#!/usr/bin/env bash
# AN INPUT ADDED TO AN EDGE BETWEEN TWO DRIVER RUNS, WITH THE RESOLVE CACHE
# PRESENT, MUST CHANGE THE TASK DERIVATION.
#
# Observed once on A (xkeyboard-config, 2026-09-05): a build.ninja edit
# adding three implicit inputs to one edge emitted the same task hash as
# the run before it while .nix-ninja-resolve-cache.v1 was present, and a
# different hash once that file was deleted. The cache memoises DIRECTORY
# listings (kinds `dir` and `py`, keyed by directory, fingerprint count,
# total size, max mtime, directory mtime); it holds no edge, so it should
# have nothing to say about a changed input list. This gate says whether
# it does. Blanket off, so the added inputs have one route: the edge.
#
# Configuration A, seconds. Needs a builder-rpc-v0 daemon.
#   PASS    run two (cache present) and run three (cache deleted) both
#           emit a task derivation different from run one, and equal
#   FAIL    run two equals run one, or run two differs from run three
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
SRC=${NN_GATE_SRC:-$REPO}
NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-rcadd-XXXXXX); S=$D/src; mkdir -p "$S/build" "$S/symbols" "$S/rules"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'a\n' > "$S/symbols/us"; printf 'b\n' > "$S/symbols/de"; printf 'c\n' > "$S/rules/base.xml"; printf 'd\n' > "$S/gen.py"
ninja() { printf 'rule gen\n  command = cat ../gen.py > $out\nbuild out.txt: gen ../gen.py%s\n' "$1" > "$S/build/build.ninja"; }
cd "$S/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
drv() {
  set +e; nix-ninja -t drv out.txt > "$D/drv$1.json" 2> "$D/drv$1.err"; rc=$?; set -e
  [ $rc -eq 0 ] || { echo "FAIL: run $1 -t drv rc=$rc"; tail -3 "$D/drv$1.err"; exit 1; }
  grep -oE '/nix/store/[a-z0-9]{32}-ninja-build\.drv' "$D/drv$1.json" | sort -u | head -1
}
ninja ""; h1=$(drv 1)
[ -f .nix-ninja-resolve-cache.v1 ] || echo "note: no resolve cache file after run one"
# Three implicit inputs: a plain file and two directories (listed through the cache's `dir` kind).
ninja " | ../rules/base.xml ../symbols ../rules"; h2=$(drv 2)
rm -f .nix-ninja-resolve-cache.v1; h3=$(drv 3)
n2=$(grep -c ':\.\./symbols/us:\|:\.\./rules/base\.xml:' "$D/drv2.json"); n2=${n2:-0}
if [ -n "$h1" ] && [ "$h2" != "$h1" ] && [ "$h2" = "$h3" ] && [ "$n2" -gt 0 ]; then
  echo "PASS: an added input re-keys the task with the resolve cache present (run two carries it, equals run three)"; exit 0
fi
echo "FAIL: h1=$h1 h2=$h2 h3=$h3 carried=$n2"; exit 1
