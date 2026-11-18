#!/usr/bin/env bash
# A PLACEHOLDER THE COMPILER SPLIT MUST NOT BE PLACED.
#
# The driver restores the outer-output placeholder in a placed file by a
# byte-for-byte swap of the whole path. gcc 16 at `-march=znver4` copies a
# constant-length literal into a stack buffer as overlapping immediate stores,
# so the path reaches the object in pieces and the swap finds nothing. llvm
# 22.1.8's llvm-config shipped that way on the consumer's compiler route at pin
# 5f0fe07 (`--bindir` printed the placeholder; the real hash was absent) and
# rustc's x.py then failed on a FileCheck path that does not exist.
#
# THE FIX WAS DETECT-THEN-FALL-BACK (a re-exec with NIX_NINJA_RAW_OUTER_PATHS=1)
# and is now DETECT-THEN-REFUSE: `placeholder_residue` scans the restored
# bytes for any 8-char window of a placeholder hash, and on a hit placement
# refuses the file and nothing is built again (79e1d51, 6e558ef).
#
# Configuration A, a REAL one-task build (the defect lives in a placed object,
# which `-t drv` never produces). Needs a builder-rpc-v0 daemon.
#
#   NO ANSWER  plain gcc keeps the path whole (fixture cannot split), or the
#              control driver placed no object, or NN_PIN_REV is unset
#   FAIL       head placed an object carrying placeholder windows, or did not
#              announce the fallback; or the CONTROL placed a clean object,
#              which means the fixture no longer reproduces the class
#   PASS       control ships residue silently, head ships none (it refuses
#              with the residue named; see the verdict below)
#
# A response-file arm tested the fallback's re-exec (c6040a0) and was retired
# with it.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
PIN=${NN_PIN_REV:-}
[ -n "$PIN" ] || { echo "NO ANSWER: NN_PIN_REV unset; the control arm is mandatory"; exit 2; }
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
SRC=${NN_GATE_SRC:-$REPO}
NN_HEAD=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN_HEAD"; exit 2; }
NN_PIN=$(b "git+file://$REPO?rev=$PIN#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN_PIN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

# Same digest the driver uses for output name `out`; it is llvm's placeholder.
PLACEHOLDER=$(python3 -c '
import hashlib
a = b"0123456789abcdfghijklmnpqrsvwxyz"
d = hashlib.sha256(b"nix-ninja-outer-output:out").digest()
print("".join(chr(a[x % 32]) for x in d[:32]))')
FAKE_OUT=/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-fixture-1.0
CFLAGS='-O2 -march=znver4'

D=$(mktemp -d -t nn-split-XXXXXX)
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

# windows FILE: how many 8-char windows of the placeholder hash FILE carries.
# Absolute python: the arms replace PATH, and an empty reading once scored as
# a PASS because `[ "" -eq 0 ]` is an error, not a false.
PY=$(command -v python3) || { echo "NO ANSWER: no python3"; exit 2; }
windows() { "$PY" - "$1" "$PLACEHOLDER" <<'EOF'
import sys
d = open(sys.argv[1], "rb").read(); h = sys.argv[2].encode()
print(sum(1 for i in range(len(h) - 7) if h[i:i+8] in d))
EOF
}

# The llvm-config shape: a constant-length copy of a literal naming $out into
# a stack buffer, which gcc emits as immediate stores at this -march.
setup() { # $1 dir $2 plain|rsp; the path rides in config.h or in the .rsp
  mkdir -p "$1/build"
  if [ "${2:-plain}" = rsp ]; then
    : > "$1/build/config.h"
  else
    printf '#define TOOLS_DIR "%s/bin"\n' "$FAKE_OUT" > "$1/build/config.h"
  fi
  cat > "$1/main.c" <<'EOF'
#include <string.h>
#include "config.h"
void sink(char *);
void bindir(void) { char buf[256]; memcpy(buf, TOOLS_DIR, sizeof(TOOLS_DIR)); sink(buf); }
EOF
  if [ "${2:-plain}" = rsp ]; then
    printf 'rule cc\n  command = %s/bin/gcc @$out.rsp\n  rspfile = $out.rsp\n  rspfile_content = %s -I. -DTOOLS_DIR=\\"%s/bin\\" -c $in -o $out\n  deps = gcc\nbuild main.o: cc ../main.c | config.h\n' \
      "$CC" "$CFLAGS" "$FAKE_OUT" > "$1/build/build.ninja"
  else
    printf 'rule cc\n  command = %s/bin/gcc %s -I. -c $in -o $out\n  deps = gcc\nbuild main.o: cc ../main.c | config.h\n' \
      "$CC" "$CFLAGS" > "$1/build/build.ninja"
  fi
}

# FIXTURE CHECK: with the PLACEHOLDER spelled in, plain gcc must lose the whole
# hash, or no restore could ever miss it and the arms cannot differ.
mkdir -p "$D/fx"
printf '#include <string.h>\nvoid sink(char *);\nvoid f(void){char b[256];memcpy(b,"/nix/store/%s-fixture-1.0/bin",sizeof("/nix/store/%s-fixture-1.0/bin"));sink(b);}\n' \
  "$PLACEHOLDER" "$PLACEHOLDER" > "$D/fx/f.c"
"$CC/bin/gcc" $CFLAGS -c "$D/fx/f.c" -o "$D/fx/f.o" || { echo "NO ANSWER: fixture compile failed"; exit 2; }
if command grep -q -a "$PLACEHOLDER" "$D/fx/f.o"; then
  echo "NO ANSWER: gcc $CFLAGS kept the placeholder whole; the fixture cannot split"; exit 2
fi
fxw=$(windows "$D/fx/f.o")
echo "fixture: whole hash absent, $fxw/25 windows present"

export PATH="$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 out=$FAKE_OUT
unset NIX_NINJA_RAW_OUTER_PATHS

arm() { # $1 label $2 driver [$3 plain|rsp]; prints rc, windows, fallback seen
  setup "$D/$1" "${3:-plain}"
  # The driver resolves itself on PATH, so each arm's own driver goes first.
  ( cd "$D/$1/build" && PATH="$2/bin:$PATH" nix-ninja main.o ) > "$D/$1.out" 2> "$D/$1.err"; rc=$?
  obj="$D/$1/build/main.o"
  if [ -e "$obj" ]; then w=$(windows "$obj"); else w=none; fi
  case $w in none|[0-9]*) ;; *) echo "NO ANSWER: $1 window count unreadable ('$w')"; exit 2 ;; esac
  fb=$(command grep -c 'rebuilding with real outer paths' "$D/$1.err")
  echo "$1: rc=$rc placed-object-windows=$w fallback-lines=$fb"
  eval "${1}_rc=$rc ${1}_w=$w ${1}_fb=$fb"
}
arm control "$NN_PIN"
arm head "$NN_HEAD"

