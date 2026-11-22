#!/usr/bin/env bash
# krb5's include class, reduced: a QUOTED include resolved through a DOTDOT
# -I, compiled from a subdirectory of the build tree.
#
# Seen in a configuration B build of krb5:
#   threads.c:28:10: fatal error: k5-platform.h: No such file or directory
#   task cwd  /build/krb5-1.22.2/src/util/support
#   flags     -I../../include
#
# The header is at src/include/k5-platform.h, two levels up from the task's
# cwd. It reads like a525636's class and is not the same shape: there the
# DOTDOT is in the include SPELLING and the resolved file is a sibling source;
# here the spelling is a bare quoted name and the dotdot is in the SEARCH PATH.
# The two reach different code, so one being fixed says nothing about the other.
#
#   resolves and compiles  -> the class is not this
#   fatal error, same text -> reproduced at a thousandth the size
#
# Configuration A, on the route krb5 takes: autotools, so a compiler drop-in
# turns each `gcc -c` into a one-edge build.ninja and runs the driver on it,
# rather than ninja reading a whole graph. The edge below is that file.
#
#   gates/krb5-dotdot-include-repro.sh <driver-bin>
#
# <driver-bin> is a store path, e.g. `nix build .#nix-ninja --print-out-paths`
# plus /bin/nix-ninja. OBJSUF=so names the object with a non-object suffix,
# which is the arm that discriminates (see the mutation note at the end).
set -euo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
NN=${1:?usage: krb5-dotdot-include-repro.sh <driver-bin>}
[ -x "$NN" ] || { echo "not executable: $NN" >&2; exit 2; }

b() { nice -n 10 nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
TASK=$(b "$REPO#nix-ninja-task")
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
SED=$(px gnused); GREP=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

TOP=$(mktemp -d -t nn-krb5-XXXXXX); trap 'chmod -R u+w "$TOP" 2>/dev/null; rm -rf "$TOP"' EXIT
mkdir -p "$TOP/src/include" "$TOP/src/util/support" "$TOP/.nn-cc"
# The header sits where only the dotdot -I can reach it. Nothing in the
# compile's own directory resolves it, so a pass here cannot be an accident
# of the source directory being searched first.
printf '#pragma once\n#define K5_PLATFORM_TAG 5150\n' > "$TOP/src/include/k5-platform.h"
printf '#include "k5-platform.h"\nint k5_threads(void){return K5_PLATFORM_TAG;}\n' > "$TOP/src/util/support/threads.c"

# ONE EDGE, exactly what the drop-in writes for
# `gcc -c threads.c -o threads.o -I../../include -I../../include`: the real
# compiler by store path, the -I fused to its value, `deps = gcc`, and the
# driver run from the compile's directory with the build-dir walk OFF. The
# walk is off because a gcc task takes its headers from `deps = gcc`, so only
# discovery can carry the header; a gate with the walk on could pass by the
# walk sweeping src/include in.
OBJ="threads.${OBJSUF:-o}"
cat > "$TOP/.nn-cc/edge.ninja" <<EOF
rule cc
  command = $CC/bin/gcc -c threads.c -o $OBJ -I../../include -I../../include
  deps = gcc
build $OBJ: cc threads.c
EOF

set +e
out=$( cd "$TOP/src/util/support" && env \
  PATH="$(dirname "$NN"):$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$SED/bin:$GREP/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="extra-experimental-features = $F" \
  NIX_NINJA_NO_BUILD_DIR_SCAN=1 \
  "$NN" -C "$PWD" -f "$TOP/.nn-cc/edge.ninja" "$OBJ" 2>&1 )
rc=$?
set -e
echo "$out" | tail -20
echo "exit $rc"
# MUTATION-CHECKED 2026-09-04 against a driver whose command_is_compile
# answers false (the pre-f381c92 behaviour), built in the store with its
# unit tests skipped because one of them fails on that mutation, which is
# the first detector. OBJSUF=o stayed green (the name test still admits an
# object) and OBJSUF=so reproduced the error above verbatim. So the pair
# discriminates the predicate: neither the walk (the edge runs with
# NIX_NINJA_NO_BUILD_DIR_SCAN=1) nor the argument sweep (the -I is fused)
# can carry the header. A driver built by cargo outside the store cannot be
# used here: the driver refuses a non-store path for itself before any task.
if [ "$rc" = 0 ] && [ -e "$TOP/src/util/support/threads.${OBJSUF:-o}" ]; then
  echo "VERDICT    resolved through the dotdot -I and compiled - not reproduced here"
  exit 0
elif printf '%s' "$out" | grep -q 'k5-platform.h: No such file'; then
  echo "VERDICT    REPRODUCED - the header was not materialized into the task"
  exit 1
else
  echo "VERDICT    failed for some other reason; read the output above"
  exit 1
fi
