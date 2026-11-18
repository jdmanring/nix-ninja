#!/usr/bin/env bash
# THE MAKE-DIR LIST RIDES passAsFile WHEN, AND ONLY WHEN, A TASK CARRIES ONE.
#
# Class 30 (`731d908`): `NIX_NINJA_MAKE_DIRS` scales with the package, reached
# 132,267 bytes on libLLVMDemangle's archive edge, and execve refused it over
# the fixed MAX_ARG_STRLEN of 131,072 ("Argument list too long"). The fix names
# it in `passAsFile`, and names it CONDITIONALLY, because `passAsFile` is
# emitted into every task derivation and an unconditional name re-keys every
# banked compile (priced both ways in the re-key record). Two halves, and
# nothing else pins either:
#
#   carried   a compile whose -I names an empty build-tree directory emits the
#             list in env AND names it in passAsFile
#   absent    a compile naming no include directory emits neither, so its
#             passAsFile is the pre-fix string byte for byte
#   alias     (own build dir) a configure-time soname link puts the alias list
#             on every task; it rides passAsFile too, and an empty include
#             directory whose path holds a space is carried rather than
#             dropped by the walk (a leftover of the space-joined encoding)
#
# `empty-include-dir-compile-repro.sh` witnesses the task binary READING the
# list from the file, but cannot see a regression to inline transport: the
# reader takes either spelling, so that gate stays green while the 128 KiB
# failure comes back. This one reads the emission.
#
# SECONDS AND NO SANDBOX: `-t drv` resolves rather than builds. Configuration
# A. Walk ON (the empty directory is the walk's), blanket off.
#
# THE CONTROL IS `fc3985e`, the parent of the fix, built by revision
# (`NN_MAKEDIRS_CONTROL_REV`): it must carry the list in env and NOT name it in
# passAsFile, or the fixture does not model the class.
#   PASS       both halves on head, control differs
#   FAIL       a half is wrong on head, or the control already names it
#   NO ANSWER  (exit 2) the fixture produced no make-dir list, or a drive failed
set -u
# NEVER GLOB /nix/store: zsh or bash expanding `/nix/store/*...` reads and
# sorts every entry, and over 650k entries one expansion reached 77 GB RSS
# and stalled a live round (2026-10-17). Stream it and stop at the first hit.
store_find() {  # store_find NAME_PATTERN [SUBPATH]
  find /nix/store -maxdepth 1 -name "$1" ! -name '*.drv' -print 2>/dev/null |
    while IFS= read -r d; do
      [ -e "$d${2:-}" ] && { printf '%s\n' "$d${2:-}"; break; }
    done
}

FEAT='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
b() { nice -n 10 nix build --cores 8 --extra-experimental-features "$FEAT" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

CTL_REV=$(git -C "$REPO" rev-parse "${NN_MAKEDIRS_CONTROL_REV:-fc3985e}") || exit 2
SRC=${NN_GATE_SRC:-$REPO}
NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the driver did not build"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the task binary did not build"; exit 2; }
CTL_NN=$(b "git+file://$REPO?rev=$CTL_REV#nix-ninja") || { echo "NO ANSWER: the control driver did not build"; exit 2; }
CTL_TASK=$(b "git+file://$REPO?rev=$CTL_REV#nix-ninja-task") || { echo "NO ANSWER: the control task binary did not build"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
# Any resident nix satisfies `Tools::new`; see reader-wiring-drv-repro.sh.
NIXP=$(store_find '*nix-2*' /bin)
NIXP=${NIXP%/bin}
if [ -z "$NIXP" ] || [ ! -x "$NIXP/bin/nix" ]; then
  NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)
fi
for t in "$CU" "$BASH" "$PE" "$CC" "$GR" "$NIXP"; do
  [ -n "$t" ] || { echo "NO ANSWER: a tool did not resolve"; exit 2; }
done

D=$(mktemp -d -t nn-makedirs-XXXXXX)
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT
mkdir -p "$D/src/gio" "$D/build/subprojects/gvdb" "$D/build/gio"
printf 'int gio_init(void){return 0;}\n' > "$D/src/gio/ginit.c"
cat > "$D/build/build.ninja" <<'NINJA'
rule c_DIRS
  command = gcc -Isubprojects/gvdb -Werror=missing-include-dirs -MD -MQ $out -MF $out.d -o $out -c $in
  deps = gcc
  depfile = $out.d
rule c_PLAIN
  command = gcc -MD -MQ $out -MF $out.d -o $out -c $in
  deps = gcc
  depfile = $out.d
