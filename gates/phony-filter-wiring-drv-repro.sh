#!/usr/bin/env bash
# THE PHONY FILTER IS CALLED WHERE A COMPILE'S INPUTS ARE COLLECTED.
#
# `compile_filter_drops` decides, per file a phony expansion reaches, whether a
# compile carries it: a generated object or source is dropped (perfetto's
# `.gen.o` world), generated data is kept (libid3tag `genre.dat`, openal-soft
# `default_hrtf.txt`, fixed in `3b2efcc`). Its unit arms pin the function, and
# NONE pins the call: replacing `if compile_filter_drops(` with `if false &&`
# leaves all 670 driver tests green (2026-11-16). This gate reads the emission.
#
# The fixture's translation unit names neither generated file, so the
# `named_by_tu` guard cannot answer and only the shape guard decides.
#
# SECONDS AND NO SANDBOX: `-t drv` resolves rather than builds. Configuration
# A. Blanket off (NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0), so only the phony
# expansion can carry the files.
#
# THE CONTROL IS `3b2efcc^` (`NN_PHONY_CONTROL_REV`), the header-allowlist
# filter: it must drop the `.dat`, or the fixture does not model the class.
#   PASS       head carries gen.dat and not gen.o; the control drops gen.dat
#   FAIL       head drops gen.dat (the class) or carries gen.o (the call is
#              bypassed), or the control already carries gen.dat
#   NO ANSWER  (exit 2) a drive failed or emitted no single derivation
set -u
store_find() {  # NEVER GLOB /nix/store; stream it and stop at the first hit
  find /nix/store -maxdepth 1 -name "$1" ! -name '*.drv' -print 2>/dev/null |
    while IFS= read -r d; do
      [ -e "$d${2:-}" ] && { printf '%s\n' "$d${2:-}"; break; }
    done
}

FEAT='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
b() { nice -n 10 nix build --cores 8 --extra-experimental-features "$FEAT" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CTL_REV=$(git -C "$REPO" rev-parse "${NN_PHONY_CONTROL_REV:-3b2efcc^}") || exit 2
SRC=${NN_GATE_SRC:-$REPO}
NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the driver did not build"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the task binary did not build"; exit 2; }
CTL_NN=$(b "git+file://$REPO?rev=$CTL_REV#nix-ninja") || { echo "NO ANSWER: the control driver did not build"; exit 2; }
CTL_TASK=$(b "git+file://$REPO?rev=$CTL_REV#nix-ninja-task") || { echo "NO ANSWER: the control task binary did not build"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc)
NIXP=$(store_find '*nix-2*' /bin)
NIXP=${NIXP%/bin}
if [ -z "$NIXP" ] || [ ! -x "$NIXP/bin/nix" ]; then
  NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)
fi
for t in "$CU" "$BASH" "$PE" "$CC" "$NIXP"; do
  [ -n "$t" ] || { echo "NO ANSWER: a tool did not resolve"; exit 2; }
done

D=$(mktemp -d -t nn-phonyfilter-XXXXXX)
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
mkdir -p "$D/src" "$D/build"
printf 'int x(void){return 0;}\n' > "$D/src/x.c"
printf 'seed\n' > "$D/src/seed"
cat > "$D/build/build.ninja" <<'NINJA'
rule gen
  command = cp $in $out
rule cc
  command = gcc -MD -MQ $out -MF $out.d -o $out -c $in
  deps = gcc
  depfile = $out.d
build gen.dat: gen ../src/seed
build gen.o: gen ../src/seed
build cmake_object_order_depends_target_x: phony gen.dat gen.o
build x.c.o: cc ../src/x.c || cmake_object_order_depends_target_x
NINJA

# drive <driver> <taskbin> <json out>; status is the driver's
drive() {
  ( cd "$D/build"
    PATH="$1/bin:$2/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin" \
    NIX_NINJA_TASK="$2/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $FEAT" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$1/bin/nix-ninja" -t drv x.c.o > "$3" 2>"$3.err"
  )
}

# read_drv <json>: "D<dat>O<obj>" from the input tokens' build paths
# (`<path>:<build path>:<rel>`); nothing if unparseable
read_drv() {
  python3 - "$1" <<'PY'
import json, sys
try:
    j = json.load(open(sys.argv[1]))
    (e,) = j.values()
    toks = e["env"].get("NIX_NINJA_INPUTS", "").split()
except Exception:
    sys.exit(0)
built = {t.split(":")[1] for t in toks if t.count(":") >= 2}
print(f"D{int('gen.dat' in built)}O{int('gen.o' in built)}")
PY
}

for arm in head ctl; do
  if [ $arm = head ]; then dn=$NN; dt=$TASK; else dn=$CTL_NN; dt=$CTL_TASK; fi
  drive "$dn" "$dt" "$D/$arm.json" || {
    echo "NO ANSWER: the $arm drive exited non-zero"; tail -3 "$D/$arm.json.err"; exit 2; }
  r=$(read_drv "$D/$arm.json")
  [ -n "$r" ] || { echo "NO ANSWER: the $arm drive emitted no single derivation"; exit 2; }
  eval "${arm}_r='$r'"
done

echo "head  $head_r"
echo "ctl   $ctl_r  ($CTL_REV)"
fails=0
[ "$ctl_r" = "D0O0" ] || { echo "FAIL: the control must drop both generated files, or the fixture does not model the class"; fails=1; }
case $head_r in
  D1O0) ;;
  D0*) echo "FAIL: head drops gen.dat, the data a compile reads (the class)"; fails=1 ;;
  *O1) echo "FAIL: head carries gen.o, so the filter is not applied where the compile's inputs are collected"; fails=1 ;;
esac
[ $fails = 0 ] || exit 1
echo "PASS: the phony filter keeps generated data and drops a generated object at the call site"
