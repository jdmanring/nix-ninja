#!/usr/bin/env bash
# A SECOND DRIVER PASS OVER A BUILD DIRECTORY THAT ALREADY HOLDS PLACED
# PRODUCTS. The consumer's shim runs the driver once for the build phase and
# again with NIX_NINJA_MATERIALIZE_ALL for install (and again for a check
# phase), all in one directory. A product the first pass placed and refreshed
# into a real file is, to the second pass, a file on disk, and several readers
# take a file on disk for an input: the build-directory walk, the command-line
# source fallback, the -Wl group scan. Each registers an opaque upload under
# the graph node an edge produces, the node keeps its first registration, and
# co-output expansion carries the upload to the alias names, which are then
# placed as links to a plain file and refreshed into copies (onetbb, brotli,
# configuration B, 2026-09-04).
#
# Configuration A, seconds per pass. Verdict after the second pass:
#   PASS  libfoo.so.1 -> libfoo.so.1.2.3 and libfoo.so -> libfoo.so.1 are
#         links with exactly those texts, libfoo.so.1.2.3 is a regular file,
#         app runs and exits 0
#   FAIL  otherwise, with the listing; NIX_NINJA_DIAG registrations are in
#         the kept log
# Usage: second-pass-placement-repro.sh [--control <rev>]   (driver from a
# worktree at <rev>; default this tree)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd); SRC=$REPO
if [ "${1:-}" = --control ]; then
  W=$(mktemp -d -t nn-spp-wt-XXXXXX); rmdir "$W"
  git -C "$REPO" worktree add -q --detach "$W" "${2:?rev}" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "driver from $2"
fi
b() { nix build --extra-experimental-features "$F" --cores 4 --max-jobs 1 --no-link --print-out-paths "$@" | tail -1; }
px() { b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1"; }
NN=$(b "$SRC#nix-ninja") || exit 2; TASK=$(b "$SRC#nix-ninja-task") || exit 2
CC=$(px gcc); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CM=$(px cmake)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default")
D=$(mktemp -d -t nn-spp-XXXXXX); mkdir -p "$D/build"; cd "$D/build" || exit 2
printf 'int foo(void){return 7;}\n' > foo.c
printf 'int foo(void); int main(void){return foo()==7?0:1;}\n' > main.c
cat > build.ninja <<'NINJA'
rule cc
  command = gcc -fPIC -MD -MF $out.d -c $in -o $out
  depfile = $out.d
  deps = gcc
rule link
  command = gcc -shared -Wl,-soname,libfoo.so.1 -o $out $in
rule alias
  command = cmake -E cmake_symlink_library libfoo.so.1.2.3 libfoo.so.1 libfoo.so
rule exe
  command = gcc -o $out main.o libfoo.so.1 -Wl,-rpath,'$$ORIGIN'
build foo.o: cc foo.c
build main.o: cc main.c
build libfoo.so.1.2.3: link foo.o
build libfoo.so.1 libfoo.so: alias libfoo.so.1.2.3
build app: exe main.o libfoo.so.1 | libfoo.so
build all: phony libfoo.so app
default all
NINJA
run() { env -i HOME="$D" PATH="$NN/bin:$CC/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$CM/bin" NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" NIX_NINJA_DIAG=1 "$@" "$NN/bin/nix-ninja" all; }
echo "--- pass 1 (all)"; run env >> "$D/log" 2>&1; echo "rc=$?"; ls -la libfoo* | cut -c1-70
echo "--- pass 2 (NIX_NINJA_MATERIALIZE_ALL=1 all)"; run env NIX_NINJA_MATERIALIZE_ALL=1 >> "$D/log" 2>&1; echo "rc=$?"; ls -la libfoo* app 2>&1 | cut -c1-90
echo "--- pass 3 (a check phase: NIX_NINJA_MATERIALIZE_ALL=1 all again)"; run env NIX_NINJA_MATERIALIZE_ALL=1 >> "$D/log" 2>&1; echo "rc=$?"; ls -la libfoo* app 2>&1 | cut -c1-90
printf '  app RUNPATH: %s\n' "$("$PE/bin/patchelf" --print-rpath app)"
ok=1
[ "$(readlink libfoo.so.1)" = libfoo.so.1.2.3 ] || ok=0
[ "$(readlink libfoo.so)" = libfoo.so.1 ] || ok=0
[ -f libfoo.so.1.2.3 ] && [ ! -L libfoo.so.1.2.3 ] || ok=0
./app; [ $? -eq 0 ] || ok=0
command grep -E 'DIAG opaque registration' "$D/log" | sed 's/^/  /' | head -6
# The walk's map must hold undeclared files only, so its printed length
# is the same on every pass; before 40fafcc it grew with each pass's
# placed products (brotli: 17 then 61) and its length gates the blanket.
counts=$(command grep -o 'build_dir_inputs=[0-9]*' "$D/log" | sort -u | wc -l)
printf '  build_dir_inputs across passes: %s\n' "$(command grep -o 'build_dir_inputs=[0-9]*' "$D/log" | tr '\n' ' ')"
[ "$counts" -eq 1 ] || { echo "  the walk's map changed size between passes"; ok=0; }
if [ $ok = 1 ]; then echo "PASS: aliases are links with their texts after the second pass, walk map constant"; rm -rf "$D"; exit 0; fi
echo "FAIL: see listing; kept $D (log: $D/log)"; exit 1
