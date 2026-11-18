#!/usr/bin/env bash
# REAL NINJA MUST READ A TREE THIS DRIVER BUILT AS UP TO DATE, AND ONLY THEN.
#
# meson-python's editable install asks `ninja -n` (through the consumer's
# shim, which routes `-n` to real ninja) and tests the output for the marker
# `ninja: no work to do.`. Real ninja answers from `.ninja_log` and
# `.ninja_deps`, which this driver never wrote, so every tree it built read as
# dirty and three verbose `test_editable` tests failed (row 24).
#
# Configuration A, a real meson project built the way the consumer's ninja shim
# builds it (`NIX_NINJA_MATERIALIZE_ALL=1`, target `all`), then real ninja
# 1.13.2 asked. Needs a builder-rpc-v0 daemon.
#
#   PASS       head: a fresh build reads clean (the marker, exactly); a touched
#              header reads dirty; a head rebuild reads clean again; the
#              control driver's build reads dirty
#   FAIL       any of those inverted; `-d explain` is printed for the arm
#   NO ANSWER  meson setup or a driver build failed, so nothing was asked
#
# A FAKE PASS would be a marker that prints whatever the tree holds; the
# touched-header arm is what rules that out, and the control arm rules out a
# fixture that real ninja reads clean with no help from this driver.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CTL=${NN_NINJA_STATE_CONTROL_REV:-c538753}
b() { nice -n 10 nix build --cores "${NN_GATE_CORES:-8}" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
SRC=${NN_GATE_SRC:-$REPO}
NN_HEAD=$(b "$SRC#nix-ninja") || exit 2
CTL_FULL=$(git -C "$REPO" rev-parse "$CTL") || { echo "NO ANSWER: $CTL is not a revision"; exit 2; }
NN_CTL=$(b "git+file://$REPO?rev=$CTL_FULL#nix-ninja") || exit 2
TASK=$(b "$SRC#nix-ninja-task") || exit 2
NIXBIN=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1) || exit 2
NINJA=$(b "nixpkgs#ninja^out")/bin/ninja || exit 2
MESON=$(b "nixpkgs#meson^out")/bin/meson || exit 2
CC=$(b "nixpkgs#gcc^out") || exit 2
CU=$(b "nixpkgs#coreutils^out") || exit 2
BASH=$(b "nixpkgs#bash^out") || exit 2
PE=$(b "nixpkgs#patchelf^out") || exit 2
# The cython arm needs cython ON PATH: meson probes for it at setup and the
# task runs it by bare name (`command = cython -M ...`).
CY=$(b "nixpkgs#python3Packages.cython^out") || exit 2
"$NINJA" --version | command grep -q '^1\.13\.' || { echo "NO ANSWER: ninja is $("$NINJA" --version), the formats are 1.13's"; exit 2; }

D=$(mktemp -d -t nn-ninjastate-XXXXXX)
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

project() { # $1 dir
  mkdir -p "$1/inc"
  cat > "$1/meson.build" <<'EOF'
project('nnstate', 'c', 'cython')
# A CYTHON TARGET IS PART OF THE FIXTURE, not decoration. meson's cython rule
# is `depfile = $out.dep` with NO `deps` line, and the driver's depfile gate
# required `deps = gcc` until 2026-10-06, so the depfile was never an output
# of that task and real ninja read the edge dirty FOREVER: measured on
# meson-python's `complex` fixture as `ninja explain: depfile
# '...pyx.c.dep' is missing` on all three cython edges, with `recorded 8/11
# edge(s)`. A fixture with only C compiles has every one of its depfiles
# carried for another reason (`c_COMPILER` does carry `deps = gcc`) and CANNOT
# see that class, which is why this gate was green while meson-python failed.
py = import('python').find_installation()
py.extension_module('greetx', 'greet.pyx')
lib = static_library('greet', 'greet.c', include_directories: 'inc')
executable('hello', 'main.c', include_directories: 'inc', link_with: lib)
EOF
  printf '#define GREETING "hi"\nconst char *greet(void);\n' > "$1/inc/greet.h"
  printf '#include "greet.h"\nconst char *greet(void) { return GREETING; }\n' > "$1/greet.c"
  printf '#include <stdio.h>\n#include "greet.h"\nint main(void) { puts(greet()); return 0; }\n' > "$1/main.c"
  printf '# cython: language_level=3\n' > "$1/greet.pyx"
}

