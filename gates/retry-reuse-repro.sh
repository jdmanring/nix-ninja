#!/usr/bin/env bash
# A RETRY REUSES THE KILLED ATTEMPT'S RECORD. A signal-9 kill takes the
# sandbox filesystem with it, so the next attempt starts with no local state
# and re-derives every task: qtwebengine's iters 1-4 each paid ~15 min of
# resolve plus ~25-32 GB of re-emitted derivation JSON before dying again.
# Where the retry loop names an explicit directory outside the build
# (NIX_NINJA_DISCOVERY_CACHE_DIR, the same one the discovery cache rides),
# each pass journals every edge it derives there and merges on a clean exit;
# a first pass that finds no local record but finds the killed attempt's
# loads it exactly like an earlier pass's.
#
# The fixture is two compiles and a link, named as target `app` on every
# run (before dd8b4ef the driver built no `default`, so with no target every
# run aborted and this arm was NO ANSWER from 275c956 to 2026-11-15; naming
# the target keeps it independent of that change). Run 1 builds them; the build
# directory is then wiped and recreated byte for byte at the SAME path (a
# retry runs the same derivation, so /build is the same path with a fresh
# filesystem; the attempt key hashes absolute paths, so another path would
# be another key). Run 2 shares only the loop directory with run 1.
# Verdict:
#   PASS  run 2 prints one later-pass line reusing every run-1 task with
#         nothing fresh, no fallback, nothing stale, and builds no new task;
#         and the control run without the loop directory says nothing (a
#         first pass), proving the line is about the attempt record and not
#         the fixture
#   FAIL  otherwise, naming what run 2 did instead
#   NO ANSWER  the namespace, store or driver could not be prepared
# Usage: retry-reuse-repro.sh [REV]   (driver at REV, default HEAD;
# tracked files only, by revision)
# Env: NN_DEVSTORE (default ~/.cache/nn-scratch/ds-stock); NN_RR_KEEP=1 keeps
# the scratch dir on a PASS
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
DSD=${NN_DEVSTORE:-$HOME/.cache/nn-scratch/ds-stock}
if [ "${1:-}" != --in-ns ]; then
  [ -d "$DSD/store/nix/store" ] || { echo "NO ANSWER: no devstore at $DSD"; exit 2; }
  exec unshare -rm --propagation private "$0" --in-ns "$@"
fi
shift
REV=$(git -C "$REPO" rev-parse "${1:-HEAD}") || { echo "NO ANSWER: no such revision"; exit 2; }
mount --bind "$DSD/store/nix" /nix || { echo "NO ANSWER: could not bind the devstore over /nix"; exit 2; }
NIXP=$(find /nix/store -maxdepth 1 -name '*-nix-2.36.0pre20260825_3aef07e' -print -quit)
[ -x "$NIXP/bin/nix-daemon" ] || { echo "NO ANSWER: the devstore holds no pinned daemon"; exit 2; }
NS=$DSD/ns; mkdir -p "$NS"; rm -f "$NS/socket"
cat > "$NS/nix.conf" <<CONF
build-users-group =
sandbox = false
trusted-users = root
require-drop-supplementary-groups = false
experimental-features = $F
CONF
NIX_CONF_DIR=$NS NIX_DAEMON_SOCKET_PATH=$NS/socket "$NIXP/bin/nix-daemon" --extra-experimental-features "$F" > "$NS/daemon.log" 2>&1 &
DPID=$!
trap 'kill $DPID 2>/dev/null' EXIT
for _ in 1 2 3 4 5 6 7 8 9 10; do [ -S "$NS/socket" ] && break; sleep 1; done
[ -S "$NS/socket" ] || { echo "NO ANSWER: the private daemon never opened its socket"; tail -3 "$NS/daemon.log"; exit 2; }
export NIX_REMOTE="unix://$NS/socket"
PATH=$NIXP/bin:$PATH
LOGS=/nix/var/log/nix/drvs
b() { nix build --extra-experimental-features "$F" --cores 4 --max-jobs 2 --no-link --print-out-paths "$@" 2>>"$D/prep.log" | tail -1; }
D=$(mktemp -d -t nn-rr-XXXXXX)
FL="git+file://$REPO?rev=$REV"
px() { b --impure --expr "let p=(builtins.getFlake \"$FL\").inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1"; }
NN=$(b "$FL#nix-ninja"); TASK=$(b "$FL#nix-ninja-task")
CC=$(px gcc); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
for v in NN TASK CC CU BASH PE; do
  [ -n "${!v}" ] || { echo "NO ANSWER: could not build $v in the devstore (see $D/prep.log)"; exit 2; }
