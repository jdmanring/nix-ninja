#!/usr/bin/env bash
# AN ALIAS A SECOND EDGE CONSUMES, which the openfec arm cannot see.
#
# Class 19 is "an edge's other outputs are not placed". openfec and brotli
# both PRODUCE a soname alias and consume it nowhere themselves, so their
# arms assert placement and the round trip and stop. CLAUDE.md already says
# what that leaves uncovered: "the failure needs a CONSUMER that takes the
# alias as an input", and brotli was the one member of the class that could
# not exhibit it.
#
# onetbb is that consumer, reported live at addff59 - a pin carrying 93346c2,
# 36d7158 and 6015510:
#
#     ld.bfd: warning: libtbbmalloc.so.2, needed by
#       lib_test_malloc_used_by_lib.so, not found (try using -rpath-link)
#     undefined reference to `scalable_malloc'
#
# THE SHAPE IS THREE EDGES AND IT HAS TO BE, because two do not fail. A link
# against the versioned file by path succeeds whether or not its soname alias
# is present. The alias is needed one level further on: the intermediate
# library records libmal.so.1 as DT_NEEDED, and the executable's link
# resolves that NEEDED through -rpath-link. Absent, ld warns and the symbol
# goes undefined - which is fatal for an executable and only a warning for a
# library, so the executable is what makes this arm assert.
#
# THE INTERMEDIATE CARRIES ITS OWN -rpath, and that is not decoration. The
# real onetbb command does (`-Wl,-rpath,/build/source/build/gnu_...`), and
# DT_RUNPATH is NOT INHERITED: the loader resolves libuse's own NEEDED from
# libuse's RUNPATH alone, never from the executable's. Without it the run
# check fails for a loader-semantics reason with nothing to do with the class
# - measured, on this arm's second run, which is the fixture shape this
# project keeps shipping.
#
#   A   libmal.so.1.4.2                  the link, and see the split below
#   A'  libmal.so.1                      the soname alias
#   B   libuse.so   links A by path      records NEEDED libmal.so.1
#   C   prog        links B              needs libmal.so.1 in ITS tree,
#                                        named nowhere on its own command
#
# TWO CONFIGURATIONS, AND THE DIFFERENCE BETWEEN THEM IS THE WHOLE FINDING.
# They share one graph and differ in ONE structural way: whether the soname
# alias is a second `out` of the link edge, or the output of a SEPARATE edge.
#
#   co-output   A declares both names          PASSES
#   split       A declares one, A' the alias   REPRODUCES onetbb verbatim
#
# onetbb is the split shape, read from the round's own command lines: the
# link carries one -o and a trailing `&& :` where cmake would have chained
# the symlink, and `cmake -E cmake_symlink_library` is its own edge. So
# `co_outputs` for the link edge is EMPTY, and every expansion route
# downstream - input-side, cmdline, transitive - has nothing to expand from.
# Neither mechanism is broken; the alias is a co-output of nothing either
# consumer declares. This is the UNDECLARED-OUTPUT family, not class 19.
#
# THE SPLIT SHAPE IS COMMON AND USUALLY HARMLESS, which is why this survived.
# lz4 has the identical separate-symlink edge and builds. Counted on the
# reporting round: 162 DISTINCT LIBRARIES carry the split shape and exactly
# one exhibits the failure, whose signature is the ld warning - 18
# occurrences, all libtbbmalloc.so.2, across two consumers inside its own
# package. (An earlier 507 was TASKS, counting retries and both editions.)
# The discriminator is not the split, it is whether a SAME-PACKAGE consumer
# then needs the alias through DT_NEEDED. lz4 has no such consumer;
# onetbb's test suite does. 162 is an UPPER BOUND on the population at
# risk: a package declaring the versioned file by path on every consuming
# edge carries the shape and never needs the alias.
#
# So this asks whether a soname alias reaches a consumer's sandbox
# TRANSITIVELY, which the green openfec arm does not exercise: openfec
# produces an alias and consumes it nowhere.
#
# Needs a builder-rpc-v0 daemon. Run from a checkout.
set -euo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

NN=$(b "$REPO#nix-ninja"); TASK=$(b "$REPO#nix-ninja-task")
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)
GATE_PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin"
# The list is the guard: this arm's whole verdict is a link succeeding, and a
# missing linker would fail it for a reason that has nothing to do with the
# class.
for t in nix-ninja gcc ld sh nix nix-ninja-task patchelf; do
  found=""; IFS=: read -ra dirs <<< "$GATE_PATH"
  for d in "${dirs[@]}"; do [ -x "$d/$t" ] && { found=$d; break; }; done
  [ -n "$found" ] || { echo "FAIL: $t is not on the gate PATH, so this arm would assert nothing"; exit 2; }
done

