#!/usr/bin/env bash
# A GENERATED TU SOURCE MUST ORDER BEHIND ITS PRODUCER AS A BUILT INPUT.
#
# qtwebengine 6.11.2 iter1 (leaf 2026-11-04): the GN jumbo_merge rule writes
# gen/ui/gfx/memory_buffer_sources_jumbo_{1,2}.cc via merge_for_jumbo.py.
# The compile edges name the gen .cc as their explicit $in, ordered through
# phony/ui/gfx/memory_buffer_sources.inputdeps. Leaf 2026-11-05 then showed
# 87 such compiles each spawning a doomed gcc -M fallback: the TU is
# generated and unwritten at resolve time, so cc1plus dies on the TU itself
# and the scan answer is kept anyway. The fallback is now skipped for a
# generated-absent TU (unit-pinned in generated_not_yet_written_tests; no
# config-A gate can reach it, since the scheduler runs the producer first
# and the TU exists on the local disk by resolve time). The TU still reaches
# the task as a Built input (scheduler orders producer first;
# derived_files hit at resolve), the static scan excludes Built by design,
# and the dynamic arm covers the TU at build time from realised inputs.
# This gate mirrors the GN shape on configuration A: a generator edge
# writes gen/jumbo_1.cc; a compile edge names it explicit with a phony
# order-only inputdeps; the compile must wait for the producer and build.
#
#   PASS    the object builds (producer ran, TU staged as Built)
#   FAIL    the compile resolves without the generated source or dies on it
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

NN=$(b "$REPO#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-jumbo-tu-XXXXXX)
mkdir -p "$D/build/gen"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
# The producer: a stand-in for merge_for_jumbo.py writing two gen sources.
cat > "$D/build/build.ninja" <<NINJA
rule gen_jumbo
  command = printf 'int jumbo_ok(void){return 1;}\n' > gen/jumbo_1.cc
rule cxx
  command = gcc -c gen/jumbo_1.cc -o \$out
  deps = gcc
  depfile = \$out.d
build gen/jumbo_1.cc: gen_jumbo
build phony/jumbo.inputdeps: phony gen/jumbo_1.cc
build jumbo_1.o: cxx gen/jumbo_1.cc || phony/jumbo.inputdeps
NINJA
cd "$D/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja jumbo_1.o > "$D/build.log" 2>&1; rc=$?
set -e
if [ $rc -eq 0 ] && [ -f jumbo_1.o ]; then
  echo "PASS: generated TU source ordered behind its producer and built"
  exit 0
fi
echo "FAIL: compile did not build its generated TU source (rc=$rc)"
command grep -E 'No such file|ABORTED|FAILED|error|fatal' "$D/build.log" | head -5
exit 1
