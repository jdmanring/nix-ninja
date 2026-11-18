#!/usr/bin/env bash
# A GENERATED HEADER INCLUDES A GENERATED SIBLING THAT NO EDGE DECLARES.
#
# RED BY DESIGN while the class is open, in the way `dt-needed-alias-repro.sh`
# is, and excluded from both runners for the same reason: a corpus arm answers
# do-not-spend-the-machine, which a permanently red arm cannot.
#
# glib 2.88.3 on configuration B, the leaf under `glib-tests-protocol`:
#
#   gio/tests/gdbus-object-manager-example/objectmanager-gen.h:12:10:
#   fatal error: gdbus-example-objectmanager-visibility.h: No such file
#
# READ FROM THE PACKAGE'S MESON, not from the message. Both files are
# `custom_target` outputs in ONE directory; `objectmanager-gen` emits that
# include because of `--symbol-decorator-header`, and its target declares no
# dependency on the visibility target. So the compile declares the includer
# and nothing declares the sibling. Real ninja survives it because both
# targets are in `all` and the tree is shared; a narrowed sandbox does not.
#
# THE MECHANISM IS THE VIRTUAL MAP'S SEED, read in `new_task`: `virtual_paths`
# is built from `task.inputs`, the edge's ALREADY DECLARED inputs, so it can
# only rescue a generated file the compile was going to receive anyway. A file
# reachable only through the CONTENTS of another generated file is outside it
# by construction, and the sandbox cannot recover it either, since discovery
# there has the filesystem and not the graph.
#
# THE INCLUDER IS AT ITS BUILD PATH in cpp's own trace, so this is NOT the
# placed-header class (25); a fixture that reached it through a store path
# would be modelling the wrong thing.
#
# THE SECOND ARM IS THE POSITIVE CONTROL. The same compile also includes a
# generated header the edge DOES declare, which must build at every revision.
# Without it a driver that resolved nothing at all would satisfy the subject's
# failure and read as the class.
#
# Configuration A, seconds. Blanket OFF, because the implicit-input blanket is
# the widest route that could supply the file and would make this pass for the
# wrong reason.
#   PASS    the subject compile builds
#   RED     the expected state while the class is open; the verdict prints
#           cpp's own line so a reader can tell it from an apparatus failure
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

NN=$(b "$REPO#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-genhdr-sib-XXXXXX); S=$D/src
mkdir -p "$S" "$D/build/ex"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf '#include "ex/gen.h"\n#include "ex/declared.h"\nint use(void){return GEN_OK + DECLARED_OK;}\n' > "$S/a.c"
# Two generators writing into ONE directory, as the package has. `gen.h`
# names `sib.h` by quoted sibling; nothing declares `sib.h` to the compile.
cat > "$D/build/build.ninja" <<NINJA
rule CUSTOM_COMMAND
  command = \$COMMAND
rule c_COMPILER
  command = gcc -I. -MD -MQ \$out -MF \$out.d -o \$out -c \$in
  deps = gcc
  depfile = \$out.d
build ex/sib.h: CUSTOM_COMMAND
  COMMAND = printf '#define SIB_OK 1\\n' > ex/sib.h
build ex/gen.h: CUSTOM_COMMAND
  COMMAND = printf '#include "sib.h"\\n#define GEN_OK SIB_OK\\n' > ex/gen.h
build ex/declared.h: CUSTOM_COMMAND
  COMMAND = printf '#define DECLARED_OK 1\\n' > ex/declared.h
build a.o: c_COMPILER ../src/a.c | ex/gen.h ex/declared.h
NINJA
cd "$D/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja a.o > "$D/build.log" 2>&1; rc=$?
set -e

# THE COMPILER'S WORDS ARE IN THE LEAF'S LOG, NOT IN THE DRIVER'S ABORT, and
# a first version of this gate scored on neither: it tested for a placed file
# on disk after an aborted run, which is absent for every reason including the
# right one. The abort names the resolved derivation; its log is the evidence.
leaf=$(command grep -oE "build of resolved derivation '[^']*'" "$D/build.log" \
       | command grep -oE '/nix/store/[a-z0-9]{32}-[^\x27]*\.drv' | head -1)
if [ -n "$leaf" ]; then
  nix log --extra-experimental-features "$F" "$leaf" > "$D/leaf.log" 2>&1 || true
else
  : > "$D/leaf.log"
fi
cat "$D/build.log" "$D/leaf.log" > "$D/all.log"

if [ $rc -eq 0 ] && [ -f a.o ]; then
  echo "PASS: a generated header's generated sibling reaches the compile"
  exit 0
fi
# THE POSITIVE CONTROL, from the same compile: cpp must name the DECLARED
# generated header as the includer. That says declared generated files do
# reach a task, so the subject is about the undeclared one and not about
# generated files at large. Without it a driver that staged nothing would
# produce the same missing-file line and read as this class.
if ! command grep -q 'ex/gen\.h' "$D/all.log"; then
  echo "NO ANSWER: cpp never named the DECLARED generated header, so nothing"
  echo "  here is a statement about the undeclared sibling"
  tail -5 "$D/all.log" | sed 's/^/  /'
  exit 2
fi
echo "control OK: the DECLARED generated header reached the compile"
if command grep -q 'sib\.h: No such file or directory' "$D/all.log"; then
  echo "RED BY DESIGN: the undeclared generated sibling is not carried (rc=$rc)"
  command grep -o 'sib\.h: No such file or directory' "$D/all.log" | head -1
  exit 1
fi
echo "NO ANSWER: the compile failed without cpp naming the sibling (rc=$rc)"
command grep -E 'ABORTED|error|fatal' "$D/all.log" | head -3
echo "  load average now: $(cut -d' ' -f1-3 /proc/loadavg), $(nproc) threads"
exit 2
