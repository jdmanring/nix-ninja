#!/usr/bin/env bash
# AN INSTALL PASS MUST REUSE THE BUILD PASS. The consumer's shim runs the
# driver once for the build phase and again, over the same build directory,
# with NIX_NINJA_MATERIALIZE_ALL for install (and again for a check phase).
# Every edge of the second pass is the same edge as the first, so it must
# emit the same derivation and find it realised. qtbase 6.11.2 (2026-10-17)
# rebuilt the package in its install pass: the second pass read state the
# first pass left in the build directory and emitted different bytes for
# every edge. Three fields differed on one edge:
#   NIX_NINJA_ALIASES   the first pass's placed soname links, read by the
#                       build-dir walk as configure-time aliases
#   NIX_NINJA_INPUTS    the collected depfile read back in place of the scan:
#                       generated headers uploaded from placed copies instead
#                       of bound to their producer
#   NIX_NINJA_MAKE_DIRS an include dir that exists only because the first
#                       pass placed a generated header in it
# The fixture carries all three shapes: a CMAKE_SYMLINK_LIBRARY alias edge, a
# generator whose headers a compile reaches only through an order-only phony
# (CMake's shape) and finds by depfile, and an include dir the generator
# creates. A fourth field followed on qtbase (2026-10-18): an output placed
# over a file configure left (a 0-byte placeholder) read back with the
# placed bytes by an edge whose response file names its directory; the
# fixture's sync edge is syncqt's shape.
#
# NIX_NINJA_RESOLVE_CACHE=0 stands in for the sandbox: it switches off state
# carried between invocations the way NIX_BUILD_TOP does, without the
# recursive-nix upload protocol a plain daemon connection refuses.
#
# Configuration A against a THROWAWAY STORE, so it never talks to the system
# daemon and is safe beside a round. Placement links the build dir to store
# paths, so the throwaway store must appear AT /nix/store: the script re-runs
# itself in a user and mount namespace with the devstore's /nix bound over
# /nix and a private daemon serving it. The devstore must already hold the
# toolchain and the pinned daemon (contrib/devstore.sh builds one; copy the
# daemon in with `nix copy --from 'local?read-only=true'`).
# Verdict:
#   PASS  passes 2 and 3 build no task (no new build log in the store), and
#         each sets aside what earlier passes placed with none of it changed
#   FAIL  otherwise, naming each rebuilt task by its first log line
#   NO ANSWER  the namespace, store or driver could not be prepared
# Usage: install-pass-reuse-repro.sh [REV]   (driver at REV, default HEAD;
# tracked files only, by revision)
# Env: NN_DEVSTORE (default ~/.cache/nn-scratch/ds-stock); NN_IPR_KEEP=1 keeps
# the scratch dir on a PASS
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
DSD=${NN_DEVSTORE:-$HOME/.cache/nn-scratch/ds-stock}
if [ "${1:-}" != --in-ns ]; then
  [ -d "$DSD/store/nix/store" ] || { echo "NO ANSWER: no devstore at $DSD"; exit 2; }
  exec unshare -rm --propagation private "$0" --in-ns "$@"
fi
shift
REV=$(git -C "$REPO" rev-parse "${1:-HEAD}") || { echo "NO ANSWER: no such revision"; exit 2; }
mount --bind "$DSD/store/nix" /nix || { echo "NO ANSWER: could not bind the devstore over /nix"; exit 2; }
# store_find: a streamed find with -quit, never a glob over the store. After
# the bind this searches the devstore.
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
D=$(mktemp -d -t nn-ipr-XXXXXX)
FL="git+file://$REPO?rev=$REV"
px() { b --impure --expr "let p=(builtins.getFlake \"$FL\").inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1"; }
NN=$(b "$FL#nix-ninja"); TASK=$(b "$FL#nix-ninja-task")
CC=$(px gcc); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CM=$(px cmake)
for v in NN TASK CC CU BASH PE CM; do
  [ -n "${!v}" ] || { echo "NO ANSWER: could not build $v in the devstore (see $D/prep.log)"; exit 2; }