# ONE CASE, RUN TWICE. The two configurations share every file and differ in
# the split alone, so the comparison is the finding rather than either
# result: a reproduction that needed a second difference would not
# discriminate.
run_case() {
  SPLIT=$1
D=$(mktemp -d -t nn-aliascons-XXXXXX)
mkdir -p "$D/build" "$D/bin" "$D/src"
echo 'int mal_symbol(void) { return 217; }' > "$D/src/mal.c"
echo 'int mal_symbol(void); int use_symbol(void) { return mal_symbol() + 1; }' > "$D/src/use.c"
echo 'int use_symbol(void); int main(void) { return use_symbol() == 218 ? 0 : 1; }' > "$D/src/prog.c"

# The split is the ONE variable between the two configurations.
if [ "$SPLIT" = 1 ]; then
  MAL_RULES='rule linkmal
  command = gcc -shared -Wl,-soname,libmal.so.1 mal.o -o ../bin/libmal.so.1.4.2
rule symlinkmal
  command = ln -sf libmal.so.1.4.2 ../bin/libmal.so.1'
  MAL_EDGES='build ../bin/libmal.so.1.4.2: linkmal mal.o
build ../bin/libmal.so.1: symlinkmal ../bin/libmal.so.1.4.2'
else
  MAL_RULES='rule linkmal
  command = gcc -shared -Wl,-soname,libmal.so.1 mal.o -o ../bin/libmal.so.1.4.2 && ln -sf libmal.so.1.4.2 ../bin/libmal.so.1'
  MAL_EDGES='build ../bin/libmal.so.1.4.2 ../bin/libmal.so.1: linkmal mal.o'
fi

cat > "$D/build/build.ninja" <<EOF
rule cc
  command = gcc -fPIC -c \$in -o \$out
$MAL_RULES
rule linkuse
  command = gcc -shared -Wl,-soname,libuse.so use.o ../bin/libmal.so.1.4.2 -Wl,-rpath,../bin -o ../bin/libuse.so
rule linkprog
  command = gcc prog.o ../bin/libuse.so -Wl,-rpath,../bin -o ../bin/prog
build mal.o: cc ../src/mal.c
build use.o: cc ../src/use.c
build prog.o: cc ../src/prog.c
$MAL_EDGES
build ../bin/libuse.so: linkuse use.o ../bin/libmal.so.1.4.2
build ../bin/prog: linkprog prog.o ../bin/libuse.so
build all: phony ../bin/prog ../bin/libuse.so ../bin/libmal.so.1.4.2 ../bin/libmal.so.1
default all
EOF

cd "$D/build"
# set +e around the driver: with set -e a non-zero exit ends the script before
# the table prints, and a real reproduction then reads as a script crash.
set +e
env -i HOME="$D" PATH="$GATE_PATH" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" all
rc=$?
set -e
echo "driver rc=$rc"

for n in libmal.so.1.4.2 libmal.so.1 libuse.so prog; do
  if [ -e "$D/bin/$n" ] || [ -L "$D/bin/$n" ]; then printf '  %-18s present\n' "$n"
  else printf '  %-18s MISSING\n' "$n"; fi
done

if [ "$rc" != 0 ]; then
  echo
  echo "REPRODUCED: the executable's link failed. In configuration 2 that is"
  echo "  onetbb's shape and is the expected result; in configuration 1 it is"
  echo "  a REGRESSION, because a co-output alias must reach the consumer."
  echo "  build dir kept: $D"
  return 1
fi

# THE PHONY NAMES THE LIBRARIES ON PURPOSE. Local mode materializes the
# REQUESTED targets, so a graph whose default is `prog` alone leaves the
# libraries in the store and the run check then fails for a reason that has
# nothing to do with the class - measured, first run of this arm. The LINK is
# the subject and it happens inside the task either way; the run check is a
# second assertion and it needs a complete tree to be about anything.
#
# rc=0 IS NOT THE VERDICT. A driver that linked prog without the alias ever
# reaching C's tree would also exit 0, so ask the artifact: the program must
# RUN, which needs libmal.so.1 resolvable at load time by its soname.
if ! "$D/bin/prog"; then
  echo "FAIL: prog links and does not run, so the soname chain is broken at"
  echo "  load time even though the link succeeded. build dir kept: $D"
  return 1
fi
echo "PASS: a co-output alias reaches a transitive consumer - the executable"
echo "  links through the intermediate library's DT_NEEDED and runs."
rm -rf "$D"
  return 0
}

echo "=== configuration 1: the alias is a CO-OUTPUT of the link edge"
if run_case 0; then co_out=pass; else co_out=fail; fi
echo
echo "=== configuration 2: the alias is a SEPARATE edge (onetbb's shape)"
if run_case 1; then split=pass; else split=fail; fi

echo
printf 'co-output  %s\nsplit      %s\n' "$co_out" "$split"
if [ "$co_out" = pass ] && [ "$split" = fail ]; then
  echo "PASS: a co-output alias reaches a transitive consumer, and the same"
  echo "  graph with the alias on its OWN edge does not. The split is the only"
  echo "  difference, so it is the discriminator - onetbb's shape, and a GAP"
  echo "  pinned rather than a fix. This arm goes red when the split case is"
  echo "  handled, which is the only way that fix cannot land silently."
  exit 0
fi
if [ "$co_out" = pass ] && [ "$split" = pass ]; then
  echo "THE SPLIT CASE NOW BUILDS - which is NOT what this gate expects today."
  echo "  If a fix reaches an alias produced by a separate edge, that is it"
  echo "  landing: update this arm to require both, and update class 19."
  exit 1
fi
echo "FAIL: the co-output case must pass; read the kept build dir above."
exit 1