build gio/dirs.c.o: c_DIRS ../src/gio/ginit.c
build gio/plain.c.o: c_PLAIN ../src/gio/ginit.c
NINJA

# ARM 3, ITS OWN BUILD DIR because a configure-time soname link puts the alias
# list on EVERY task, compiles included, and would contaminate the plain arm.
# It also names an empty include directory whose path holds a space, which the
# walk dropped while the list was space-joined.
mkdir -p "$D/abuild/lib" "$D/abuild/sp ace"
printf 'x' > "$D/abuild/lib/libfoo.so.1.0"
ln -s libfoo.so.1.0 "$D/abuild/lib/libfoo.so.1"
cat > "$D/abuild/build.ninja" <<'NINJA'
rule c_SPACE
  command = gcc -I"sp ace" -Werror=missing-include-dirs -o $out -c $in
build alias.c.o: c_SPACE ../src/gio/ginit.c
NINJA

# drive <driver> <taskbin> <target> <json out>; status is the driver's
drive() {
  ( cd "$D/${5:-build}"
    PATH="$1/bin:$2/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin" \
    NIX_NINJA_TASK="$2/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $FEAT" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$1/bin/nix-ninja" -t drv "$3" > "$4" 2>"$4.err"
  )
}

# read <json>: prints "M<env><paf> A<env><paf> S<space>" as digits, where M is
# NIX_NINJA_MAKE_DIRS, A is NIX_NINJA_ALIASES and S says the make-dir list
# carries the directory named `sp ace`; nothing if unparseable
read_drv() {
  python3 - "$1" <<'PY'
import json, sys
try:
    j = json.load(open(sys.argv[1]))
    (e,) = j.values()
    env = e["env"]
except Exception:
    sys.exit(0)
paf = env.get("passAsFile", "").split()
m, a = "NIX_NINJA_MAKE_DIRS", "NIX_NINJA_ALIASES"
space = int("sp ace" in env.get(m, "").split("\n"))
print(f"M{int(m in env)}{int(m in paf)} A{int(a in env)}{int(a in paf)} S{space}")
PY
}

for arm in head ctl; do
  if [ $arm = head ]; then dn=$NN; dt=$TASK; else dn=$CTL_NN; dt=$CTL_TASK; fi
  for t in dirs plain alias; do
    bd=build; [ $t = alias ] && bd=abuild
    drive "$dn" "$dt" "$( [ $t = alias ] && echo alias.c.o || echo "gio/$t.c.o")" "$D/$arm-$t.json" "$bd" || {
      echo "NO ANSWER: the $arm drive of the $t arm exited non-zero"; tail -3 "$D/$arm-$t.json.err"; exit 2; }
    r=$(read_drv "$D/$arm-$t.json")
    [ -n "$r" ] || { echo "NO ANSWER: the $arm drive of the $t arm emitted no single derivation"; exit 2; }
    eval "${arm}_${t}='$r'"
  done
done

echo "head  dirs=$head_dirs  plain=$head_plain  alias=$head_alias"
echo "ctl   dirs=$ctl_dirs  plain=$ctl_plain  alias=$ctl_alias  ($CTL_REV)"

# Without the lists in env the fixture never reached the class.
for v in "$head_dirs" "$ctl_dirs"; do
  [ "${v:1:1}" = 1 ] || { echo "NO ANSWER: the dirs fixture produced no make-dir list, so it models nothing"; exit 2; }
done
for v in "$head_alias" "$ctl_alias"; do
  [ "${v:5:1}" = 1 ] || { echo "NO ANSWER: the alias fixture produced no alias list, so it models nothing"; exit 2; }
done
fails=0
[ "$head_dirs" = "M11 A00 S0" ] || { echo "FAIL: carried half: a task with make dirs must name NIX_NINJA_MAKE_DIRS in passAsFile"; fails=1; }
[ "$head_plain" = "M00 A00 S0" ] || { echo "FAIL: absent half: a task without the lists must name neither (re-keys every banked compile)"; fails=1; }
[ "$head_alias" = "M11 A11 S1" ] || { echo "FAIL: alias arm: the alias list must ride passAsFile and the spaced directory must be carried"; fails=1; }
[ "$ctl_dirs" = "M10 A00 S0" ] || { echo "FAIL: the control already names the make-dir list in passAsFile, so the fixture does not model the class"; fails=1; }
[ "$ctl_alias" = "M00 A10 S0" ] || { echo "FAIL: the control must drop the spaced directory and carry aliases inline, or the alias arm models nothing"; fails=1; }
[ $fails = 0 ] || exit 1
echo "PASS: the make-dir list rides passAsFile exactly where a task carries one, aliases too"