done
echo "driver $NN (rev ${REV:0:12})"
mkdir -p "$D/build"; cd "$D/build" || exit 2
# A nonce in every source and in the generator, so pass 1 builds every task
# even when an earlier run left the store holding the fixture's tasks.
NONCE=$(date +%s%N)
printf '/* %s */\n#include "x.h"\n#include <y.h>\nint foo(void){return X + Y;}\n' "$NONCE" > foo.c
printf '/* %s */\nint foo(void); int main(void){return foo()==8?0:1;}\n' "$NONCE" > main.c
printf '/* %s */\n#include "x.h"\nint bar(void){return X;}\n' "$NONCE" > bar.c
cat > build.ninja <<'NINJA'
rule cc
  command = gcc -fPIC -Igen -Igen/sub -MD -MF $out.d -c $in -o $out
  depfile = $out.d
  deps = gcc
rule gen
  command = mkdir -p gen/sub && printf '#define X 7 /* NONCE */\n' > gen/x.h && printf '#define Y 1\n' > gen/sub/y.h
rule link
  command = gcc -shared -Wl,-soname,libfoo.so.1 -o $out $in
rule alias
  command = cmake -E cmake_symlink_library libfoo.so.1.2.3 libfoo.so.1 libfoo.so
rule exe
  command = gcc -o $out main.o libfoo.so.1 -Wl,-rpath,'$$ORIGIN'
rule sync
  command = sh -c 'cat meta/d/meta.json > "$$1"' sh $out @sync.args
rule metagen
  command = printf '{"m":"NONCE"}\n' > $out
