#!/usr/bin/env bash
# A TASK OUTPUT AND AN UPLOAD OF THE SAME NAME BOTH CLAIM ONE BUILD PATH.
#
# dav1d 1.5.3, witnessed by the consumer at 9c2db59 on the ninja route
# (nix log /nix/store/f570f0nbyg1fy3kih7gxq8cl2pl1zcr5-ninja-build.drv):
# `include/vcs_version.h` is claimed by the edge that GENERATES it and by an
# UPLOAD of a file of that name, and the task binary refuses the second in
# sandbox setup with "Two different files claim one build path".
#
# THE SHAPE NEEDS TWO PASSES IN ONE DIRECTORY, which is what a build,
# install and check phase does. Pass one runs the generator and the driver
# places its product on disk as a real file. On pass two the compile's own
# dependency scan finds that file where the graph also produces it, uploads
# it as an opaque source, and the edge supplies the same build path as a
# task output. One pass cannot reach it: nothing is on disk to upload.
#
# NOT TWO SOURCES DISAGREEING, and the fix depends on that. Both claimants
# in dav1d hold identical bytes, the supposed source being itself
# `/* auto-generated, do not edit */`; this is the class where a later pass
# adopts a placed product as an input. Preferring the edge's output is
# therefore safe, and it is the only order available, since the output does
# not exist yet and cannot be re-read the way a stale upload can.
#
# BLANKET OFF. The implicit-input blanket would sweep the build directory
# and supply the header for a different reason, which is the widest of the
# routes a discovery gate has to switch off.
#
# Configuration A, seconds.
#   PASS  the second pass emits a task carrying ONE claimant for the header
#         and the object builds
#   FAIL  two claimants, or the sandbox refusal
set -uo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
SRC=$REPO
b() { nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null | tail -1; }

