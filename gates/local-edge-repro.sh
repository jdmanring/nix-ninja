#!/usr/bin/env bash
# A LOCAL EDGE, on configuration B: qtwebengine's gn generator and the
# nested `ninja -C` over the tree it writes, run by the outer driver in the
# package sandbox instead of as task derivations (crates/nix-ninja/src/
# local_edge.rs).
#
# The fixture is the qtwebengine shape reduced. A `cmake -P
# .../cmake/QtGnGen.cmake` edge writes sub/build.ninja (two compiles, an
# archive, a stamp) and sub/archives.rsp, which it declares; an outer edge
# runs `cmake -E env K=V <drop-in ninja> -j1 -C sub T.stamp`, the drop-in
# the shape of the consumer's nn-ninja-shim.sh plain branch (sets
# NIX_NINJA_MATERIALIZE_ALL, execs NN_NIX_NINJA); an outer link takes the
# inner archive through `@sub/archives.rsp`. The outer driver runs at -j 3.
#
#   PASS   the outer build succeeds and its app runs, the inner driver
#          derived its four tasks itself, it ran at width 3 over the
#          command's -j1, exactly one `ran local edge` line per edge, AND
#          the control (NIX_NINJA_NO_LOCAL_EDGES=1, same tree) fails
#   FAIL   otherwise
#   NO ANSWER (exit 2)  the derivation did not build
#   NN_GATE_SRC=<dir>   take driver and task binary from that tree
#   NN_PROBE_REV=<rev>  or from that revision
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-localedge-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

# The fixture, sourced by the builder. `$S` is the arm's root. It reaches the
# builder as a file (`builtins.path`), so nix interpolates nothing in it.
SETUP=$(cat <<'SH'
arm() {
  S=$1; mkdir -p "$S/cmake" "$S/build"
  printf 'int a(void){return 1;}\n' > "$S/a.c"
  printf 'int b(void){return 2;}\n' > "$S/b.c"
  printf '#include <stdio.h>\nint a(void); int b(void);\nint main(void){ if (a()+b()==3) puts("local edge ok"); return a()+b()-3; }\n' > "$S/main.c"
  cat > "$S/sub.ninja.in" <<'EOF'
rule cc
  command = gcc -c $in -o $out
rule ar
  command = ar rcs $out $in
rule stamp
  command = touch $out
build a.o: cc ../../a.c
build b.o: cc ../../b.c
build libinner.a: ar a.o b.o
build T.stamp: stamp libinner.a
EOF
  cat > "$S/cmake/QtGnGen.cmake" <<'EOF'
configure_file(${SRC}/sub.ninja.in ${BUILD_DIR}/build.ninja COPYONLY)
file(WRITE ${BUILD_DIR}/archives.rsp "sub/libinner.a")
EOF
  cat > "$S/build/build.ninja" <<EOF
rule CUSTOM_COMMAND
  command = \$COMMAND
rule cc
  command = gcc -c \$in -o \$out
rule link
  command = gcc -o \$out \$in @sub/archives.rsp
build sub/archives.rsp: CUSTOM_COMMAND $S/cmake/QtGnGen.cmake
  COMMAND = cd $S/build && $CMAKE -DBUILD_DIR=$S/build/sub -DSRC=$S -P $S/cmake/QtGnGen.cmake
build sub/T.stamp: CUSTOM_COMMAND || sub/archives.rsp
  COMMAND = cd $S/build && $CMAKE -E env NN_FIXTURE=1 $NIX_BUILD_TOP/nn-bin/ninja -j1 -C $S/build/sub T.stamp
  pool = console
build main.o: cc ../main.c
build app: link main.o | sub/archives.rsp sub/T.stamp
EOF
  cd "$S/build"
  export NN_INNER_LOG=$S/inner.log; : > "$NN_INNER_LOG"
  if nix-ninja -j 3 app > "$S/outer.log" 2>&1 && ./app >> "$S/outer.log" 2>&1; then
    echo 0 > "$S/rc"; else echo 1 > "$S/rc"; fi
  cd "$NIX_BUILD_TOP"
}
mkdir -p "$NIX_BUILD_TOP/nn-bin"
printf '#!%s\nNIX_NINJA_MATERIALIZE_ALL=1 "$NN_NIX_NINJA" "$@" 2>>"$NN_INNER_LOG"\n' "$(command -v sh)" > "$NIX_BUILD_TOP/nn-bin/ninja"
chmod +x "$NIX_BUILD_TOP/nn-bin/ninja"
export PATH=$NIX_BUILD_TOP/nn-bin:$PATH
( export NIX_NINJA_NO_LOCAL_EDGES=1; arm "$NIX_BUILD_TOP/control" )
arm "$NIX_BUILD_TOP/subject"
SH
)
SETUP_FILE=$(mktemp -t nn-localedge-setup-XXXXXX); printf '%s\n' "$SETUP" > "$SETUP_FILE"
trap 'rm -f "$SETUP_FILE"; [ -n "${W:-}" ] && git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1; true' EXIT

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-local-edge-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf p.cmake ];
  dontUnpack = true; dontFixup = true; dontUseCmakeConfigure = true;
  setup = builtins.path { path = $SETUP_FILE; name = \"local-edge-setup.sh\"; };
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    export NN_NIX_NINJA=\$(command -v nix-ninja)
    export CMAKE=\$(command -v cmake)
    source \$setup
  '';
  installPhase = ''
    for a in control subject; do mkdir -p \$out/\$a; cp \$NIX_BUILD_TOP/\$a/rc \$NIX_BUILD_TOP/\$a/outer.log \$NIX_BUILD_TOP/\$a/inner.log \$out/\$a/; done
  '';
}"
log=$(mktemp -t nn-localedge-XXXXXX)
echo "building the local-edge fixture on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths --cores "${NN_PROBE_CORES:-3}" --max-jobs 1 --expr "$EXPR" 2>"$log")
if [ $? -ne 0 ] || [ -z "$OUT" ]; then
  echo "NO ANSWER: the derivation did not build, so NOTHING WAS MEASURED. Tail:"; tail -20 "$log" | sed 's/^/  /'; rm -f "$log"; exit 2
