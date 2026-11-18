#!/usr/bin/env bash
# A COMPILE WHOSE -I NAMES A DIRECTORY THAT EXISTS EMPTY IN THE BUILD TREE.
#
# DOES NOT COVER THE SOURCE-TREE SPELLING; `empty-include-dir-source-tree-
# repro.sh` does, and it exists because this gate could not. The header below
# already names `-I../subprojects/gvdb`, so both spellings were known, but
# this fixture puts a header in that directory, so the include scan stages it
# and the spelling is never exercised. A green verdict here is a claim about
# the build-tree half only, and the two gates are a pair.
#
# glib's gvdb is an in-tree meson subproject, so every gio object carries
# `-Isubprojects/gvdb` for the build tree beside `-I../subprojects/gvdb`
# for the source tree. The build-tree directory holds no file a task
# stages, and glib compiles with `-Werror=missing-include-dirs`, so cc1
# fails on every gio object with `subprojects/gvdb: No such file or
# directory`. The walk lists empty directories (`pending/empty-dirs`), but
# a compile task received none of them; it now receives the ones its
# command names as include directories (`include_dirs_named`).
#
# Configuration A, seconds. Walk ON (the listing is the walk's), blanket
# off. Needs a builder-rpc-v0 daemon.
#   PASS    the object builds
#   FAIL    otherwise
#   --control <rev>   expects the failure (any rev before the fix)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=${NN_GATE_SRC:-$REPO}
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-emptyinc-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-emptyinc-XXXXXX); S=$D/src; mkdir -p "$S/gio" "$S/subprojects/gvdb" "$D/build/subprojects/gvdb" "$D/build/gio"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf '#include "gvdb.h"\nint gio_init(void){return 0;}\n' > "$S/gio/ginit.c"
printf '/* source-tree twin, has a file */\n' > "$S/subprojects/gvdb/gvdb.h"
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
if [ -n "$CONTROL_REV" ]; then
  if [ $brc -ne 0 ]; then echo "CONTROL PASS: $CONTROL_REV fails the compile (rc=$brc)"; grep -o 'subprojects/gvdb: No such file[^"]*' "$D/build.log" | head -1; exit 0; fi
  echo "CONTROL FAILED: expected the compile to fail at $CONTROL_REV"; exit 1
fi
if [ $brc -eq 0 ] && [ -f gio/ginit.c.o ]; then
  echo "PASS: a compile naming an empty build-tree include directory builds"; exit 0
fi
echo "FAIL: rc=$brc"; grep -E 'ABORTED|No such file' "$D/build.log" | head -3; exit 1
