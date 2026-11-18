#!/usr/bin/env bash
# WHAT DOES A FORWARDED ENVIRONMENT VARIABLE COST?
#
# emission-price.sh CANNOT ANSWER THIS AND ITS FREE VERDICT IS NOT ABOUT IT.
# That gate runs every driver under `env -i`, which is right for its own
# question and blind to this one: a variable the allowlist newly forwards is
# never set, so the emission cannot move and the price reads free on all five
# shapes. That is a true statement about the OTHER shapes and says nothing
# about the change. This gate is the other half, and the two are read
# together.
#
# It reports rather than asserts, because the answer is a pair of numbers and
# not a verdict: what the variable adds to the emitted bytes, and how many
# STORE PATHS it adds to the task's inputs. The second is the one with a
# standing cost, since every store path in a task's closure is a daemon lock
# acquisition per realise.
#
#   NN_VAR=<name>    the variable to price (default XDG_DATA_DIRS)
#   NN_VALUE_A=...   a value a plain stdenv build holds
#   NN_VALUE_B=...   a value a build using the feature holds
set -u
REPO=$(cd "$(dirname "$0")/../.." && pwd)
VAR=${NN_VAR:-XDG_DATA_DIRS}
F='nix-command flakes'
nb() { nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null; }
px() { nb --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
DRV=$(nb "$REPO#packages.x86_64-linux.nix-ninja" | tail -1)
TASK=$(nb "$REPO#packages.x86_64-linux.nix-ninja-task" | tail -1)
CC=$(px gcc); CU=$(px coreutils); SH=$(px bash); PE=$(px patchelf)
NIXP=""
for p in $(nb --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default"); do
  [ -x "$p/bin/nix" ] && { NIXP=$p; break; }
done
for v in DRV TASK CC CU SH PE NIXP; do
  eval "[ -n \"\$$v\" ] && [ -d \"\$$v\" ]" || { echo "NO ANSWER: $v did not resolve"; exit 2; }
done
# A HOST PATH IS NOT USABLE HERE: the driver refuses a bash outside the store,
# and the refusal prints on stderr while stdout stays empty, which scores as
# an identical emission against anything.
GATE_PATH="$CC/bin:$CU/bin:$SH/bin:$NIXP/bin:$TASK/bin:$PE/bin"
D=$(mktemp -d -t nn-fenv-XXXXXX)
trap 'rm -rf "$D"' EXIT
mkdir -p "$D/src" "$D/build"
printf 'int main(void){return 0;}\n' > "$D/src/main.c"
printf 'rule cc\n  command = gcc -c $in -o $out\nbuild plain.o: cc ../src/main.c\n' > "$D/build/build.ninja"
emit() {
  ( cd "$D/build" && env -i HOME="$D" PATH="$DRV/bin:$GATE_PATH" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F dynamic-derivations ca-derivations recursive-nix" \
      NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 "$@" \
      "$DRV/bin/nix-ninja" -t drv plain.o 2>>"$D/err" )
}
paths() { printf '%s' "$1" | command grep -o '/nix/store/[a-z0-9]\{32\}-[^"/ ]*' | command grep -v 'ninja-build\.drv' | sort -u; }
A=$(emit)
[ -n "$A" ] || { echo "NO ANSWER: the baseline emission is empty"; tail -3 "$D/err"; exit 2; }
VA=${NN_VALUE_A:-$PE/share}
VB=${NN_VALUE_B:-}
report() { # report <label> <value>
  local out; out=$(emit "$VAR=$2")
  [ -n "$out" ] || { echo "  $1: NO ANSWER, empty emission"; return; }
  printf '  %-14s bytes %s -> %s (%+d)   inputs %s -> %s (%+d)\n' "$1" \
    "${#A}" "${#out}" "$(( ${#out} - ${#A} ))" \
    "$(paths "$A" | wc -l)" "$(paths "$out" | wc -l)" \
    "$(( $(paths "$out" | wc -l) - $(paths "$A" | wc -l) ))"
  comm -13 <(paths "$A") <(paths "$out") | sed 's/^/      adds /'
}
echo "variable: $VAR   (absent: ${#A} bytes, $(paths "$A" | wc -l) input store paths)"
report "plain build" "$VA"
[ -n "$VB" ] && report "feature build" "$VB"
echo "the bytes move wherever the variable is SET, which is the re-key; the"
echo "inputs are the standing cost, one daemon lock per path per realise."
