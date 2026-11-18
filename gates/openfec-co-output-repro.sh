#!/usr/bin/env bash
# AN EDGE'S OTHER OUTPUTS, at openfec's shape and a thousandth its size.
#
# openfec sets LIBRARY_OUTPUT_PATH to a SIBLING of the build directory, so its
# link edge writes ../bin/Release/libopenfec.so.1.4.2 with the soname link and
# the development alias beside it. `all` names only the versioned file.
# Target resolution walked phony aliases and stopped, so local mode placed the
# versioned file alone and left the two aliases in the store. The build phase
# reported success; cmake's install failed a phase later on a name that had
# been built:
#
#     install: cannot stat '../bin/Release/libopenfec.so'
#
# Reproduced here with one link edge declaring three outputs above the build
# directory, and a phony naming only the versioned one - which is the whole
# mechanism, since a target naming an alias directly always resolved.
#
#   fixed driver   -> all three names present in ../bin/Release
#   pre-fix driver -> the versioned file alone, and the run still exits 0
#
# THE CONTROL IS THE HALF THAT MATTERS. A driver that placed all three for
# some other reason would pass this, so `--control <rev>` builds the driver at
# a revision before 93346c2 and requires it to place exactly one. Without that
# arm a pass is evidence about the reduction.
#
# Needs a builder-rpc-v0 daemon. Run from a checkout.
set -euo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}

SRC=$REPO
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-cogate-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV"
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W
  echo "control: driver built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja"); TASK=$(b "$SRC#nix-ninja-task")
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-cooutput-XXXXXX)
mkdir -p "$D/build" "$D/bin/Release" "$D/src"
echo 'int fec_symbol(void) { return 142; }' > "$D/src/fec.c"

# One edge, three declared outputs, all of them ABOVE the build directory -
# openfec's LIBRARY_OUTPUT_PATH arithmetic. The phony names the versioned
# file only, which is what `all` does in the real package.
cat > "$D/build/build.ninja" <<'EOF'
rule cc
  command = gcc -fPIC -c $in -o $out
rule link
  command = gcc -shared -Wl,-soname,libfec.so.1 fec.o -o ../bin/Release/libfec.so.1.4.2 && ln -sf libfec.so.1.4.2 ../bin/Release/libfec.so.1 && ln -sf libfec.so.1 ../bin/Release/libfec.so
build fec.o: cc ../src/fec.c
build ../bin/Release/libfec.so.1.4.2 ../bin/Release/libfec.so.1 ../bin/Release/libfec.so: link fec.o
build all: phony ../bin/Release/libfec.so.1.4.2
default all
EOF

cd "$D/build"
# set +e AROUND THE DRIVER. With `set -e` in force a non-zero driver exit ends
# the script before `rc=$?` runs, so the presence table never prints and a
# real failure reads as a script crash. Gate 2 had this right and this one
# did not.
set +e
env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" all
rc=$?
set -e

echo "driver rc=$rc"
present=0
for n in libfec.so.1.4.2 libfec.so.1 libfec.so; do
  if [ -e "$D/bin/Release/$n" ] || [ -L "$D/bin/Release/$n" ]; then
    printf '  %-18s present\n' "$n"; present=$((present+1))
  else
    printf '  %-18s MISSING\n' "$n"
  fi
done
echo "placed $present/3"

if [ -n "$CONTROL_REV" ]; then
  if [ "$present" = 1 ]; then
    echo "CONTROL PASS: the pre-fix driver places the versioned file alone,"
    echo "  which is the defect. A build reporting success and an install"
    echo "  failing a phase later is exactly this."
    rm -rf "$D"; exit 0
  fi
  echo "CONTROL FAILED: expected 1 placed, got $present. Either the revision"
  echo "  carries the fix or this reduction does not exercise the class -"
  echo "  and a subject arm passing over it would mean nothing."
  echo "  build dir kept: $D"
  exit 1
fi

if [ "$present" = 3 ]; then
  # PRESENCE IS NOT ENOUGH, and brotli is why. Two fixes for that class
  # passed a presence check while converting the aliases to CONTENT, which
  # loses the soname relationship, freezes each copy at the bytes it held,
  # and makes CMake's install-time rewrite skip them. So ask what the names
  # ARE, not just whether they exist.
  fail=0
  for a in libfec.so libfec.so.1; do
    if [ ! -L "$D/bin/Release/$a" ]; then
      echo "FAIL: $a is a real file, not a link. An alias placed as content"
      echo "  loses the soname relationship - the brotli defect."
      fail=1
    fi
  done
  real=$(readlink -f "$D/bin/Release/libfec.so" || true)
  if [ "$real" != "$D/bin/Release/libfec.so.1.4.2" ]; then
    echo "FAIL: libfec.so resolves to '$real', not to the versioned library."
    fail=1
  fi
  if ! nm -D --defined-only "$D/bin/Release/libfec.so" 2>/dev/null | grep -q fec_symbol; then
    echo "FAIL: the alias does not reach the library's symbols, so the chain"
    echo "  is placed but broken - which is what install would hit."
    fail=1
  fi
  if [ "$fail" = 0 ]; then
    echo "PASS: naming one output of an edge placed every output of that edge,"
    echo "  the aliases are links, and the chain resolves to the library."
    rm -rf "$D"; exit 0
  fi
  echo "build dir kept: $D"
  exit 1
fi
echo "FAIL: $present of 3 placed. build dir kept: $D"
exit 1
