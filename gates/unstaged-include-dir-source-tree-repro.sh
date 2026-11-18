#!/usr/bin/env bash
# A COMPILE WHOSE -I NAMES A POPULATED SOURCE DIRECTORY IT OPENS NOTHING IN.
#
# CLOSED at `498dac3`, and registered in run-all.sh in that same commit. The
# carrier stopped keying on emptiness and became `NIX_NINJA_MAKE_DIRS`, named
# for what the task does with the list.
#
# THIS IS GLIB, AND ITS SIBLING GATES ARE NOT. Both
# `empty-include-dir-compile-repro.sh` and
# `empty-include-dir-source-tree-repro.sh` name a directory that is EMPTY, and
# a build on configuration B at `132b372` is what established that glib has no
# such directory: `../subprojects/gvdb` in the 2.88.3 tarball holds thirty
# entries, four of gio's own sources include `gvdb/gvdb-reader.h` through that
# `-I`, and the compile that dies opens nothing there. So the emptiness test
# excludes it and no widening of the empty-directory carrier reaches it.
#
# WHY IT COULD NOT PASS BEFORE, read from the code. `include_dirs_named` put
# the spelling in its list, and the consumer intersected that list with
# `self.empty_dirs`, the walk's empty directories. A populated one was not in
# that set at all, so nothing carried it, nothing staged a file under it, and
# the directory was absent from the task sandbox while
# `-Werror=missing-include-dirs` made the absence fatal.
#
# THE FIXTURE'S SECOND ARM IS WHAT MAKES THE FIRST SAFE TO FIX. A stand-in
# directory could in principle turn a missing header into a silent search
# miss, so the same run compiles a TU that DOES include a header from that
# directory and requires it to build: where a header is used the scan declares
# it and staging creates the directory, which is why an empty stand-in can
# only ever satisfy `missing-include-dirs`.
#
# Configuration A, seconds. Walk ON, blanket off. Needs a builder-rpc-v0
# daemon.
#   PASS    both arms build
#   RED     the class has regressed; the verdict prints cc1's own line so a
#           reader can tell it from an apparatus failure
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

D=$(mktemp -d -t nn-unstagedinc-XXXXXX); S=$D/src
# The source-tree directory is POPULATED, as glib's is, and the build-tree
# twin is present and empty, which is glib's shape exactly. Both spellings are
# on the command; the build-tree half is already carried, so a failure here is
# the populated source-tree half alone.
mkdir -p "$S/gio" "$S/subprojects/gvdb/gvdb" "$D/build/subprojects/gvdb" "$D/build/gio"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'int gio_init(void){return 0;}\n' > "$S/gio/ginit.c"
# ARM TWO opens a header from that same directory, so the scan declares it and
# staging creates the directory. It must build at every revision, and it is
# what says a stand-in cannot hide a missing header.
printf '#define GVDB_OK 1\n' > "$S/subprojects/gvdb/gvdb/gvdb-reader.h"
printf 'int gvdb_ok(void){return GVDB_OK;}\n' > "$S/gio/guser.c"
printf '#include "gvdb/gvdb-reader.h"\n' | cat - "$S/gio/guser.c" > "$S/gio/guser.c.t" && mv "$S/gio/guser.c.t" "$S/gio/guser.c"
cat > "$D/build/build.ninja" <<NINJA
rule c_COMPILER
  command = gcc -Isubprojects/gvdb -I../src/subprojects/gvdb -Werror=missing-include-dirs -MD -MQ \$out -MF \$out.d -o \$out -c \$in
  deps = gcc
  depfile = \$out.d
build gio/ginit.c.o: c_COMPILER ../src/gio/ginit.c
build gio/guser.c.o: c_COMPILER ../src/gio/guser.c
NINJA
cd "$D/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja gio/guser.c.o > "$D/control.log" 2>&1; crc=$?
nix-ninja gio/ginit.c.o > "$D/build.log" 2>&1; brc=$?
set -e

# THE CONTROL IS FIRST BECAUSE IT DECIDES WHETHER THE SUBJECT SAYS ANYTHING.
if [ $crc -ne 0 ] || [ ! -f gio/guser.c.o ]; then
  echo "NO ANSWER: the control arm did not build, so the subject means nothing (rc=$crc)"
  command grep -E 'ABORTED|error' "$D/control.log" | head -3
  echo "  load average now: $(cut -d' ' -f1-3 /proc/loadavg), $(nproc) threads"
  exit 2
fi
echo "control OK: a TU that INCLUDES a header from that directory builds"

if [ $brc -eq 0 ] && [ -f gio/ginit.c.o ]; then
  echo "PASS: a compile naming a populated SOURCE-tree include directory builds"
  exit 0
fi
# THE VERDICT IS THE MESSAGE AND NOT THE STATUS. Any apparatus failure also
# exits non-zero, so a red run that cannot show cc1 naming the directory is a
# NO ANSWER and must not be read as the class reproducing.
line=$(command grep -o "subprojects/gvdb: No such file or directory" "$D/build.log" | head -1)
if [ -n "$line" ]; then
  echo "RED BY DESIGN: a populated source-tree include directory is not carried (rc=$brc)"
  command grep -o '\.\./src/subprojects/gvdb: No such file or directory' "$D/build.log" | head -1
  exit 1
fi
echo "NO ANSWER: the compile failed without cc1 naming the directory (rc=$brc)"
command grep -E 'ABORTED|error' "$D/build.log" | head -3
echo "  load average now: $(cut -d' ' -f1-3 /proc/loadavg), $(nproc) threads"
exit 2
