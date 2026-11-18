#!/usr/bin/env bash
# DOES ONE UNCHANGED TARGET EMIT ONE DERIVATION?
#
# Per-TU banking is what this tool is priced on, and it rests entirely on a
# task's key being a function of the task. It is not: the driver writes
# `.nix-ninja-nar-stamps.v1` INTO the build directory (`NAR_FILE` in resolve_cache.rs)
# and the implicit-input blanket sweeps that directory into every task's
# inputs, so the key moves whenever the file does - which is every run, and
# also WITHIN a run as the file is rewritten.
#
# THIS ARM PINNED THE GAP until 037cf7f, when the walk started skipping
# the driver's own state files (`build_dir_disposition`); it now requires
# two local drives to emit identical task derivations, and goes red if a
# driver-written file re-enters the blanket.
#
# WHAT IT DELIBERATELY DOES NOT CARE ABOUT: which file moved. Three
# mechanisms are known - the stamps file here, the blanket over a build
# directory that GROWS as the build proceeds (the consumer's, in-sandbox,
# 61 packages), and glslang's 145 executions inside one invocation, which
# neither of those explains. Comparing whole derivations catches all three.
#
# CONFIGURATION A ONLY, AND THAT LIMIT IS THE POINT. `persistence_enabled`
# is `!in_nix_build` (resolve_cache.rs:160), so the stamps file is never
# written inside a derivation and this mechanism is gated OFF on the route
# the consumer builds. A result here is a claim about configuration A. The
# in-sandbox arm is MISSING and is what would have caught the split that had
# one session attributing the other's numbers to this cause.
set -uo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
b(){ nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null; }
NN=$(b "$REPO#nix-ninja") || { echo "FAIL: no driver"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task") || { echo "FAIL: no task binary"; exit 2; }
px(){ b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
CC=$(px gcc); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
# A MULTI-OUTPUT PACKAGE PRINTS SEVERAL PATHS AND `head -1` CAN TAKE THE
# MAN OUTPUT, which is how this gate first reported an empty emission and
# would have compared two empty strings as equal. Pick the output that
# actually carries the binary, then PROVE each tool resolves before any
# assertion is made.
NIXP=""
for p in $(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default"); do
  [ -x "$p/bin/nix" ] && { NIXP=$p; break; }
done
GATE_PATH="$NN/bin:$CC/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin"
# THE LIST IS THE GUARD. It first held five names, patchelf was the
# sixth thing the driver needs, and the emission failed while every
# checked tool resolved. A pre-flight check covers what it names.
for t in nix-ninja gcc sh nix nix-ninja-task patchelf; do
  found=""
  IFS=: read -ra dirs <<< "$GATE_PATH"
  for d in "${dirs[@]}"; do [ -x "$d/$t" ] && { found=$d; break; }; done
  [ -n "$found" ] || { echo "FAIL: $t is not on the gate PATH, so this arm would assert nothing"; exit 2; }
done

D=$(mktemp -d -t nn-emit-XXXXXX); trap 'rm -rf "$D"' EXIT
mkdir -p "$D/build" "$D/src"
printf 'int main(void){return 0;}\n' > "$D/src/main.c"
cat > "$D/build/build.ninja" <<'EOF'
rule cc
  command = gcc -c $in -o $out
build main.o: cc ../src/main.c
default main.o
EOF
cd "$D/build"
emit(){ env -i HOME="$D" PATH="$GATE_PATH" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" -t drv main.o 2>/dev/null; }
A=$(emit); B=$(emit)
# An empty emission compares EQUAL to another empty emission and would read
# as stability, so the reason is printed rather than swallowed.
if [ -z "$A" ]; then
  echo "the driver emitted nothing; its stderr was:"
  env -i HOME="$D" PATH="$GATE_PATH" NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $F" "$NN/bin/nix-ninja" -t drv main.o 2>&1 \
    >/dev/null | tail -3 | sed 's/^/  /'
fi
ha=$(printf '%s' "$A" | sha256sum | cut -c1-16)
hb=$(printf '%s' "$B" | sha256sum | cut -c1-16)
echo "emission 1  $ha"
echo "emission 2  $hb"
[ -n "$A" ] || { echo "FAIL: the driver emitted nothing, so this arm asserts nothing"; exit 1; }

if [ "$ha" != "$hb" ]; then
  echo
  echo "the differing inputs:"
  diff <(printf '%s' "$A" | tr ',' '\n') <(printf '%s' "$B" | tr ',' '\n') |
    grep -oE '[a-z0-9]{32}-[^":]*' | sort -u | head -6 | sed 's/^/  /'
  echo
  echo "FAIL: emission is UNSTABLE across two local drives (class 27 regressed:"
  echo "the walk must skip the driver's own state files, build_dir_disposition)."
  exit 1
fi
echo "PASS: emission is STABLE across two local drives (class 27, configuration A)."
