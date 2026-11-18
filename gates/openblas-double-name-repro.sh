#!/usr/bin/env bash
# openblas's install-path class: an already-materialized build product
# re-uploaded under a name derived from its own store path.
#
#   add_to_store of "2j9lfgwk...-sgtts2.f.o"
#   could not add path '/nix/store/syw437nm...-2j9lfgwk...-sgtts2.f.o'
#
# `new_opaque_file` runs `fs::canonicalize`, which FOLLOWS a symlink. A
# materialized build product was a symlink into the store, so the canonical
# path was the store path and `file_name()` was a basename already carrying a
# hash; the upload then minted a second hash over it. It appears only under
# materialize-everything, because that is the mode that places every output in
# the tree and walks it.
#
# ae7e6d1 places build products as reflink COPIES so `make` can stat them.
# A copy is a real file, so `canonicalize` returns the build-dir path and the
# name cannot carry a hash. That was recorded as a PREDICTION and is what this
# gate decides.
#
# The shape: one edge compiles, a second names the first edge's output ONLY in
# its command, so the product reaches the second task through the build-dir
# walk rather than as a declared input - which is how openblas's install-time
# "build all" reaches a test object the ordinary build never needed.
#
# THIS REDUCTION DOES NOT REPRODUCE THE CLASS, AND THAT IS ITS RESULT. Run
# against a9c7095, before the fix, it completes with no double-named add. So
# it says nothing about whether ae7e6d1 removes the defect, and the prediction
# in CLAUDE.md stays a prediction.
#
# What it DOES bound, which is worth more than another attempt: a product that
# the GRAPH KNOWS does not take the opaque-upload path at all. Materialized as
# a symlink and then reached by a later edge through the build-dir walk, it
# resolves to its existing derived path and `new_opaque_file` is never called.
# openblas's object was a TEST object from a tree the check phase had just
# built, which the ordinary build never needed, so the next attempt needs a
# file in the build directory that is materialized and is NOT a graph output.
# Do not build a third variant of this one.
#
#   local/gates/openblas-double-name-repro.sh <driver-bin>
set -euo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
NN=${1:?usage: openblas-double-name-repro.sh <driver-bin>}
[ -x "$NN" ] || { echo "not executable: $NN" >&2; exit 2; }

b() { nice -n 10 nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
TASK=$(b "$REPO#nix-ninja-task")
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-obl-XXXXXX); trap 'chmod -R u+w "$D" 2>/dev/null; rm -rf "$D"' EXIT
printf 'int sgtts2(void){return 2;}\n' > "$D/sgtts2.c"
# TWO INVOCATIONS, which is the shape the single-graph version could not make.
# Run 1 builds and materializes the object. Run 2 is a DIFFERENT graph that
# does not declare it and only names it in a command, so it reaches the second
# driver as a build-directory file its graph knows nothing about - which is how
# openblas's install-time "build all" meets a test object the check phase had
# already built and placed.
cat > "$D/one.ninja" <<'EOF'
rule cc
  command = gcc -c $in -o $out
build sgtts2.o: cc sgtts2.c
default sgtts2.o
EOF
printf 'int other(void){return 3;}
' > "$D/other.c"
cat > "$D/two.ninja" <<'EOF'
rule pack
  command = gcc -c $in -o $out && cp sgtts2.o packed-copy.o
build packed.o: pack other.c
default packed.o
EOF

set +e
run() {
  ( cd "$D" && env \
    PATH="$(dirname "$NN"):$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
    NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
    NIX_NINJA_MATERIALIZE_ALL=1 \
    NIX_CONFIG="extra-experimental-features = $F" \
    "$NN" -f "$1" "$2" 2>&1 )
}
out1=$(run one.ninja sgtts2.o); rc1=$?
echo "run 1 exit $rc1"
# What run 1 LEFT is the subject: a symlink is the defect's precondition and a
# real file is the fix. Read it before run 2, because run 2's behaviour is
# only interpretable against it.
if [ -L "$D/sgtts2.o" ]; then placed=symlink; elif [ -f "$D/sgtts2.o" ]; then placed=file; else placed=absent; fi
echo "run 1 placed sgtts2.o as: $placed"
out=$(run two.ninja packed.o)
rc=$?
set -e
echo "$out" | tail -14
echo "exit $rc"
# A double name is a store-name whose own basename already carries a 32-char
# hash: <hash>-<hash>-<file>. Read for it directly rather than only for the
# failure, because the name can be minted on a run that still succeeds.
if printf '%s' "$out" | grep -Eq '[0-9a-z]{32}-[0-9a-z]{32}-'; then
  echo "VERDICT    REPRODUCED - a name was minted over a store basename"
elif [ "$rc" = 0 ]; then
  echo "VERDICT    no double-named add, and the build completed"
else
  echo "VERDICT    failed for some other reason; read the output above"
fi