build gen/x.h gen/sub/y.h: gen
build gen_target: phony gen/x.h gen/sub/y.h
build foo.o: cc foo.c || gen_target
build main.o: cc main.c
build bar.o: cc bar.c || gen_target
build libfoo.so.1.2.3: link foo.o
build libfoo.so.1 libfoo.so: alias libfoo.so.1.2.3
build app: exe main.o libfoo.so.1 | libfoo.so
build sync.h: sync
build meta/d/meta.json: metagen sync.h
build all: phony libfoo.so app meta/d/meta.json
default all
NINJA
sed -i "s/NONCE/$NONCE/" build.ninja
# Configure's placeholder for an output (qtbase's 0-byte
# src/corelib/meta_types/qt6core_metatypes.json): the args file configure
# wrote (syncqt's @Core_syncqt_args) names the directory holding it as
# `-binaryDir`, the directory is uploaded before the producer has run, and
# pass 1 places the producer's output over the placeholder. Without the
# upload, cat fails and so does pass 1.
mkdir -p meta/d && : > meta/d/meta.json && echo '-binaryDir meta/d' > sync.args
run() { env -i HOME="$D" NIX_REMOTE="$NIX_REMOTE" NIX_NINJA_RESOLVE_CACHE=0 PATH="$NN/bin:$CC/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$CM/bin" NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" "$@"; }
mkdir -p "$LOGS"
pass() {  # pass <arm> <n> <env...>: run, then count build logs newer than the marker
  local arm=$1 n=$2 target=all; shift 2; touch "$D/mark"; sleep 1
  [ "$n" = 2 ] && target=${P2TARGET:-all}
  run env "$@" "$NN/bin/nix-ninja" "$target" >> "$D/$arm.log" 2>&1; local rc=$?
  find "$LOGS" -mindepth 2 -maxdepth 2 -newer "$D/mark" -name '*.drv.bz2' > "$D/$arm.built$n"
  echo "$arm pass $n rc=$rc built=$(wc -l < "$D/$arm.built$n")"
  return $rc
}
fails=0
between() {  # between <arm> <n>: unknown writes after pass n, kept in $D/<arm>.unknown
  local u="w$RANDOM$2" d
  mkdir -p "unknown-$u" && date +%s%N > "unknown-$u/$u.log" && echo "unknown-$u/$u.log" >> "$D/$1.unknown"
  date +%s%N > "$u.dat" && echo "$u.dat" >> "$D/$1.unknown"
  for d in $(find . -mindepth 1 -maxdepth 3 -type d -not -path './.nix-ninja-state*' -not -path './unknown-*' | head -20); do
    date +%s%N > "$d/$u.tmp" && echo "$d/$u.tmp" >> "$D/$1.unknown"
  done
}
rewound() {  # rewound <arm> <n>: pass n set aside what earlier passes placed, all unchanged
  # ninja_state re-stamps recorded outputs after placement; a journal that
  # missed it reads every one as changed and the pass sets none aside, which
  # a fixture whose keys survive visible outputs never shows as a rebuild.
  local l c
  c=$(command grep -a -c 'an earlier pass placed outputs' "$D/$1.log"); c=${c:-0}
  [ "$c" -eq $(($2 - 1)) ] || { echo "  $1: pass $2 printed $c rewind line(s), not $(($2 - 1))"; fails=$((fails + 1)); return; }
  l=$(command grep -a 'an earlier pass placed outputs' "$D/$1.log" | tail -1)
  case $l in
    *"; 0 file(s) and"*) echo "  $1: pass $2 set no file aside: $l"; fails=$((fails + 1)) ;;
    *"; 0 changed since placement stay"*) ;;
    *) echo "  $1: pass $2 left placed files visible: $l"; fails=$((fails + 1)) ;;
  esac
}
arm() {  # arm <name> <executable to run after the passes>
  local a=$1 exe=$2
  pass "$a" 1 || { echo "NO ANSWER: $a pass 1 failed (log $D/$a.log)"; exit 2; }
  [ -s "$D/$a.built1" ] || { echo "NO ANSWER: $a pass 1 built nothing, so the counter cannot be trusted"; exit 2; }
  # BETWEEN PHASES ANYTHING MAY WRITE INTO THE BUILD DIR (a check run's
  # logs, a hook's output, bytecode). Unknown names, in a new directory, at
  # the top, and inside every directory configure made (filling one that
  # configure left empty), so no list of known writers is involved.
  between "$a" 1
  pass "$a" 2 NIX_NINJA_MATERIALIZE_ALL=1 || { echo "  $a: pass 2 failed (log $D/$a.log)"; fails=$((fails + 1)); return; }
  rewound "$a" 2
  between "$a" 2
  pass "$a" 3 NIX_NINJA_MATERIALIZE_ALL=1 || { echo "  $a: pass 3 failed (log $D/$a.log)"; fails=$((fails + 1)); return; }
  rewound "$a" 3
  "$exe" || { echo "  $a: $exe exits non-zero"; fails=$((fails + 1)); }
  local u
  for u in $(cat "$D/$a.unknown"); do
    [ -f "$u" ] || { echo "  $a: a file written between phases is gone: $u"; fails=$((fails + 1)); }
  done
  # EVERY LATER PASS SAYS WHAT IT REUSED, and pass 1 says nothing: both
  # later passes reuse what pass 1 wrote, derive nothing fresh and fall
  # back on nothing. A round's tripwire reads the line.
  local lp
  lp=$(command grep -a 'later-pass reused' "$D/$a.log")
  [ "$(printf '%s\n' "$lp" | command grep -cE 'later-pass reused [1-9][0-9]* task\(s\), derived 0 fresh, 0 fallback, 0 stale')" -eq 2 ] \
    && [ "$(printf '%s\n' "$lp" | command grep -c .)" -eq 2 ] \
    || { echo "  $a: later-pass lines are not two clean reuses: $lp"; fails=$((fails + 1)); }
  local f
  for f in $(cat "$D/$a.built2" "$D/$a.built3"); do
    printf '  %s rebuilt: %s\n' "$a" "$(bzcat "$f" 2>/dev/null | command grep -a -m1 -E 'Building|Linking|Running|Generating|Copying' | cut -c1-120)"
    fails=$((fails + 1))
  done
}
# THE NINJA ARM'S SECOND PASS IS A CHECK PASS: stdenv runs check before
# install, and a check target's closure can leave out an output placed over a
# configure file (here meta/d/meta.json). The install pass after it must still
# read configure's version.
P2TARGET=app arm ninja ./app
# THE FALLBACK COUNT CAN LEAVE ZERO: one recorded task pointed at a
# derivation not in the store is derived again, and counted.
st=$(find .nix-ninja-state -mindepth 1 -maxdepth 1 -type d -print -quit)
awk 'NR==2{sub(/\t\/nix\/store\/[a-z0-9]+-/, "\t/nix/store/00000000000000000000000000000000-")}1' \
  "$st/emitted.v1" > "$st/emitted.v1.t" && mv "$st/emitted.v1.t" "$st/emitted.v1"
run "$NN/bin/nix-ninja" all >> "$D/ninja.log" 2>&1 || { echo "  ninja: the fallback pass failed"; fails=$((fails + 1)); }
l=$(command grep -a 'later-pass reused' "$D/ninja.log" | tail -1)
case $l in
  *", 1 fallback, "*": /nix/store/00000000000000000000000000000000-"*) ;;
  *) echo "  ninja: a recorded task not in the store: $l"; fails=$((fails + 1)) ;;
