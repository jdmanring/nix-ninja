#!/usr/bin/env bash
# A COMPILE WHOSE -I NAMES AN EMPTY DIRECTORY ABOVE THE BUILD DIRECTORY.
#
# CLOSED at `132b372`, which needed both crates: the driver carries a named
# directory above the build tree, and the task binary refused every `..`
# spelling categorically. Registered in run-all.sh.
#
# THE SIBLING GATE MODELS THE OTHER HALF and cannot reach this one.
# `empty-include-dir-compile-repro.sh` names a source-tree directory too, but
# its fixture puts a header in it, so the include scan stages it and the
# spelling is never exercised.
#
# IT DOES NOT MODEL GLIB EITHER, and a B build at that revision is what
# established it: glib's `../subprojects/gvdb` holds thirty entries, so the
# emptiness this fixture has is the one property the package lacks.
# `unstaged-include-dir-source-tree-repro.sh` is that class and is red.
#
# Configuration A, seconds. Walk ON, blanket off. Needs a builder-rpc-v0
# daemon.
#   PASS    the object builds, which means the class is CLOSED and this gate
#           moves into run-all.sh in that same commit
#   RED     the expected state while the class is open; the verdict prints
#           cc1's own line so a reader can tell it from an apparatus failure
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

D=$(mktemp -d -t nn-emptyinc-src-XXXXXX); S=$D/src
# The build-tree twin is present and EMPTY, the source-tree one is present and
# EMPTY. Both spellings are on the command, which is glib's own shape; the
# build-tree half is already carried, so a failure here is the source-tree
# half alone rather than the class at large.
mkdir -p "$S/gio" "$S/subprojects/gvdb" "$D/build/subprojects/gvdb" "$D/build/gio"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'int gio_init(void){return 0;}\n' > "$S/gio/ginit.c"
cat > "$D/build/build.ninja" <<NINJA
rule c_COMPILER
  command = gcc -Isubprojects/gvdb -I../src/subprojects/gvdb -Werror=missing-include-dirs -MD -MQ \$out -MF \$out.d -o \$out -c \$in
  deps = gcc
  depfile = \$out.d
build gio/ginit.c.o: c_COMPILER ../src/gio/ginit.c
NINJA
cd "$D/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja gio/ginit.c.o > "$D/build.log" 2>&1; brc=$?
set -e

if [ $brc -eq 0 ] && [ -f gio/ginit.c.o ]; then
  echo "PASS: a compile naming an empty SOURCE-tree include directory builds"
  exit 0
fi
# THE VERDICT IS THE MESSAGE AND NOT THE STATUS. Any apparatus failure also
# exits non-zero, so a red run that cannot show cc1 naming the directory is a
# NO ANSWER and must not be read as the class reproducing.
line=$(command grep -o "subprojects/gvdb: No such file or directory" "$D/build.log" | head -1)
if [ -n "$line" ]; then
  echo "RED BY DESIGN: the source-tree spelling is not carried (rc=$brc)"
  command grep -o '\.\./src/subprojects/gvdb: No such file or directory' "$D/build.log" | head -1
  exit 1
fi
echo "NO ANSWER: the compile failed without cc1 naming the directory (rc=$brc)"
command grep -E 'ABORTED|error' "$D/build.log" | head -3
echo "  load average now: $(cut -d' ' -f1-3 /proc/loadavg), $(nproc) threads"
exit 2
