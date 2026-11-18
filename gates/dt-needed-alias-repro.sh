#!/usr/bin/env bash
# A STAGED SHARED LIBRARY'S OWN DT_NEEDED IS NOT AN INPUT.
#
# liblapack 3.12.1, witnessed by the consumer at pin 463b972
# (nix log /nix/store/cjl3iw1vi9md2hp26jbyvqfgn0473fxl-ninja-build.drv):
# the link names lib/liblapacke.so.3.12.0 and passes
# -Wl,-rpath-link,<build>/lib. liblapacke carries `DT_NEEDED liblapack.so.3`,
# a build-tree soname alias that NO ARGUMENT NAMES, so nothing puts it in the
# task's input set and every LAPACK symbol comes out undefined.
#
# The shape reproduced here is that one, three edges deep:
#   libinner.so.1.0.0   built with -Wl,-soname,libinner.so.1
#   libinner.so.1       alias -> libinner.so.1.0.0   (configure-time link)
#   libouter.so.1.0.0   links -linner, so it records DT_NEEDED libinner.so.1
#   prog                links lib/libouter.so.1.0.0 by PATH, plus -rpath-link
# The final link names libouter and never names libinner. The linker follows
# libouter's DT_NEEDED, looks for libinner.so.1 along -rpath-link, and in the
# sandbox finds nothing.
#
# WHY THE ALIAS MATTERS AND NOT JUST THE LIBRARY: the walk already carries
# configure-time soname links through NIX_NINJA_ALIASES. What is missing is
# reading each shared-library input's DT_NEEDED and carrying the aliases that
# satisfy it, WITH the file each alias points at. Carrying the alias alone
# leaves a dangling link.
#
# Blanket OFF, because the implicit-input blanket would sweep the whole build
# directory into the task and supply libinner for the wrong reason. A gate for
# a discovery mechanism must switch off every other route to the file.
#
# Configuration A, seconds.
#   PASS  prog links
#   FAIL  undefined reference, or the linker cannot find libinner.so.1
set -uo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
b(){ nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null; }
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-dtn-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  SRC=$W; echo "driver and task binary from $NN_PROBE_REV"
else SRC=${NN_GATE_SRC:-$REPO}; fi
NN=$(b "$SRC#nix-ninja")       || { echo "FAIL: no driver"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task")|| { echo "FAIL: no task binary"; exit 2; }
px(){ b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
CC=$(px gcc); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
NIXP=""
for p in $(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default"); do
  [ -x "$p/bin/nix" ] && { NIXP=$p; break; }
done
GATE_PATH="$NN/bin:$CC/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin"
for t in nix-ninja gcc sh nix nix-ninja-task patchelf; do
  found=""; IFS=: read -ra dirs <<< "$GATE_PATH"
  for d in "${dirs[@]}"; do [ -x "$d/$t" ] && { found=$d; break; }; done
  [ -n "$found" ] || { echo "FAIL: $t is not on the gate PATH, so this arm would assert nothing"; exit 2; }
done

D=$(mktemp -d -t nn-dtn-XXXXXX); trap 'rm -rf "$D"' EXIT
mkdir -p "$D/build/lib" "$D/src"
printf 'int inner(void){return 41;}\n'                       > "$D/src/inner.c"
printf 'int inner(void); int outer(void){return inner()+1;}\n' > "$D/src/outer.c"
printf 'int outer(void); int main(void){return outer()==42?0:1;}\n' > "$D/src/main.c"

# The two libraries and the soname alias are built OUTSIDE the graph, the way
# a configure step leaves them, so the only edge under test is the final link.
( cd "$D/build" && PATH="$GATE_PATH" \
  gcc -fPIC -shared -Wl,-soname,libinner.so.1 -o lib/libinner.so.1.0.0 ../src/inner.c &&
  ln -s libinner.so.1.0.0 lib/libinner.so.1 &&
  ln -s libinner.so.1     lib/libinner.so &&
  gcc -fPIC -shared -Wl,-soname,libouter.so.1 -o lib/libouter.so.1.0.0 ../src/outer.c \
      -Llib -linner ) >"$D/setup.log" 2>&1 || {
  echo "FAIL: the fixture itself did not build; nothing was measured"; tail -5 "$D/setup.log"; exit 2; }

# PROVE THE FIXTURE HAS THE PROPERTY IT CLAIMS. Without this the gate can pass
# because the library never recorded the dependency at all.
need=$("$PE/bin/patchelf" --print-needed "$D/build/lib/libouter.so.1.0.0" 2>/dev/null | grep -c '^libinner\.so\.1$')
[ "$need" -eq 1 ] || { echo "FAIL: libouter does not record DT_NEEDED libinner.so.1; the fixture is inert"; exit 2; }

cat > "$D/build/build.ninja" <<'EOF'
rule link
  command = gcc -o $out $in -Wl,-rpath-link,lib
build prog: link ../src/main.c lib/libouter.so.1.0.0
default prog
EOF
cd "$D/build"
env -i HOME="$D" PATH="$GATE_PATH" NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
  NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" NIX_CONFIG="experimental-features = $F" \
  "$NN/bin/nix-ninja" prog >"$D/run.log" 2>&1
rc=$?
if [ $rc -eq 0 ] && [ -e "$D/build/prog" ]; then
  echo "PASS: the link resolved the staged library's own DT_NEEDED"
  exit 0
fi
echo "FAIL: the link did not resolve libouter's DT_NEEDED (rc=$rc)"
grep -oE "cannot find -l[^ ]*|libinner\.so\.1[^ ]*|undefined reference to [^ ]*" "$D/run.log" | sort -u | head -5
tail -6 "$D/run.log" | sed 's/^/  /'
exit 1