esac
# A SOURCE EDITED BETWEEN PHASES SENDS ITS READERS BACK TO DERIVATION. They
# are recorded edges, so they land in NEITHER `reused` nor `fresh` (fresh is
# keys no earlier pass recorded); the `re-derived` field is what names them.
# Before that field existed this arm asserted nonzero `stale`, from the
# contract before 87d6c6b, and could not be corrected: the behaviour it
# wanted to see had no way to appear on the line.
printf '/* edited */\n' >> foo.c
run "$NN/bin/nix-ninja" all >> "$D/ninja.log" 2>&1 || { echo "  ninja: the stale pass failed"; fails=$((fails + 1)); }
l=$(command grep -a 'later-pass reused' "$D/ninja.log" | tail -1)
case $l in
  *"; re-derived "[1-9]*": "*) ;;
  *|"") echo "  ninja: an edited source re-derived no recorded edge: $l"; fails=$((fails + 1)) ;;
esac
# A DECLARED SOURCE REMOVED BETWEEN PHASES is counted too: its edge no
# longer resolves, and the derivation pass 1 wrote is placed and counted.
mv main.c main.c.away
run "$NN/bin/nix-ninja" all >> "$D/ninja.log" 2>&1 || true
mv main.c.away main.c
l=$(command grep -a 'later-pass reused' "$D/ninja.log" | tail -1)
case $l in
  *", 0 stale: "*|"") echo "  ninja: a removed source counted no stale edge: $l"; fails=$((fails + 1)) ;;
esac
# later <what> <want> <target>: one more pass; its line must hold <want>.
later() {
  run "$NN/bin/nix-ninja" "$3" >> "$D/ninja.log" 2>&1 || { echo "  ninja: the $1 pass failed"; fails=$((fails + 1)); }
  l=$(command grep -a 'later-pass reused' "$D/ninja.log" | tail -1)
  case $l in *"$2"*) ;; *) echo "  ninja: $1: wanted '$2': $l"; fails=$((fails + 1)) ;; esac
}
# WITH THE SOURCE PUT BACK NOTHING IS STALE, and a hook that rewrites a
# generated file byte for byte, or only touches it, changes nothing a
# reused edge read (the rewind notes both: the stamp moved).
head -n -1 foo.c > "$D/foo.c" && cat "$D/foo.c" > foo.c
later unchanged ', 0 fallback, 0 stale: ' all
# A GENERATED FILE A HOOK EDITS BETWEEN PHASES is counted, with foo.c put
# back so a compile that read the header is all that can count. Pass 1
# placed only its targets, so foo.o's record took gen/x.h from the first
# pass that placed it.
cp gen/x.h "$D/x.h"
printf '/* hook */\n' >> gen/x.h
run "$NN/bin/nix-ninja" all >> "$D/ninja.log" 2>&1 || { echo "  ninja: the generated-edit pass failed"; fails=$((fails + 1)); }
l=$(command grep -a 'later-pass reused' "$D/ninja.log" | tail -1)
case $l in
  *", 0 stale: "*|"") echo "  ninja: an edited generated file counted no stale edge: $l"; fails=$((fails + 1)) ;;
esac
# The hook puts the pass-1 bytes back (this pass places no gen/x.h, so the
# edit stood until now).
cat "$D/x.h" > gen/x.h
later restore-gen ', 0 stale: ' all
touch gen/x.h
later touch-gen ', 0 stale: ' all
# A READER DERIVED FRESH AFTER A HOOK'S EDIT: bar.o's scan finds gen/x.h as
# a derived input, so its pass places the header over the edit, and bar.o,
# which read placement's bytes, is not stale on the pass after. (An edit a
# pass leaves standing under a fresh reader is not reached this way; the
# unit test an_edit_left_standing_counts_only_what_was_written_before_it
# models it.)
printf '/* hook, left standing */\n' >> gen/x.h
later standing-derive 'derived 1 fresh, 0 fallback, 0 stale: ' bar.o
later standing-reuse 'derived 0 fresh, 0 fallback, 0 stale: ' bar.o
# AN ERROR EXIT STILL REPORTS.
c=$(command grep -a -c 'later-pass reused' "$D/ninja.log")
run "$NN/bin/nix-ninja" no-such-target >> "$D/ninja.log" 2>&1 && { echo "  ninja: a missing target succeeded"; fails=$((fails + 1)); }
[ "$(command grep -a -c 'later-pass reused' "$D/ninja.log")" -eq $((c + 1)) ] \
  || { echo "  ninja: an error exit printed no later-pass line"; fails=$((fails + 1)); }