PATHX="$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$CY/bin:$TASK/bin:$NIXBIN/bin"
# THE FLAG IS A PARAMETER BECAUSE THE SHIM DOES NOT ALWAYS SET IT, and this
# gate asserted row 24 closed while only ever building the branch that does.
# `nn-ninja-shim.sh` before a fix made in the consumer's tree set
# NIX_NINJA_MATERIALIZE_ALL on its `test` and `install` branches only; meson-python's editable rebuild takes the PLAIN build branch
# at the bottom of that script, which does not. Measured in the consumer's
# 2026-10-03 round: the flag's own log line appears 28 times, for the packages
# that ran install or check, and ZERO times for meson-python, whose three
# verbose test_editable variants failed on `stdout == ''` at that pin.
drive() { # $1 tree $2 driver $3 log $4 "plain" to build WITHOUT the flag
  if [ "${4:-mat}" = plain ]; then
    ( cd "$1/build" && env PATH="$2/bin:$PATHX" NIX_CONFIG="extra-experimental-features = $F" \
        nix-ninja all ) > "$3" 2>&1
  else
    ( cd "$1/build" && env PATH="$2/bin:$PATHX" NIX_CONFIG="extra-experimental-features = $F" \
        NIX_NINJA_MATERIALIZE_ALL=1 nix-ninja all ) > "$3" 2>&1
  fi
}
ask() { # $1 tree; prints real ninja's -n output
  ( cd "$1/build" && env PATH="$PATHX" "$NINJA" -n 2>&1 )
}
explain() { ( cd "$1/build" && env PATH="$PATHX" "$NINJA" -n -d explain 2>&1 | head -15 ); }
clean() { [ "$1" = "ninja: no work to do." ]; }

setup() { # $1 tree $2 driver $3 "plain" to build without the flag
  project "$1"
  env PATH="$PATHX" NINJA="$2/bin/nix-ninja" "$MESON" setup "$1/build" "$1" > "$1.setup.log" 2>&1 \
    || { echo "NO ANSWER: meson setup failed"; tail -5 "$1.setup.log"; exit 2; }
  drive "$1" "$2" "$1.build.log" "${3:-mat}" \
    || { echo "NO ANSWER: driver build failed"; tail -8 "$1.build.log"; exit 2; }
}

fails=0
setup "$D/head" "$NN_HEAD"
out=$(ask "$D/head")
command grep -m1 'recorded .* edge' "$D/head.build.log"
if clean "$out"; then echo "head fresh:      clean"; else
  echo "head fresh:      DIRTY  ($out)"; explain "$D/head"; fails=$((fails+1)); fi

sleep 1; touch "$D/head/inc/greet.h"
out=$(ask "$D/head")
if clean "$out"; then echo "header touched:  CLEAN (the marker is unconditional)"; fails=$((fails+1)); else
  echo "header touched:  dirty ($(printf '%s' "$out" | wc -l) edge line(s))"; fi

drive "$D/head" "$NN_HEAD" "$D/head.rebuild.log" \
  || { echo "NO ANSWER: head rebuild failed"; tail -8 "$D/head.rebuild.log"; exit 2; }
out=$(ask "$D/head")
if clean "$out"; then echo "head rebuilt:    clean"; else
  echo "head rebuilt:    DIRTY  ($out)"; explain "$D/head"; fails=$((fails+1)); fi

setup "$D/ctl" "$NN_CTL"
out=$(ask "$D/ctl")
if clean "$out"; then echo "control ${CTL:0:7}: CLEAN (the fixture needs no help)"; fails=$((fails+1)); else
  echo "control ${CTL:0:7}: dirty"; fi

# THE PRODUCTION BRANCH, and the arm whose absence let this gate report row 24
# closed while the package failed. Built WITHOUT the flag, exactly as an
# editable rebuild reaches the driver, the intermediates are absent from the
# build directory, so real ninja reports work and NO RECORD CAN MAKE IT CLEAN.
# That is the driver being honest rather than broken: every way `ninja_state`
# can be wrong fails toward "work to do". The arm exists to pin WHICH half is
# covered, so a reader cannot take the clean arms above as a claim about the
# branch meson-python takes. It goes green-side only when the tree is complete,
# which is a consumer decision about the shim and not a driver change.
setup "$D/plain" "$NN_HEAD" plain
out=$(ask "$D/plain")
if clean "$out"; then
  echo "plain branch:    CLEAN - every intermediate was placed without the flag"
  echo "                 (if the shim now sets it, or placement changed, retire this arm)"
  fails=$((fails+1))
else
  echo "plain branch:    dirty, as the consumer's editable rebuild sees it"
fi

if [ "$fails" -eq 0 ]; then
  echo "PASS: real ninja reads a driver-built tree clean, a touched header dirty, the control dirty, and the unmaterialized plain branch dirty"
  exit 0
fi
echo "FAIL: $fails arm(s) inverted"
exit 1
