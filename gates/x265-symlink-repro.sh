#!/usr/bin/env bash
# x265's failure class, end to end, with its own negative control.
#
# x265 builds its 10- and 12-bit archives in SIBLING directories with make and
# links them in:  ln -s ../build-10bits/libx265.a ./libx265-10.a
# The archive is named only by `-L. -lx265-10`, so it is declared nowhere in
# build.ninja and reaches the sandbox only if the build-directory walk uploads
# it. `read_build_dir` asked the walk ENTRY for its type, and WalkDir does not
# follow links, so a symlink was neither file nor directory, fell through to
# the alias handler, was refused for pointing out of the tree, and vanished.
# The link then failed on `cannot find -lx265-10` with both files on disk.
#
# Reproduced here at one twentieth the size: one archive built in a sibling
# directory, symlinked into the build dir, named only by -l.
#
#   fixed driver   -> builds, links, the binary runs and exits 0
#   pre-fix driver -> ld.bfd: cannot find -lextra-10: No such file or directory
#
# The second half is the point. Without it this passes whether or not the fix
# is present, because a test that cannot fail is not evidence. To run the
# control, replace the symlink arm of `build_dir_disposition` with
# `BuildDirDisposition::Skip`, build with cargo (the derivation's own tests
# refuse the mutation, which is a second control), and re-run.
#
# Needs: a builder-rpc-v0 daemon, and the experimental features on the
# invocation rather than machine-wide. Run from a checkout.
set -euo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
b() { nice -n 10 nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@"; }
NN=$(b "$REPO#nix-ninja"); TASK=$(b "$REPO#nix-ninja-task")
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

D=$(mktemp -d -t nn-x265-XXXXXX); trap 'rm -rf "$D"' EXIT
mkdir -p "$D/build-10bits" "$D/build" "$D/src"
echo 'int extra_symbol(void) { return 1265; }' > "$D/src/extra.cpp"
printf 'extern int extra_symbol(void);\nint main(void){return extra_symbol()==1265?0:1;}\n' > "$D/src/main.cpp"
PATH="$CC/bin:$BU/bin:$CU/bin:$PATH" g++ -c "$D/src/extra.cpp" -o "$D/build-10bits/extra.o"
PATH="$CC/bin:$BU/bin:$CU/bin:$PATH" ar rcs "$D/build-10bits/libextra.a" "$D/build-10bits/extra.o"
ln -s ../build-10bits/libextra.a "$D/build/libextra-10.a"
cat > "$D/build/build.ninja" <<'EOF'
rule cxx
  command = g++ -c $in -o $out
rule link
  command = g++ $in -L. -lextra-10 -o $out
build main.o: cxx ../src/main.cpp
build app: link main.o
default app
EOF
cd "$D/build"
env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
  NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" app
[ -e app ] || { echo "FAIL: no artifact"; exit 1; }
./app || { echo "FAIL: binary did not run clean, so the archive was not linked"; exit 1; }
echo "PASS: linked against a symlinked sibling archive and ran"

# SCOPE, established rather than assumed. This class reaches the NINJA route
# only. `read_build_dir` has one caller, build.rs:121, gated on
# NIX_NINJA_NO_BUILD_DIR_SCAN, which Saxum's scripts/nn-cc-shim.sh sets - and
# that shim writes a build.ninja holding exactly one rule and one edge,
# `build $nn_obj: cc $nn_tu`. A single compile, never a link. So the compiler
# route cannot meet this shape: its links run under make or libtool outside
# nix-ninja, where the symlinked archive is an ordinary file.
#
# Recorded because the opposite was briefly believed here, on the strength of
# a run that forced a link edge through a driver with the walk disabled. That
# configuration the shim cannot produce, so the failure was an artifact of the
# setup rather than a defect. A verdict against a starved configuration is not
# a finding.
