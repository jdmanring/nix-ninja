#!/usr/bin/env bash
# A COMPILE FORCED TO INCLUDE A SOURCE HEADER WITH `-include`.
#
# tinysparql 3.11.1 puts `-include tracker-private.h` in the c_args of
# `libtracker_sparql_private`, so every source of that static library carries
# it and the header is an ordinary SOURCE file shipped in the tarball, which
# no edge declares. The scan reads `#include` directives out of the TU and its
# headers, and a forced include appears in none of them: gcc blames
# `<command-line>: fatal error: tracker-private.h: No such file or directory`,
# naming no source file, which is the signature. Measured in one round as 36
# failed tasks in one package, target-sized rather than file-sized.
#
# TWO ARMS, and the second is what makes the first mean anything:
#
#   1  the forced header sits in a SOURCE directory named by -I, and the
#      object must build. This is the package's own geometry.
#   2  a TU that also #includes an ordinary header from that same directory
#      must build in the same run. A fixture whose only header is the forced
#      one cannot tell "the forced include was carried" from "the whole
#      include chain works", and a broken fixture prints the same failure as
#      a live class.
#
# ARM 2'S DISCRIMINATION RUNS THE OTHER WAY, which its own wording above does
# not say: `both.c` calls the symbol the forced header declares, so it fails
# whenever arm 1 fails and cannot witness an ordinary chain working while the
# forced one does not. What it catches is a seeding that satisfies the forced
# include by REPLACING the scan's answer rather than adding to it, which is the
# regression the fix's ordering guards against.
#
# THE FIXTURE IS VALIDATED AND THE DRIVER HALF IS NOT. Under real ninja both
# objects build from this exact build.ninja, and deleting the header alone
# produces the package's own `<command-line>: fatal error: tracker-private.h:
# No such file or directory`, which is the sentence the log grep below matches.
# So a red run is a statement about the driver rather than about the geometry.
# Everything past the first `nix build` here is still unrun: two attempts died
# `DaemonStalled` against a competing round.
#
# THE HEADER IS NOT EMPTY ON PURPOSE: it declares a symbol the TU uses, so a
# compile that received the file but not its contents fails too, rather than
# passing because nothing depended on it.
#
# Configuration A, seconds. Blanket OFF, which is load bearing: the implicit
# input blanket is the widest carrier in the driver and would supply the
# header for the wrong reason. It is gated `!is_gcc_task` and this edge is
# `deps = gcc`, so it cannot fire here anyway, and the export says so rather
# than relying on it.
#   PASS    both objects build
#   FAIL    otherwise
#   --control <rev>   expects the failure (any rev before the fix)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-4}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=${NN_GATE_SRC:-$REPO}
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-forceinc-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-forceinc-XXXXXX); S=$D/src
mkdir -p "$S/lib" "$D/build/lib"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
# The forced header, and it carries a declaration the TU uses.
printf '#ifndef TRACKER_PRIVATE_H\n#define TRACKER_PRIVATE_H\nint tracker_private_seed(void);\n#endif\n' > "$S/lib/tracker-private.h"
# An ordinary header in the SAME directory, for arm 2's control.
printf '#ifndef ORDINARY_H\n#define ORDINARY_H\nint ordinary_seed(void);\n#endif\n' > "$S/lib/ordinary.h"
# Arm 1: nothing in this TU names the forced header.
printf 'int forced_user(void){ return tracker_private_seed(); }\n' > "$S/lib/forced.c"
# Arm 2: this TU reaches the same directory through an ordinary include.
printf '#include "ordinary.h"\nint both_user(void){ return tracker_private_seed() + ordinary_seed(); }\n' > "$S/lib/both.c"
cat > "$D/build/build.ninja" <<NINJA
rule c_COMPILER
  command = gcc -I../src/lib -include tracker-private.h -MD -MQ \$out -MF \$out.d -o \$out -c \$in
  deps = gcc
  depfile = \$out.d
build lib/forced.c.o: c_COMPILER ../src/lib/forced.c
build lib/both.c.o: c_COMPILER ../src/lib/both.c
NINJA
cd "$D/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja lib/forced.c.o lib/both.c.o > "$D/build.log" 2>&1; brc=$?
set -e
# gcc DECORATES the sentence with colour codes between every phrase even
# with no tty, so a grep for the plain spelling misses it; anchor on the two
# plain phrases either side. Printed as those phrases rather than the raw
# line, because the gate's PATH carries the store tools alone and no sed.
msg=$(command grep -oE 'command-line.*fatal error.*: No such file or directory' "$D/build.log" | head -1 | command grep -oE 'command-line|[a-z_-]+\.h: No such file or directory' | tr '\n' ' ')
if [ -n "$CONTROL_REV" ]; then
  if command grep -q 'DaemonStalled' "$D/build.log"; then
    echo "NO ANSWER: the control aborted DaemonStalled and did not reach the compile"
    exit 2
  fi
  if [ $brc -ne 0 ]; then
    echo "CONTROL PASS: $CONTROL_REV fails the compile (rc=$brc)"
    # THE COMPILER'S OWN SENTENCE, or a statement that it was not reached.
    # A control that fails for another reason is not this class.
    [ -n "$msg" ] && echo "  $msg" || echo "  NOTE: the forced-include error was not in the log; read it before believing this control"
    exit 0
  fi
  echo "CONTROL FAILED: expected the compile to fail at $CONTROL_REV"; exit 1
fi
# A DAEMON ABORT IS NO ANSWER ABOUT THE CLASS, never a red verdict, and this
# gate scored it FAIL for as long as it existed. Its three attempts have all
# ended this way, so the only verdict it has ever printed was the wrong one,
# and registering it would have put a machine-state failure into the corpus
# under this class's name. The same lane was missing one layer up in
# run-all.sh and is fixed there too.
if command grep -q 'DaemonStalled' "$D/build.log"; then
  echo "NO ANSWER: the driver aborted DaemonStalled, so this run measured nothing"
  command grep -m1 'ABORTED' "$D/build.log"
  echo "  READ THE DAEMON LOG BEFORE CONCLUDING THE MACHINE WAS BUSY. Four runs"
  echo "  of this gate ended here and every one was attributed to load; the log"
  echo "  records the driver's connects SUCCEEDING and the worker segfaulting"
  echo "  before it answers, which is why the count is 0 connect failures."
  echo "    grep 'daemon worker .* crashed' /var/log/nix-daemon/current | tail"
  echo "  A crash loop opens no gap, and daemon-gap-cpu-probe.sh now counts it."
  exit 2
fi
if [ $brc -eq 0 ] && [ -f lib/forced.c.o ] && [ -f lib/both.c.o ]; then
  echo "PASS: a forced include naming a source header is carried, and an ordinary include from the same directory still is"
  exit 0
fi
echo "FAIL: rc=$brc forced=$([ -f lib/forced.c.o ] && echo y || echo n) both=$([ -f lib/both.c.o ] && echo y || echo n)"
[ -n "$msg" ] && echo "  $msg"
command grep -E 'ABORTED|fatal error' "$D/build.log" | head -3
exit 1
