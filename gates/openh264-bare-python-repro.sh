#!/usr/bin/env bash
# openh264's class: a command naming a python script with NO directory
# component.
#
#   read_dir() for python closure: No such file or directory
#
# `Path::parent` of a bare relative name answers `Some("")` rather than
# `None`, and `read_dir("")` is ENOENT, so the sibling-import closure killed
# the package before any task was named. The EMPTY PARENTHESES are the whole
# signature: without reading them the error looks like a missing interpreter,
# and searching the build graph for `python` returns nothing because the graph
# is not the subject.
#
# `fbbced8` repaired ONE of four call sites that take a parent and hand it to a
# directory read, and the package failed identically at the revision carrying
# it. `e9eb4c3` routes all four through one function. This is the first
# exercise of either: the entry has been UNVERIFIED on a package since it was
# filed, and it was filed because reasoning about it was already wrong once.
#
# The script is named bare AND imports a sibling, so the closure has to walk a
# directory it can only get from that parent.
#
#   local/gates/openh264-bare-python-repro.sh <driver-bin>
set -euo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
NN=${1:?usage: openh264-bare-python-repro.sh <driver-bin>}
[ -x "$NN" ] || { echo "not executable: $NN" >&2; exit 2; }

b() { nice -n 10 nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
TASK=$(b "$REPO#nix-ninja-task")
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); PY=$(px python3)
CC=$(px gcc); BU=$(px binutils)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-oh264-XXXXXX); trap 'chmod -R u+w "$D" 2>/dev/null; rm -rf "$D"' EXIT
cat > "$D/tables.py" <<'EOF'
def rows():
    return ["a", "b", "c"]
EOF
cat > "$D/gen.py" <<'EOF'
import sys
import tables
open(sys.argv[1], "w").write("".join(tables.rows()) + "\n")
EOF
# THE BLANKET IS TURNED OFF ABOVE, AND WITHOUT THAT THIS ARM MEASURED THE
# NEIGHBOUR. `tables.py` sits in the build directory and this rule declares
# no `deps = gcc`, so the implicit-input blanket swept the sibling in and the
# python closure contributed nothing. Measured by disabling the closure
# entirely (`for dir in py_dirs.into_iter().take(0)`): the arm still PASSED,
# output correct, exit 0. With NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 the same
# mutation FAILS the build, so the arm now discriminates its own subject.
# Same shape as the tablegen arm, which passed with its reader disabled
# because a separated `-I dir` let the argument sweep declare the files.
# A gate for a discovery mechanism has to switch OFF every other route that
# could supply the file, and the blanket is the widest of them.
# The command names gen.py with no directory component, which is the subject.
cat > "$D/build.ninja" <<'EOF'
rule genpy
  command = python3 gen.py $out
build table.txt: genpy gen.py
default table.txt
EOF

set +e
out=$( cd "$D" && env \
  PATH="$(dirname "$NN"):$PY/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  NIX_CONFIG="extra-experimental-features = $F" \
  "$NN" table.txt 2>&1 )
rc=$?
set -e
echo "$out" | tail -12
echo "exit $rc"
if [ "$rc" = 0 ] && [ "$(cat "$D/table.txt" 2>/dev/null)" = "abc" ]; then
  echo "VERDICT    the bare script resolved, its sibling was carried, output correct"
  exit 0
elif printf '%s' "$out" | grep -q 'read_dir() for python closure'; then
  echo "VERDICT    REPRODUCED - a bare parent reached a directory read"
  exit 1
else
  echo "VERDICT    failed for some other reason; read the output above"
  exit 1
fi
