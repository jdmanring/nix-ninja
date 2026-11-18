#!/usr/bin/env bash
# A GENERATOR THAT WRITES AN OBJECT BY EXECING THE COMPILER IT NEVER NAMES.
#
# CLOSED at `3ec8201`, and registered in run-all.sh in that same commit.
#
# glib 2.88.3 on configuration B, once the include-directory class was closed.
# systemtap's dtrace compiles the probe object itself:
#
#   dtrace -G -s ../gobject/gobject_probes.d -o gobject/...gobject_probes.o
#
# `command_names_toolchain` reads the command line, finds no toolchain program
# by basename, and the edge carries no `deps = gcc`, so `require_cc` adds
# neither the compiler wrapper to the inputs nor its directory to the emitted
# PATH. The task dies inside python's subprocess with
# `FileNotFoundError: [Errno 2] No such file or directory: 'gcc'`, which is the
# loud failure the gate intends rather than a silent miss.
#
# THE FIXTURE EXECS THROUGH PYTHON for the same reason the package does: a
# shell wrapper naming gcc anywhere on the line would be caught by the very
# allowlist under test, so the fixture would pass without the defect being
# fixed. The compiler's name exists only inside the script.
#
# THE SECOND ARM IS THE NEGATIVE CONTROL AND IT DECIDES THE FIRST. The same
# script, the same interpreter, the same store paths, writing a NON-object
# output. It must keep failing after any fix that gates on output shape, or
# the fix is the unconditional one this design refuses: the compiler wrapper
# is 27 store paths of closure against 14 for everything else a task carries,
# and carrying it everywhere re-keys every task to buy one edge.
#
# Configuration A, seconds. Blanket off. Needs a builder-rpc-v0 daemon.
#   PASS    the object arm builds AND the non-object arm still fails
#   RED     the class has regressed; the verdict prints the interpreter's own
#           line so a reader can tell it from an apparatus failure
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

NN=$(b "$REPO#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep); PY=$(px python3)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-objgen-XXXXXX); S=$D/src
mkdir -p "$S" "$D/build/gen"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
printf 'int probe(void){return 7;}\n' > "$S/probe.c"
# The compiler's name appears ONLY here, never on a command line.
cat > "$S/mkprobe.py" <<'PYEOF'
import subprocess, sys
out, src = sys.argv[1], sys.argv[2]
subprocess.run(["gcc", "-c", src, "-o", out], check=True)
PYEOF
cat > "$D/build/build.ninja" <<NINJA
rule CUSTOM_COMMAND
  command = \$COMMAND
build gen/probe.o: CUSTOM_COMMAND
  COMMAND = $PY/bin/python3 ../src/mkprobe.py gen/probe.o ../src/probe.c
build gen/probe.stamp: CUSTOM_COMMAND
  COMMAND = $PY/bin/python3 ../src/mkprobe.py gen/probe.stamp ../src/probe.c
NINJA
cd "$D/build"
# RESOLVED BEFORE PATH IS REPLACED. Taking it after put `nice` and `nix` off
# the path, both arms died on the apparatus, and the negative control read OK
# because it is satisfied by any failure at all.
# THE COMPILER IS ON THE DRIVER'S PATH, as it is for any real package, and
# that is NOT what the gate tests: a task's PATH is emitted separately, so the
# sandbox still gets the compiler only where `needs_cc` says so. Omitting it
# here was invisible while the gate was red, because `require_cc` is never
# reached when nothing asks for the compiler, and the fix turned it into a
# second failure that read exactly like the first.
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja gen/probe.o     > "$D/object.log"    2>&1; orc=$?
nix-ninja gen/probe.stamp > "$D/nonobject.log" 2>&1; nrc=$?
set -e

# THE NEGATIVE CONTROL FIRST. It must FAIL: an edge whose output is not object
# shaped has no claim on the compiler, and a fix that gave it one would be the
# unconditional carry this design refuses.
if [ $nrc -eq 0 ]; then
  echo "FAIL: the non-object arm BUILT, so the compiler is carried unconditionally"
  echo "  that re-keys every task to buy one edge; the fix must gate on output shape"
  exit 1
fi
echo "control OK: a non-object generator still does not get the compiler"

if [ $orc -eq 0 ] && [ -f gen/probe.o ]; then
  echo "PASS: a generator writing an object reaches the compiler it never names"
  exit 0
fi
# THE VERDICT IS THE MESSAGE AND NOT THE STATUS. Any apparatus failure also
# exits non-zero, so a red run that cannot show the interpreter failing to find
# the compiler is a NO ANSWER and must not be read as the class reproducing.
if command grep -q "No such file or directory: 'gcc'" "$D/object.log"; then
  echo "RED BY DESIGN: the object-writing generator cannot reach the compiler (rc=$orc)"
  command grep -o "FileNotFoundError.*'gcc'" "$D/object.log" | head -1
  exit 1
fi
echo "NO ANSWER: the object arm failed without the interpreter naming gcc (rc=$orc)"
command grep -E 'ABORTED|error' "$D/object.log" | head -3
echo "  load average now: $(cut -d' ' -f1-3 /proc/loadavg), $(nproc) threads"
exit 2
