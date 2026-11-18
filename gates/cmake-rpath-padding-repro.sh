#!/usr/bin/env bash
# CMake reserves space for its install-time RPATH rewrite by padding the
# build-time RPATH with EMPTY entries - a run of trailing colons - sized to
# the install value. `file(RPATH_CHANGE)` edits in place and records the
# padding as part of OLD_RPATH, so a rewrite that drops the empty entries
# removes the whole allocation and the install refuses.
#
# Three arms, so the fix is measured against what it replaces:
#   untouched   the link as CMake left it            install must succeed
#   stripped    empties dropped, store dir appended  install must FAIL
#   preserved   empties kept,    store dir appended  install must succeed
#
# The third arm is also what answers whether an empty entry reaches the
# DELIVERED artifact: it reads the installed file's RUNPATH afterwards.
set -uo pipefail

# BY ATTRIBUTE, NEVER A LITERAL STORE PATH: a literal is collected by the next
# GC and the arm answers NO ANSWER from then on (2026-10-03).
nnpkg() { nix build --no-link --print-out-paths --impure --expr \
  "(builtins.getFlake \"$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)\").inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}.$1" | tail -1; }
CMAKE=${CMAKE:-$(nnpkg cmakeMinimal)/bin/cmake}
PATCHELF=${PATCHELF:-$(nnpkg patchelf)/bin/patchelf}
echo "cmake: $("$CMAKE" --version | head -1)   patchelf: $("$PATCHELF" --version)"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# A store-shaped directory the task binary would append: a sibling task's
# output holding a needed library.
SIBLING=/nix/store/0000000000000000000000000000000a-ninja-build-src-lib-aaa-libaaa.so.1/src/lib
INSTALL_RPATH=/nix/store/0000000000000000000000000000000b-padprobe-1.0/lib

mkdir -p "$WORK/src"
echo 'int a_fn(void){return 1;}'                        > "$WORK/src/a.c"
echo 'int a_fn(void); int b_fn(void){return a_fn();}'   > "$WORK/src/b.c"
cat > "$WORK/CMakeLists.txt" <<EOF
cmake_minimum_required(VERSION 3.20)
project(padprobe C)
add_library(aaa SHARED src/a.c)
add_library(bbb SHARED src/b.c)
target_link_libraries(bbb PRIVATE aaa)
set_target_properties(bbb PROPERTIES INSTALL_RPATH "$INSTALL_RPATH")
install(TARGETS aaa bbb DESTINATION lib)
EOF

"$CMAKE" -S "$WORK" -B "$WORK/build" -DCMAKE_INSTALL_PREFIX="$WORK/inst" >/dev/null 2>&1 \
  || { echo "PROBE BROKEN: configure failed"; exit 2; }
"$CMAKE" --build "$WORK/build" >/dev/null 2>&1 \
  || { echo "PROBE BROKEN: build failed"; exit 2; }

LINKED=$("$PATCHELF" --print-rpath "$WORK/build/libbbb.so")
[ -n "$LINKED" ] || { echo "PROBE BROKEN: linked libbbb.so has no RPATH"; exit 2; }
BARE=${LINKED%%:*}
COLONS=$(( ${#LINKED} - ${#BARE} ))
echo "linked RPATH   ${#LINKED} bytes: ${BARE} + ${COLONS} empty entries"
echo "install value  ${#INSTALL_RPATH} bytes"
if [ "$COLONS" -eq 0 ]; then
  echo "PROBE BROKEN: CMake emitted no padding, so there is nothing to strip"
  exit 2
fi

STRIPPED="${BARE}:${SIBLING}"
PRESERVED="${LINKED}:${SIBLING}"

# Save the pristine build so each arm starts from the same bytes.
cp -a "$WORK/build" "$WORK/build.pristine"

arm() {
  local name=$1 rpath=$2 want=$3 out rc
  rm -rf "$WORK/build" "$WORK/inst"
  cp -a "$WORK/build.pristine" "$WORK/build"
  if [ -n "$rpath" ]; then
    "$PATCHELF" --set-rpath "$rpath" "$WORK/build/libbbb.so" || { echo "PROBE BROKEN: patchelf failed"; exit 2; }
  fi
  out=$("$CMAKE" --install "$WORK/build" 2>&1); rc=$?
  local got=ok
  [ "$rc" -eq 0 ] || got=fail
  printf '%-10s want=%-4s got=%-4s' "$name" "$want" "$got"
  if [ "$got" != "$want" ]; then
    echo "   <- UNEXPECTED"
    echo "$out" | sed 's/^/    /' | head -6
    return 1
  fi
  if [ "$got" = ok ]; then
    echo "   installed RUNPATH: $("$PATCHELF" --print-rpath "$WORK/inst/lib/libbbb.so")"
  else
    echo "   $(echo "$out" | grep -iE 'rpath' | head -1)"
  fi
  return 0
}

rc=0
arm untouched "" ok               || rc=1
arm stripped  "$STRIPPED"  fail   || rc=1
arm preserved "$PRESERVED" ok     || rc=1
echo
[ $rc -eq 0 ] && echo "RESULT: padding is the allocation; preserving it is what lets the install rewrite" \
              || echo "RESULT: an arm did not behave as the mechanism predicts - read it before fixing anything"
exit $rc