done
echo "driver $NN (rev ${REV:0:12})"
mkdir -p "$D/pristine" && cd "$D/pristine" || exit 2
# A NONCE PER INVOCATION, SHARED BY EVERY RUN IN IT. The devstore persists,
# so a fixed fixture emits the derivations an earlier invocation already
# built whenever the task binary has not moved, run 1 builds nothing, and
# the gate can only refuse. Measured 2026-11-22: foo.o, main.o and app were
# registered 2026-11-18 and every corpus run since read NO ANSWER here.
# The nonce sits in both sources so all three tasks are new; it is the same
# in every run below, so run 2 still faces run 1's derivations.
N=${D##*/}
printf '#include "x.h"\n/* %s */\nint foo(void){return X;}\n' "$N" > foo.c
printf '/* %s */\nint foo(void); int main(void){return foo()==1?0:1;}\n' "$N" > main.c
printf '#define X 1\n' > x.h
cat > build.ninja <<'NINJA'
rule cc
  command = gcc -MD -MF $out.d -c $in -o $out
  depfile = $out.d
  deps = gcc
rule exe
  command = gcc -o $out $in
build foo.o: cc foo.c
build main.o: cc main.c
build app: exe foo.o main.o
default app
NINJA
run() { env -i HOME="$D" NIX_REMOTE="$NIX_REMOTE" NIX_NINJA_RESOLVE_CACHE=0 PATH="$NN/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$NIXP/bin:$TASK/bin" NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" "$@"; }
mkdir -p "$LOGS" "$D/loop"
fails=0
fresh() { rm -rf "$D/b1"; mkdir -p "$D/b1"; cp -r "$D/pristine/." "$D/b1/"; cd "$D/b1" || exit 2; }
# Run 1: the killed attempt. It derives and builds every edge.
fresh
touch "$D/mark"; sleep 1
run env NIX_NINJA_DISCOVERY_CACHE_DIR="$D/loop" "$NN/bin/nix-ninja" app > "$D/run1.log" 2>&1 \
  || { echo "NO ANSWER: run 1 failed (log $D/run1.log)"; exit 2; }
find "$LOGS" -mindepth 2 -maxdepth 2 -newer "$D/mark" -name '*.drv.bz2' > "$D/run1.built"
[ -s "$D/run1.built" ] || { echo "NO ANSWER: run 1 built nothing, so the counter cannot be trusted"; exit 2; }
[ -n "$(ls "$D/loop/attempts" 2>/dev/null)" ] || { echo "FAIL: run 1 journaled nothing to the loop directory"; fails=$((fails+1)); }
# Run 2: the retry. Same path, wiped filesystem, only the loop dir shared.
fresh
touch "$D/mark"; sleep 1
run env NIX_NINJA_DISCOVERY_CACHE_DIR="$D/loop" "$NN/bin/nix-ninja" app > "$D/run2.log" 2>&1 \
  || { echo "FAIL: the retry failed (log $D/run2.log)"; fails=$((fails+1)); }
find "$LOGS" -mindepth 2 -maxdepth 2 -newer "$D/mark" -name '*.drv.bz2' > "$D/run2.built"
l=$(command grep -a 'later-pass reused' "$D/run2.log" | tail -1)
case $l in
  *"reused 3 task(s), derived 0 fresh, 0 fallback, 0 stale: "*) ;;
  *) echo "FAIL: the retry reused nothing cleanly: $l"; fails=$((fails+1)) ;;
esac
if [ -s "$D/run2.built" ]; then echo "FAIL: the retry rebuilt $(wc -l < "$D/run2.built") task(s)"; fails=$((fails+1)); fi
# CONTROL: the same wiped retry with no loop directory is a first pass and
# says nothing. If it printed a reuse line, the line would be about the
# fixture, not the attempt record, and this gate could not discriminate.
fresh
run "$NN/bin/nix-ninja" app > "$D/run3.log" 2>&1 \
  || { echo "FAIL: the control run failed (log $D/run3.log)"; fails=$((fails+1)); }
if command grep -aq 'later-pass reused' "$D/run3.log"; then
  echo "FAIL: the control run without the loop directory reused: $(command grep -a 'later-pass reused' "$D/run3.log" | tail -1)"
  fails=$((fails+1))
fi
# Run 4: NO TARGET NAMED, so the fixture's `default app` decides. This pins
# build()'s call to requested_targets, which no cargo test reaches. Its
# control is this gate at a revision before dd8b4ef, where the driver
# refused "at least one target is required" and this arm FAILS.
fresh
run "$NN/bin/nix-ninja" > "$D/run4.log" 2>&1 \
  || { echo "FAIL: a drive naming no target failed; the default is app (log $D/run4.log)"; fails=$((fails+1)); }
[ -x "$D/b1/app" ] || { echo "FAIL: a drive naming no target did not place the default target app"; fails=$((fails+1)); }
if [ "$fails" -eq 0 ]; then echo "PASS: a retry reused every killed-attempt task, every arm"; [ -n "${NN_RR_KEEP:-}" ] && echo "kept $D" || rm -rf "$D"; exit 0; fi
echo "FAIL: $fails finding(s); kept $D"; exit 1
