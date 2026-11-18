#!/usr/bin/env bash
# A LINKER FLAG WHOSE PATH ARRIVES QUOTED, which is class 17.
#
# CMake's target_link_libraries(ssh PRIVATE "-Wl,--version-script,\"${MAP}\"")
# puts LITERAL double quotes inside LINK_LIBRARIES, so build.ninja carries
# them. The `-Wl` input rule reads the RAW NINJA TEXT - cmdline at that call
# site is never shell-split - so the candidate was the string with the quotes
# attached, Path::is_file answered no about a name no file has, and the map
# was declared by nothing. The link then fails on a file present in the outer
# tree the whole time:
#
#     ld.bfd: cannot open linker script file .../libssh.map
#
# The shell strips the quotes when the command RUNS, so the failure is not
# about quoting the command - it is about which INPUT the driver declared.
# That asymmetry is why the reduction has to carry the quotes in the ninja
# file rather than in the shell invocation.
#
#   fixed driver   -> the map is declared, the link succeeds, the symbol is
#                     global and the local one is hidden
#   pre-fix driver -> cannot open linker script file
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
  W=$(mktemp -d -t nn-lsgate-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV"
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W
  echo "control: driver built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja"); TASK=$(b "$SRC#nix-ninja-task")
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-quotedwl-XXXXXX)
mkdir -p "$D/build" "$D/src"
printf 'int shown(void){return 17;}\nint hidden(void){return 42;}\n' > "$D/src/foo.c"
# The map is named ONCE in the whole graph, inside the quoted flag - which is
# libssh's situation and is what makes the input rule the only thing that can
# declare it.
printf '{ global: shown; local: *; };\n' > "$D/src/foo.map"

cat > "$D/build/build.ninja" <<'EOF'
rule cc
  command = gcc -fPIC -c $in -o $out
rule link
  command = gcc -shared -Wl,--version-script,"../src/foo.map" foo.o -o libfoo.so
build foo.o: cc ../src/foo.c
build libfoo.so: link foo.o
default libfoo.so
EOF

cd "$D/build"
set +e
env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" libfoo.so > "$D/log" 2>&1
rc=$?
set -e
echo "driver rc=$rc"

# A TASK'S OUTPUT REACHES NO STREAM OF THE DRIVER. The link error lives in
# the task DERIVATION's log and the driver's own output carries only "build
# of resolved derivation failed", so the diagnosis has to be fetched with
# `nix log` against the drv the failure names. Written the other way first,
# this gate reported CONTROL FAILED against a control that had reproduced
# libssh's message verbatim.
task_log() {
  local drv
  # `.drv.drv` IS A REAL SHAPE: every DYNAMIC task derivation is named
  # `<hash>-ninja-build.drv.drv`, and a pattern ending at the first `.drv`
  # prints a truncated path that `nix log` cannot resolve - reporting CONTROL
  # FAILED about a control that reproduced. The fixture's edge is plain
  # today, so this is a latent trap rather than a live one.
  drv=$(grep -oE '/nix/store/[a-z0-9]{32}-ninja-build\.drv(\.drv)?' "$D/log" | tail -1)
  [ -n "$drv" ] && nix log "$drv" 2>/dev/null
}

if [ -n "$CONTROL_REV" ]; then
  if [ "$rc" != 0 ] && task_log | grep -q "cannot open linker script file"; then
    echo "CONTROL PASS: the pre-fix driver fails to declare the map, and the"
    echo "  link dies on a file present in the outer tree the whole time."
    rm -rf "$D"; exit 0
  fi
  echo "CONTROL FAILED: expected 'cannot open linker script file' in the"
  echo "  TASK's log, got rc=$rc."
  echo "  Either the revision carries the fix or this reduction does not"
  echo "  exercise the class, and a subject arm over it would mean nothing."
  tail -5 "$D/log"; echo "  kept: $D"; exit 1
fi

[ "$rc" = 0 ] || { echo "FAIL: driver rc=$rc"; tail -8 "$D/log"; echo "kept: $D"; exit 1; }
[ -e "$D/build/libfoo.so" ] || { echo "FAIL: no library"; echo "kept: $D"; exit 1; }
# THE LINK SUCCEEDING IS NOT ENOUGH. A driver that dropped the flag entirely
# would also link, so assert the script TOOK EFFECT: one symbol global, the
# other hidden. Without this the gate passes on a fix that declares nothing
# and merely stops passing the flag.
if nm -D --defined-only "$D/build/libfoo.so" | grep -q ' T shown' &&
   ! nm -D --defined-only "$D/build/libfoo.so" | grep -q ' T hidden'; then
  echo "PASS: the quoted version script was declared, materialized and APPLIED"
  echo "  (shown is global, hidden is not)."
  rm -rf "$D"; exit 0
fi
echo "FAIL: the library linked but the version script did not take effect,"
echo "  so the flag reached the compiler without its file."
nm -D --defined-only "$D/build/libfoo.so" | head -5
echo "kept: $D"; exit 1
