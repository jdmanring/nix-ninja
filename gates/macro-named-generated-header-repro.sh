#!/usr/bin/env bash
# A CONFIGURE-TIME HEADER NAMED ONLY BY A -D MACRO VALUE.
#
# qemu 10.x compiles every emulator translation unit with
# `-DCONFIG_TARGET="i386-softmmu-config-target.h"` and the source says
# `#include CONFIG_TARGET`. The header's name exists as a macro VALUE and as
# a literal in no file, so no read of the sources can name it. meson writes
# the header at SETUP time (`configure_file`), so no edge declares it and
# nothing stages it for the task; the compile reaches it through qemu's own
# `-iquote .`. Two defects met there: the scan recorded the macro use and
# found no define, so it reported itself incomplete; and the preprocessor
# fallback that covers every other computed include forwarded `-I` and
# `-isystem` but dropped `-iquote`, so gcc -MM failed against a narrower
# search path than the compile has, with the sentence the task then died on:
#   <command-line>: fatal error: i386-softmmu-config-target.h: No such file
# The driver keeps the scan's answer by design and the task never receives
# the header. Removed at the consumer with `hostCpuTargets = [ ]` (3,090
# edges) rather than fixed.
#
# TWO ARMS, and the first is the control on the fixture rather than on the
# driver. Arm 1 reaches the header through `-I.`, the spelling the fallback
# always forwarded: it must build on every revision, or the fixture is
# broken and arm 2 says nothing. Arm 2 is qemu: `-iquote .`, the header on
# disk before the drive, declared by no edge. It builds only once the scan
# reads the define off the command line and resolves the use through the
# quote dir, and the gate also asserts the fallback did NOT run, since a
# fallback that now forwards `-iquote` would build it too and hide a scan
# that had stopped resolving.
#
# A FIRST VERSION OF ARM 2 GENERATED THE HEADER BY AN EDGE and passed on the
# tree before the fix: a Built input is staged whatever the scan says, so
# that geometry never was the class. Recorded so it is not written again.
#
# Reduced 2026-09-19 on configuration A.
#
# Configuration A, seconds. Blanket OFF, which is load bearing here as in
# every discovery gate: the header is a build-dir file the walk would
# otherwise carry for the wrong reason.
#   PASS    both objects build and arm 2's drive printed no fallback line
#   FAIL    arm 2 fails, or built through the fallback
#   NO ANSWER  arm 1 fails (fixture broken) or the daemon stalled
#   --control <rev>   expects arm 2 to fail with the class sentence
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
  W=$(mktemp -d -t nn-macrohdr-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || exit 2
TASK=$(b "$SRC#nix-ninja-task") || exit 2
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-macrohdr-XXXXXX); S=$D/src
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
mkdir -p "$S" "$D/b1" "$D/b2"
# The TU names the header ONLY through the macro, and uses what it defines,
# so a compile that received the file but not its bytes fails too.
printf '#include CONFIG_TARGET\nint tn(void){ return sizeof TARGET_NAME; }\n' > "$S/cpu.c"
HDR='#define TARGET_NAME "i386"'
rule() { printf 'rule cc\n  command = gcc %s -DCONFIG_TARGET=\\"i386-softmmu-config-target.h\\" -MD -MQ $out -MF $out.d -o $out -c $in\n  deps = gcc\n  depfile = $out.d\nbuild cpu.o: cc ../src/cpu.c\n' "$1"; }
# Both arms: the header is on disk before the drive and no edge names it,
# which is what `configure_file` leaves behind.
printf '%s\n' "$HDR" > "$D/b1/i386-softmmu-config-target.h"
printf '%s\n' "$HDR" > "$D/b2/i386-softmmu-config-target.h"
rule '-I.' > "$D/b1/build.ninja"
rule '-iquote .' > "$D/b2/build.ninja"

export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 NIX_NINJA_DEBUG_DISCOVERY=1

drive() { (cd "$D/$1" && nix-ninja cpu.o > "$D/$1.log" 2>&1); echo $?; }
stalled() { command grep -q 'DaemonStalled' "$1"; }
CLASS='<command-line>: fatal error: i386-softmmu-config-target.h: No such file or directory'

rc1=$(drive b1)
stalled "$D/b1.log" && { echo "NO ANSWER: arm 1 aborted DaemonStalled"; exit 2; }
[ "$rc1" -eq 0 ] && [ -f "$D/b1/cpu.o" ] || { echo "NO ANSWER: arm 1 (header on disk) failed, so the fixture is broken and arm 2 says nothing"; tail -5 "$D/b1.log"; exit 2; }
echo "arm 1  -I.        built"

rc2=$(drive b2)
stalled "$D/b2.log" && { echo "NO ANSWER: arm 2 aborted DaemonStalled"; exit 2; }
fb=$(command grep -c 'computed include unresolvable by scan' "$D/b2.log"); fb=${fb:-0}
hit=$(command grep -c "$CLASS" "$D/b2.log"); hit=${hit:-0}
echo "arm 2  -iquote .  rc=$rc2 fallback-lines=$fb class-sentence=$hit"

if [ -n "$CONTROL_REV" ]; then
  if [ "$rc2" -ne 0 ] && [ "$hit" -gt 0 ]; then echo "CONTROL PASS: $CONTROL_REV fails arm 2 with the class sentence"; exit 0; fi
  echo "CONTROL FAILED: expected arm 2 to fail with the class sentence at $CONTROL_REV"; exit 1
fi
if [ "$rc2" -eq 0 ] && [ -f "$D/b2/cpu.o" ] && [ "$fb" -eq 0 ]; then
  echo "PASS: a configure-time header named only by a -D value is declared by the scan"; exit 0
fi
if [ "$rc2" -eq 0 ]; then
  echo "FAIL: arm 2 built but through the preprocessor fallback ($fb line(s)), so the scan did not resolve the define"; exit 1
fi
echo "FAIL: arm 2 did not build (the class)"; command grep -m1 -F "$CLASS" "$D/b2.log" | cut -c1-160 | sed 's/^/  /'; exit 1
