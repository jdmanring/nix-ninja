#!/usr/bin/env bash
# DOES THE EMITTED DERIVATION DECLARE THE .td FILES A TABLEGEN RUN READS?
#
# TableGen discovers its includes from a depfile the run itself writes, so on
# a first build nothing declares them and the run dies at the first one:
#
#     could not find include file 'llvm/CodeGen/ValueTypes.td'
#
# 3654 failing edges in one round, every one of them llvm-tblgen, against
# files present in the source output and absent from the task's build
# directory.
#
# WHAT THIS ARM EXISTS FOR, AND IT IS NOT THE READER. The reader's closure
# and its resolution order are pinned by unit tests, and both fail under
# mutation. What NOTHING pinned is that the emission path CALLS it: delete
# the call site and every one of those tests stays green. That is the same
# gap `scan_seeds` has, and this is the cheapest instrument that can see it -
# it reads the EMITTED derivation rather than the function.
#
# TWO LEVELS DELIBERATELY. The tool aborts at the first include it cannot
# open, so a one-level reader moves the failure one file along and a build
# log cannot tell the difference. The second-level file is named by no
# command line and reachable only through the first, so a one-level reader
# fails this arm.
#
# AND THE SEED SITS IN A SIBLING DIRECTORY OF THE BUILD DIRECTORY, which is
# llvm's shape: `-I/build/<pkg>-src/llvm/include` while the build directory
# is `/build/<pkg>/build`. That makes the discovered paths climb out of the
# build directory, which is the part of the upload path this arm covers and
# a same-directory fixture would not.
#
# No real tblgen is needed and using one would be wrong: the subject is what
# the driver DECLARES, not what the tool does. A stand-in named `x-tblgen`
# carries the name the reader keys on.
#
# THE `-I` IS FUSED, `-Idir` AND NOT `-I dir`, AND THE FIRST VERSION OF THIS
# ARM PASSED WITH THE READER DISABLED BECAUSE OF THAT ONE SPACE. A separated
# `-I dir` leaves the directory as a bare token, and the argument sweep
# uploads a referenced file's directory - so all three .td files were
# declared by a route that has nothing to do with this reader, and the arm
# was measuring the sweep. The round's own command uses the fused spelling,
# where the token begins with a dash and the sweep skips it. Caught by
# disabling the call site and watching the arm pass anyway.
#
# AND THE COMMAND CARRIES CMake's `cd <dir> &&` PREFIX, because that is the
# shape llvm's tblgen edges have and it is where the reader was wrong: the
# tool was read as the FIRST token, which answers `cd`, so every real
# invocation was declined while a prefix-free fixture passed. An arm without
# the prefix cannot see that, and this one is the reason it was caught at the
# emission level rather than in a round.
#
# Configuration A, which is enough here because emission is the subject and
# the comparison is within one run.
set -uo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
b() { nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null; }
NN=$(b "$REPO#nix-ninja") || { echo "FAIL: no driver"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task") || { echo "FAIL: no task binary"; exit 2; }
px() { b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
CC=$(px gcc); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
NIXP=""
for p in $(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default"); do
  [ -x "$p/bin/nix" ] && { NIXP=$p; break; }
done
[ -n "$NIXP" ] || { echo "FAIL: no nix"; exit 2; }

D=$(mktemp -d -t nn-tdgate-XXXXXX); trap 'rm -rf "$D"' EXIT
# The SOURCE tree, a sibling of the build tree, exactly as llvm's is.
mkdir -p "$D/src/llvm/include/llvm/IR" "$D/src/llvm/include/llvm/CodeGen" "$D/build" "$D/bin"
printf 'include "llvm/CodeGen/ValueTypes.td"\n' > "$D/src/llvm/include/llvm/IR/Intrinsics.td"
printf '// comment\ninclude "llvm/CodeGen/SDNodeProperties.td"\n' \
  > "$D/src/llvm/include/llvm/CodeGen/ValueTypes.td"
printf 'class P;\n' > "$D/src/llvm/include/llvm/CodeGen/SDNodeProperties.td"

# The stand-in tool. It never runs in this arm - only the emitted derivation
# is read - but it must EXIST, because the driver resolves the binary before
# it emits anything.
cat > "$D/bin/x-tblgen" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$D/bin/x-tblgen"

cat > "$D/build/build.ninja" <<EOF
rule tblgen
  command = cd $D/build && x-tblgen -I$D/src/llvm/include $D/src/llvm/include/llvm/IR/Intrinsics.td -o \$out -d \$out.d
build gen.inc: tblgen
default gen.inc
EOF

cd "$D/build"
DRV=$(env -i HOME="$D" PATH="$NN/bin:$D/bin:$CC/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" -t drv gen.inc 2>"$D/err")
rc=$?
if [ $rc -ne 0 ] || [ -z "$DRV" ]; then
  echo "the driver emitted nothing (rc=$rc); its stderr was:"
  tail -5 "$D/err" | sed 's/^/  /'
  echo "FAIL: no emission, so this arm asserts nothing"
  exit 1
fi

# A POSITIVE CONTROL FIRST. If the seed itself is absent the emission is
# about some other command and every assertion below is vacuous.
seed=$(printf '%s' "$DRV" | grep -c 'Intrinsics\.td')
lvl1=$(printf '%s' "$DRV" | grep -c 'ValueTypes\.td')
lvl2=$(printf '%s' "$DRV" | grep -c 'SDNodeProperties\.td')
echo "seed  Intrinsics.td          $seed"
echo "L1    ValueTypes.td          $lvl1"
echo "L2    SDNodeProperties.td    $lvl2"

fail=0
[ "$seed" -gt 0 ] || { echo "UNEXPECTED: the seed is not in the derivation, so nothing here is a test of the reader"; fail=1; }
[ "$lvl1" -gt 0 ] || { echo "UNEXPECTED: the direct include is undeclared - the reader is not reached from emission"; fail=1; }
[ "$lvl2" -gt 0 ] || { echo "UNEXPECTED: the TRANSITIVE include is undeclared - one level only, which the round's log could not have shown"; fail=1; }
[ $fail -eq 0 ] || { echo; echo "FAIL"; exit 1; }
echo
echo "PASS: the emitted derivation declares the whole .td closure, including"
echo "  the level nothing on the command line names, and the reader is"
echo "  reached from the emission path rather than only from a test."
