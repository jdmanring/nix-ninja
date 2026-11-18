#!/usr/bin/env bash
# A GENERATOR WRITES A SIDE PRODUCT INTO A DIRECTORY THAT EXISTS EMPTY IN THE
# BUILD TREE. rdma-core's buildlib/pandoc-prebuilt.py builds a man page and
# then `shutil.copy`s it into <build>/pandoc-prebuilt/<sha1>, a cache
# directory CMake created at configure time and nothing declares. The task
# sandbox holds the declared inputs and the walked FILES of the build
# directory; an empty directory uploads nothing, so the copy fails with
# ENOENT and the task dies (rdma-core 62.0, configuration B, 2026-09-04,
# reproduced here on A in seconds). Real ninja runs in the build tree, where
# the directory exists.
#
# Verdict: PASS when the generated file arrives AND the side copy landed;
# FAIL names which. Expected FAIL until the sandbox carries the build
# tree's empty directories.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd); SRC=$REPO
if [ "${1:-}" = --control ]; then
  W=$(mktemp -d -t nn-ebd-wt-XXXXXX); rmdir "$W"
  git -C "$REPO" worktree add -q --detach "$W" "${2:?rev}" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "driver from $2"
fi
b() { nix build --extra-experimental-features "$F" --cores 4 --max-jobs 1 --no-link --print-out-paths "$@" | tail -1; }
px() { b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1"; }
NN=$(b "$SRC#nix-ninja") || exit 2; TASK=$(b "$SRC#nix-ninja-task") || exit 2
CC=$(px gcc); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); PY=$(px python3)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default")
D=$(mktemp -d -t nn-ebd-XXXXXX); mkdir -p "$D/build/cache" "$D/src"; cd "$D/build" || exit 2
cat > "$D/src/gen.py" <<'PYS'
import sys, shutil, os
out = sys.argv[1]
open(out, "w").write("generated\n")
shutil.copy(out, os.path.join("cache", "copy-of-" + os.path.basename(out)))
PYS
cat > build.ninja <<'NINJA'
rule gen
  command = python3 ../src/gen.py $out
build page.1: gen ../src/gen.py
default page.1
NINJA
env -i HOME="$D" PATH="$NN/bin:$CC/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$PY/bin" NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" "$NN/bin/nix-ninja" page.1 > "$D/log" 2>&1; rc=$?
echo "driver rc=$rc"
[ -e page.1 ] && echo "page.1: present" || echo "page.1: ABSENT"
[ -e cache/copy-of-page.1 ] && echo "cache/copy-of-page.1: present" || echo "cache/copy-of-page.1: ABSENT"
if [ $rc = 0 ] && [ -e page.1 ]; then echo "PASS: the generator ran with its empty directory present"; rm -rf "$D"; exit 0; fi
command grep -E 'No such file|FileNotFound|ABORTED' "$D/log" | head -3 | cut -c1-160
echo "FAIL: the side directory was not in the sandbox; kept $D"; exit 1