if [ "$control_w" = none ]; then
  echo "NO ANSWER: the control driver placed no object (rc=$control_rc)"; tail -5 "$D/control.err"; exit 2
fi
if [ "$control_w" -eq 0 ]; then
  echo "FAIL: the control placed a clean object, so the fixture no longer reproduces the class"; exit 1
fi
# HEAD REFUSES, IT DOES NOT FALL BACK. 79e1d51 then 6e558ef withdrew the
# re-exec fallback and the in-place repair: placement swaps whole paths and
# refuses any file still carrying residue, and nothing is built again. The
# property this gate protects is unchanged, no placed object carries a
# placeholder piece, so head passes by refusing with the residue named, or by
# placing an object with no window. The response-file arm tested the
# fallback's re-exec, which no longer exists, and is retired with it.
if [ "$head_w" != none ] && [ "$head_w" -ne 0 ]; then
  echo "FAIL: head placed an object carrying $head_w placeholder windows"; tail -8 "$D/head.err"; exit 1
fi
if [ "$head_rc" -ne 0 ] && ! grep -q 'placeholder survived the restore' "$D/head.err"; then
  echo "FAIL: head failed (rc=$head_rc) without naming the residue"; tail -8 "$D/head.err"; exit 1
fi
echo "PASS: the control ships $control_w placeholder windows silently; head ships none (rc=$head_rc, refusing with the residue named when nonzero)"
