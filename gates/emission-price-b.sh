#!/usr/bin/env bash
# WHAT DOES THIS EDIT COST ON THE ROUTE THE CONSUMER BUILDS? The emission
# half of `emission-price.sh`, taken on CONFIGURATION B instead of A.
#
# THIS EXISTS BECAUSE EVERY PRICE SO FAR HAS CARRIED THE SAME CAVEAT. An
# effect gated on something only a sandbox supplies reads as free on A: the
# emitted PATH gains entries there that do not resolve outside a build, the
# build directory sits under `/build` so the exact mirror keeps include paths
# absolute, and `persistence_enabled` is false, so the resolve cache writes
# nothing. A free verdict on A is a claim about A, and the consumer does not
# build on A.
#
# `emission-determinism-sandbox.sh` answers a DIFFERENT question on this same
# configuration: whether one revision emits one derivation twice. It cannot
# compare two revisions, which is what pricing an edit means.
#
# THE FILESET TEST IS NOT REPEATED HERE. A task binary drvPath is a property
# of the flake and not of the configuration, so `emission-price.sh` already
# answers it and answering it twice would only invite the two to disagree.
#
# Both drivers are passed by absolute path rather than through PATH, since
# two `bin/nix-ninja` in one environment is a collision the shell resolves
# silently and in an order nobody chose. They are ALSO declared as inputs
# through `builtins.storePath`: a string that looks like a store path carries
# no dependency, so passing them only as environment values left neither of
# them inside the sandbox and the run died on a path that exists perfectly
# well outside it.
#
# NOTHING EXPLANATORY GOES INSIDE THE GENERATED EXPRESSION. It is built as a
# double-quoted shell string, so a comment written there is not a comment: a
# plain quotation mark ends the string and a backtick runs as a command. The
# note above was first written into the Nix source and turned into
# "builtins.storePath: command not found".
#
# Usage:  emission-price-b.sh [BASE_REV]     (default HEAD~1)
#   The other side is the working tree, so `git add` new files first.
set -uo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
[ -f "$REPO/crates/nix-ninja-task/src/main.rs" ] || { echo "NO ANSWER: $REPO is not the nix-ninja tree"; exit 2; }
INNER="$HERE/emission-price-b-inner.sh"
[ -r "$INNER" ] || { echo "NO ANSWER: inner script missing at $INNER"; exit 2; }
BASE_REV=${1:-HEAD~1}

WT=$(mktemp -d -t nn-priceb-wt-XXXXXX)
cleanup() {
  git -C "$WT" checkout -- . >/dev/null 2>&1
  git -C "$REPO" worktree remove --force "$WT" >/dev/null 2>&1
  git -C "$REPO" worktree prune >/dev/null 2>&1
}
trap cleanup EXIT
git -C "$REPO" worktree add -q --detach "$WT" "$BASE_REV" || { echo "NO ANSWER: cannot check out $BASE_REV"; exit 2; }

echo "base   $BASE_REV ($(git -C "$REPO" rev-parse --short "$BASE_REV"))"
echo "head   working tree at $REPO"

b(){ nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null | tail -1; }
NN_BASE=$(b "$WT#nix-ninja");  [ -x "$NN_BASE/bin/nix-ninja" ] || { echo "NO ANSWER: the base driver did not build"; exit 2; }
NN_HEAD=$(b "$REPO#nix-ninja"); [ -x "$NN_HEAD/bin/nix-ninja" ] || { echo "NO ANSWER: the head driver did not build"; exit 2; }
[ "$NN_BASE" = "$NN_HEAD" ] && { echo "NO ANSWER: both sides built the same driver, so there is no edit to price"; exit 2; }

EXPR="
let
  f = builtins.getFlake (toString $REPO);
  sys = builtins.currentSystem;
  p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  name = \"nn-emission-price-b\";
  dontUnpack = true;
  requiredSystemFeatures = [ \"recursive-nix\" ];
  NN_BASE = \"$NN_BASE/bin/nix-ninja\";
  NN_HEAD = \"$NN_HEAD/bin/nix-ninja\";
  nativeBuildInputs = [
    (builtins.storePath \"$NN_BASE\")
    (builtins.storePath \"$NN_HEAD\")
    f.packages.\${sys}.nix-ninja-task
    f.inputs.nix.packages.\${sys}.default
    p.patchelf p.gcc p.coreutils p.gnused p.bash
  ];
  buildPhase = builtins.readFile $INNER;
  installPhase = \"true\";
  dontFixup = true;
}"

log=$(mktemp -t nn-priceb-XXXXXX)
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths --expr "$EXPR" 2>"$log")
rc=$?
if [ $rc -ne 0 ] || [ -z "$OUT" ]; then
  echo "NO ANSWER: the derivation did not build, so nothing was measured. Its tail:"
  tail -25 "$log" | sed 's/^/  /'
  exit 2
fi
rm -f "$log"

cat "$OUT/verdict"
echo
grep -q '^SENSITIVITY OK' "$OUT/verdict" || { echo "NO ANSWER: the comparator could not see a changed command line"; exit 2; }
grep -q '^DETERMINISM OK' "$OUT/verdict" || { echo "NO ANSWER: one driver emitted two derivations for one shape"; exit 2; }
answered=$(awk '/^SHAPES ANSWERED/{print $3}' "$OUT/verdict")
[ "${answered:-0}" -ge 1 ] || { echo "NO ANSWER: no shape was compared"; exit 2; }
echo "configuration B. The fileset test is not asked here; emission-price.sh answers it."
# Restated with the verdict, for the reason recorded in emission-price.sh:
# a reader taking the tail keeps the conclusion and loses the proof that the
# comparator could discriminate. This gate already REFUSES a verdict when an
# arm is missing, which is the stronger design; this only makes the fact
# travel with the answer.
echo "arms passed before pricing: sensitivity determinism"
case "$(awk '/^VERDICT/{print $2}' "$OUT/verdict")" in
  FREE)  echo "VERDICT: FREE on every shape answered ($answered) on the consumer's route."; exit 0 ;;
  MOVED) echo "VERDICT: RE-KEYS THE SHAPES MARKED MOVED on the consumer's route."; exit 1 ;;
  *)     echo "VERDICT: NO ANSWER."; exit 2 ;;
esac
