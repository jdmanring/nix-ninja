#!/usr/bin/env bash
# A TOOL WHOSE INPUTS ARE LISTED INSIDE A DATA-FILE MANIFEST.
#
# `glib-compile-resources` reads the files named INSIDE a `.gresource.xml`
# and resolves them against `--sourcedir`. The edge declares the XML and
# nothing else, so the task staged the manifest alone and the tool exited:
#
#   ../gio/tests/111_digit_test.gresource.xml: Failed to locate 'test1.txt'
#   in any source directory.
#
# That killed glib on the ninja route, and glib rooted a cascade of 110
# top-level derivations in the consumer's round at pin 463b972. It is NOT
# the empty-include-directory class: the `-Werror=missing-include-dirs`
# workaround was applied there and glib still died here.
#
# The fixture is glib's own shape, down to the manifest text: a source tree
# holding the data file, a build tree holding neither, `--sourcedir` naming
# the source directory absolutely, and the manifest reached by a relative
# path from the build root.
#
# Configuration A, seconds, needs glib's compiler. Blanket OFF, so the
# reader is the only route that can supply the file.
#   PASS    the resource header is generated
#   FAIL    otherwise
#   --control <rev>   expects the failure (any rev before the fix)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CONTROL_REV=""
[ "${1:-}" = "--control" ] && CONTROL_REV=${2:?--control needs a revision}
SRC=${NN_GATE_SRC:-$REPO}
if [ -n "$CONTROL_REV" ]; then
  W=$(mktemp -d -t nn-gres-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$CONTROL_REV" || exit 1
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "control: driver and task binary built from $CONTROL_REV"
fi

NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); GR=$(px gnugrep); GLIB=$(px glib.dev)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)
GCR=$GLIB/bin/glib-compile-resources
[ -x "$GCR" ] || GCR=$(px glib)/bin/glib-compile-resources
[ -x "$GCR" ] || { echo "FAIL: no glib-compile-resources, nothing measured" >&2; exit 2; }

D=$(mktemp -d -t nn-gres-XXXXXX); S=$D/glib-src; mkdir -p "$S/gio/tests" "$D/build/gio/tests"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
# The data file exists in the SOURCE tree only, which is the whole point.
printf 'digits\n' > "$S/gio/tests/test1.txt"
cat > "$S/gio/tests/111_digit_test.gresource.xml" <<'GRES'
<?xml version="1.0" encoding="UTF-8"?>
<gresources>
  <gresource prefix="/digit_test">
    <file>test1.txt</file>
  </gresource>
</gresources>
GRES
cat > "$D/build/build.ninja" <<NINJA
rule CUSTOM_COMMAND
  command = \$COMMAND
build gio/tests/digit_test_resources.h: CUSTOM_COMMAND $S/gio/tests/111_digit_test.gresource.xml
  COMMAND = $GCR --target=gio/tests/digit_test_resources.h --sourcedir=$S/gio/tests --internal --generate --manual-register $S/gio/tests/111_digit_test.gresource.xml
NINJA
cd "$D/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
set +e
nix-ninja gio/tests/digit_test_resources.h > "$D/build.log" 2>&1; brc=$?
set -e

if [ -n "$CONTROL_REV" ]; then
  if [ $brc -ne 0 ]; then
    echo "CONTROL PASS: $CONTROL_REV cannot supply the listed file (rc=$brc)"
    grep -o "Failed to locate[^\"]*" "$D/build.log" | head -1
    exit 0
  fi
  echo "CONTROL FAILED: expected the generate to fail at $CONTROL_REV"; exit 1
fi
if [ $brc -eq 0 ] && [ -f gio/tests/digit_test_resources.h ]; then
  echo "PASS: a file listed inside a gresource manifest is carried and the header generates"
  exit 0
fi
echo "FAIL: rc=$brc"
grep -E 'Failed to locate|ABORTED' "$D/build.log" | head -3
exit 1