# A CONTROL BELONGS TO THE TREE IT RAN IN: --control <rev> drives the same
# fixture with a driver built from that revision, so the class can be shown
# rather than asserted.
if [ "${1:-}" = "--control" ]; then
  W=$(mktemp -d -t nn-bsu-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "${2:?revision}" || { echo "NO ANSWER: cannot check out $2"; exit 2; }
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1; git -C "$REPO" worktree prune >/dev/null 2>&1' EXIT
  SRC=$W
fi

NN=$(b "$SRC#nix-ninja");   [ -x "$NN/bin/nix-ninja" ] || { echo "NO ANSWER: the driver did not build"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task"); [ -x "$TASK/bin/nix-ninja-task" ] || { echo "NO ANSWER: the task binary did not build"; exit 2; }
px() { b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1"; }
CC=$(px gcc); CU=$(px coreutils); SH=$(px bash); PE=$(px patchelf)
NIXP=""
for p in $(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default"); do
  [ -x "$p/bin/nix" ] && { NIXP=$p; break; }
done
GATE_PATH="$NN/bin:$CC/bin:$CU/bin:$SH/bin:$NIXP/bin:$TASK/bin:$PE/bin"
for t in gcc sh nix nix-ninja nix-ninja-task; do
  found=""; IFS=: read -ra dirs <<< "$GATE_PATH"
  for d in "${dirs[@]}"; do [ -x "$d/$t" ] && { found=1; break; }; done
  [ -n "$found" ] || { echo "NO ANSWER: $t is not on the gate PATH"; exit 2; }
done

D=$(mktemp -d -t nn-bsu-XXXXXX)
mkdir -p "$D/build/include" "$D/src"
# The generated header carries the same shape dav1d's does, a version macro
# under an auto-generated banner, so the fixture cannot quietly become a
# test about two DIFFERENT contents.
printf '#include "vcs_version.h"\nint main(void){return V-1;}\n' > "$D/src/main.c"
cat > "$D/build/build.ninja" <<'NINJA'
rule gen
  command = sh -c 'printf "/* auto-generated, do not edit */\n#define V 1\n" > $out'
rule cc
  command = gcc -MD -MF $out.d -Iinclude -c $in -o $out
  depfile = $out.d
  deps = gcc
build include/vcs_version.h: gen
build main.o: cc ../src/main.c | include/vcs_version.h
default main.o
NINJA

# MATERIALIZE_ALL BELONGS TO PASS ONE AND NOWHERE ELSE. Only the requested
# target's outputs are placed, so without it the generated header never
# reaches the disk and pass two has nothing to adopt; with it on the `-t drv`
# run the driver is asked for two targets and aborts with "name one target",
# which prints no derivation and would read as a claimant count of zero.
run() {
  ( cd "$D/build" && env -i HOME="$D" PATH="$GATE_PATH" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F" \
      NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
      "$@" )
}

run env NIX_NINJA_MATERIALIZE_ALL=1 "$NN/bin/nix-ninja" main.o >"$D/pass1.log" 2>&1; rc1=$?
[ -f "$D/build/include/vcs_version.h" ] || {
  echo "NO ANSWER: pass one did not place the generated header, so pass two cannot reach the class"
  tail -8 "$D/pass1.log" | sed 's/^/  /'; exit 2; }
rm -f "$D/build/main.o" "$D/build/main.o.d"
run env "$NN/bin/nix-ninja" -t drv main.o >"$D/pass2.drv" 2>"$D/pass2.err"; rc2=$?
run env "$NN/bin/nix-ninja" main.o        >"$D/pass2.log" 2>&1; rc3=$?

# COUNT THE CLAIMANTS, never just the build. A build that happens to
# succeed says nothing about whether two claimants were emitted; the
# refusal is what the consumer saw and the count is what causes it.
claims=$(command grep -o '[^ "]*:include/vcs_version.h:[^ "]*' "$D/pass2.drv" 2>/dev/null | sort -u)
nclaims=$(printf '%s' "$claims" | command grep -c . || true)
refused=$(command grep -c 'claim one build path' "$D/pass1.log" "$D/pass2.log" 2>/dev/null | awk -F: '{s+=$2} END {print s+0}')

echo "pass1 rc=$rc1  emit rc=$rc2  pass2 rc=$rc3  claimants=$nclaims  refusals=$refused"
printf '%s\n' "$claims" | sed 's/^/  /' 
if [ "$rc2" -ne 0 ] || [ ! -s "$D/pass2.drv" ]; then
  echo "NO ANSWER: the second pass emitted nothing, so nothing was counted"
  tail -5 "$D/pass2.err" | sed 's/^/  /'; exit 2
fi
if [ "$refused" -gt 0 ]; then
  echo "FAIL: the task refused the second claimant (the class)"
  command grep -m1 'claim one build path' "$D/pass1.log" "$D/pass2.log" | sed 's/^/  /'
  echo "kept $D"; exit 1
fi
if [ "$nclaims" -gt 1 ]; then
  echo "FAIL: the header reached the task at $nclaims claimants; the refusal is one sandbox away"
  echo "kept $D"; exit 1
fi
if [ "$nclaims" -ne 1 ] || [ "$rc3" -ne 0 ]; then
  echo "FAIL: claimants=$nclaims and the object did not build"
  tail -8 "$D/pass2.log" | sed 's/^/  /'; echo "kept $D"; exit 1
fi
# WHICH CLAIMANT SURVIVED, not merely how many. A count of one is also what
# a fix that kept the UPLOAD and dropped the edge's output would print, and
# that fix compiles whatever bytes happen to be on disk. The encoding
# discriminates without parsing: a Built input carries its rel_path in the
# third colon-separated field and an Opaque one leaves it empty, which is
# exactly how the two dav1d claimants differ in the consumer's own log.
if [ "${claims##*:}" = "" ]; then
  echo "FAIL: the surviving claimant is the UPLOAD, not the edge's output"
  echo "  $claims"; echo "kept $D"; exit 1
fi
echo "PASS: one claimant for the generated header on the second pass, it is the edge's output, and the object built"
rm -rf "$D"