# THE CMAKE ARM: what CMake really writes, which a hand-written manifest does
# not reach. A custom command writes a header tree as byproducts of a stamp
# (include/Foo/*.h and a versioned private dir), the library names the
# include dirs as absolute paths and carries VERSION/SOVERSION, and the
# compile reaches the tree only through the target's order-only dependency.
# The headers are DECLARED: an undeclared tree from a plain custom command
# fails the build pass itself (a.h not found), a separate defect.
cm_arm() {  # cm_arm <name> <BYPRODUCTS list>
mkdir -p "$D/$1/src" "$D/$1/build"; cd "$D/$1/src" || exit 2
cat > CMakeLists.txt <<'CMAKE'
cmake_minimum_required(VERSION 3.20)
project(ipr C)
set(GEN ${CMAKE_BINARY_DIR}/include/Foo)
add_custom_command(OUTPUT ${CMAKE_BINARY_DIR}/foo_syncqt_timestamp
  COMMAND sh ${CMAKE_SOURCE_DIR}/gen.sh ${GEN} ${CMAKE_BINARY_DIR}/foo_syncqt_timestamp
  BYPRODUCTS BYPRODUCTS_LIST
  DEPENDS ${CMAKE_SOURCE_DIR}/gen.sh)
add_custom_target(foo_sync DEPENDS ${CMAKE_BINARY_DIR}/foo_syncqt_timestamp)
add_library(foo SHARED foo.c)
set_target_properties(foo PROPERTIES VERSION 1.2.3 SOVERSION 1)
target_include_directories(foo PRIVATE ${GEN} ${GEN}/1.0 ${GEN}/1.0/Foo)
add_dependencies(foo foo_sync)
add_executable(app main.c)
target_link_libraries(app foo)
CMAKE
sed -i "s|BYPRODUCTS_LIST|$2|" CMakeLists.txt
cat > gen.sh <<'GEN'
set -e
mkdir -p "$1/1.0/Foo/private"
for h in a b c; do echo "#define V_$h 1 /* NONCE */" > "$1/$h.h"; done
echo '#define FOO_VERSION 1' > "$1/FooVersion"
echo '#include <a.h>' > "$1/1.0/Foo/private/p.h"
touch "$2"
GEN
sed -i "s|NONCE|$NONCE-$1|" gen.sh
printf '/* %s */\n#include <a.h>\n#include <b.h>\n#include <private/p.h>\nint foo(void){return V_a + V_b;}\n' "$NONCE-$1" > foo.c
printf '/* %s */\nint foo(void); int main(void){return foo()==2?0:1;}\n' "$NONCE-$1" > main.c
cd "$D/$1/build" || exit 2
run "$CM/bin/cmake" -G Ninja -DCMAKE_MAKE_PROGRAM="$NN/bin/nix-ninja" -DCMAKE_C_COMPILER=gcc ../src > "$D/$1.configure.log" 2>&1 \
  || { echo "NO ANSWER: cmake configure failed ($D/$1.configure.log)"; exit 2; }
arm "$1" ./app

}
cm_arm cmake '${GEN}/a.h ${GEN}/b.h ${GEN}/c.h ${GEN}/1.0/Foo/private/p.h'
# THE QT ARM: syncqt's shape. The command declares one header of the tree
# and writes the rest undeclared, so the driver carries include/Foo as the
# edge's undeclared tree (common_include_root); a later pass finds the
# placed tree's files on disk, which qtbase's install pass uploaded one by
# one in place of the tree.
cm_arm qt '${GEN}/FooVersion'
if [ "$fails" -eq 0 ]; then echo "PASS: install and check passes reused every build-pass task, every arm"; [ -n "${NN_IPR_KEEP:-}" ] && echo "kept $D" || rm -rf "$D"; exit 0; fi
echo "FAIL: $fails finding(s) after the build pass; kept $D"; exit 1