fi
rm -f "$log"
S=$OUT/subject; C=$OUT/control
rc=$(cat "$S/rc"); crc=$(cat "$C/rc")
count() { local n; n=$(command grep -c "$1" "$2"); echo "${n:-0}"; }
ran=$(count '^nix-ninja: ran local edge ' "$S/outer.log")
ran_gn=$(count '^nix-ninja: ran local edge sub/archives\.rsp (gn generator)$' "$S/outer.log")
ran_nn=$(count '^nix-ninja: ran local edge sub/T\.stamp (nested ninja)$' "$S/outer.log")
width=$(command grep -o '^nix-ninja: local edge width [0-9]* (-j 1 on the command line)$' "$S/inner.log" | command grep -o 'width [0-9]*')
inner_tasks=$(command grep -o '^nix-ninja-stats {"tasks":[0-9]*' "$S/inner.log" | command grep -o '[0-9]*$')
ok=$(count '^local edge ok$' "$S/outer.log")
cran=$(count '^nix-ninja: ran local edge ' "$C/outer.log")
echo "subject: rc=$rc ran=$ran (gn $ran_gn, nested $ran_nn) inner tasks=${inner_tasks:-none} ${width:-no width line} app=$ok"
echo "control: rc=$crc ran=$cran ($(command grep -m1 -E 'ABORTED|FAILED' "$C/outer.log" | cut -c1-120))"
if [ "$rc" = 0 ] && [ "$ok" = 1 ] && [ "$ran" = 2 ] && [ "$ran_gn" = 1 ] && [ "$ran_nn" = 1 ] \
   && [ "${inner_tasks:-0}" = 4 ] && [ "$width" = "width 3" ] && [ "$crc" = 1 ] && [ "$cran" = 0 ]; then
  echo "PASS: both local edges ran in the build dir, the inner driver derived its four tasks at width 3 over -j1, the app linked the inner archive, and the control failed (configuration B)"; exit 0
fi
echo "FAIL: local edge (configuration B); subject outer log tail:"; tail -12 "$S/outer.log" | sed 's/^/  /'
echo "  inner log tail:"; tail -6 "$S/inner.log" | sed 's/^/  /'
exit 1
