#!/usr/bin/env bash
# WHAT DOES THIS EDIT COST? Answers both of the project's re-key tests for a
# change before it lands, in seconds, on CONFIGURATION A.
#
#   fileset test   does nix-ninja-task's drvPath move (re-keys every banked
#                  compile, plain and dynamic)
#   emission test  does the driver emit different bytes for a task shape that
#                  the change was not aimed at (re-keys those shapes)
#
# ONE SHAPE CANNOT ANSWER THE SECOND QUESTION. The corpus is four shapes
# because a change confined to one of them is free and a change touching the
# emitted PATH or environment touches all four; 9c2db59 is the recorded case
# of a driver-only edit that re-keyed every task derivation, and a probe over
# a plain compile alone would have called it free.
#
# THE INSTRUMENT MUST BE ABLE TO SAY NO, and this one has been fooled before:
# two empty emissions once compared IDENTICAL and were reported as a pass.
# Three arms run before any price is printed.
#
#   sensitivity  one driver, the shape against the same shape with one extra
#                flag. MUST DIFFER. A comparator that cannot see a changed
#                command line cannot see a changed emission either.
#   determinism  one driver, the same shape twice. MUST BE IDENTICAL, or the
#                noise floor is above the signal and no price is meaningful.
#   fileset      a scratch copy of the task binary's own fileset with one
#                byte changed. Its drvPath MUST move, or the drvPath reading
#                is measuring nothing (the pricing command has gone blind
#                once already, on uncommitted files).
#
# An empty emission is NO ANSWER, never a pass. Every arm captures status AND
# output, because a tool dying outside its environment prints nothing and the
# caller's question makes the silence look like data.
#
# CONFIGURATION A, and the verdict says so. A driver change whose effect is
# gated on something that does not resolve outside a sandbox reads as free
# here and is not: nm and readelf resolve to /usr/bin on A and are rejected,
# which is exactly how 9c2db59 was invisible until it was measured inside a
# build. A free verdict from this gate is a claim about A alone; the B
# question is emission-determinism-sandbox.sh.
#
# Usage:  emission-price.sh [BASE_REV] [HEAD_REV]
#   BASE_REV defaults to HEAD. HEAD_REV defaults to the WORKING TREE, so
#   `git add` new files first: an untracked file is invisible to the flake
#   evaluation and the two sides would then be the same tree. Naming both
#   revisions prices a landed interval, which is how the gate is calibrated
#   against a change whose cost is already recorded.
set -uo pipefail

F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
# `local/` IS ITS OWN GIT REPOSITORY, so `rev-parse --show-toplevel` from this
# script's directory answers about local/ and not about nix-ninja. The path is
# derived from the script instead, then checked.
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd) || exit 2
[ -f "$REPO/crates/nix-ninja-task/src/main.rs" ] || { echo "NO ANSWER: $REPO is not the nix-ninja tree"; exit 2; }
BASE_REV=${1:-HEAD}
HEAD_REV=${2:-}

say() { printf '%s\n' "$*"; }
# A NO ANSWER CARRIES THE MACHINE IT HAPPENED ON. Two prices here died at a
# control arm because the load average went from 16 to 97 mid-run, and the
# message named the dead arm without saying why it was dead, so the next
# reading was a hunt for a defect in the tree under test. A THRESHOLD BEFORE
# THE RUN WOULD NOT HAVE HELPED and was measured rather than assumed: the run
# that failed started at a load of about 16 and the retake that PASSED started
# at 76, so a pre-flight guard would have blocked the good run and admitted
# the bad one. The spike arrives mid-run or it does not arrive. Recording the
# number at the moment of failure is the half that discriminates.
die() { say "NO ANSWER: $*"; say "  load average now: $(cut -d' ' -f1-3 /proc/loadavg), $(nproc) threads"; exit 2; }

# STDERR IS NOT PART OF THE VALUE, and this is the third place in this file
# where merging it produced a wrong verdict. `nix build` writes its plan and
# progress to stderr, some of it indented, so `2>&1` lets a whitespace-led
# line become the store path and the caller then reports a binary that built
# perfectly well as missing. Streams stay apart and the result is trimmed to
# the last line that actually looks like a store path.
nb() {
  nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" \
    2>>"${D:-${TMPDIR:-/tmp}}/nix-build.err" | command grep -E '^/nix/store/' || true
}

# --- scratch ---------------------------------------------------------------
D=$(mktemp -d -t nn-price-XXXXXX) || die "no scratch directory"
# A worktree holding a modified tracked file can refuse `worktree remove`, and
# the fileset control modifies one on purpose. Discard first, then prune, or a
# failure between the two leaves a stale worktree behind.
drop_tree() {
  [ -n "${1:-}" ] || return 0
  git -C "$1" checkout -- . >/dev/null 2>&1
  git -C "$REPO" worktree remove --force "$1" >/dev/null 2>&1
  git -C "$REPO" worktree prune >/dev/null 2>&1
}
cleanup() {
  drop_tree "${WT:-}"
  drop_tree "${CTL:-}"
  drop_tree "${HT:-}"
  rm -rf "$D"
}
trap cleanup EXIT

# --- the two trees ---------------------------------------------------------
say "base   $BASE_REV ($(git -C "$REPO" rev-parse --short "$BASE_REV" 2>/dev/null || echo '?'))"
if [ -n "$HEAD_REV" ]; then
  say "head   $HEAD_REV ($(git -C "$REPO" rev-parse --short "$HEAD_REV" 2>/dev/null || echo '?'))"
  [ "$(git -C "$REPO" rev-parse "$BASE_REV" 2>/dev/null)" = "$(git -C "$REPO" rev-parse "$HEAD_REV" 2>/dev/null)" ] \
    && die "both sides name the same commit; there is no interval to price"
else
  say "head   working tree at $REPO"
  if [ -z "$(git -C "$REPO" status --porcelain)" ] && [ "$(git -C "$REPO" rev-parse "$BASE_REV")" = "$(git -C "$REPO" rev-parse HEAD)" ]; then
    die "the working tree is clean and equals $BASE_REV; there is no edit to price"
  fi
fi
git -C "$REPO" ls-files --others --exclude-standard -- 'crates' 'vendor-n2' 'Cargo.lock' 'Cargo.toml' | grep -q . \
  && say "WARNING: untracked files under the crates; run 'git add' or they are invisible to the evaluation"

WT=$D/base-tree
git -C "$REPO" worktree add --detach --quiet "$WT" "$BASE_REV" || die "could not check out $BASE_REV"
HEAD_TREE=$REPO
if [ -n "$HEAD_REV" ]; then
  HT=$D/head-tree
  git -C "$REPO" worktree add --detach --quiet "$HT" "$HEAD_REV" || die "could not check out $HEAD_REV"
  HEAD_TREE=$HT
fi

# --- fileset test ----------------------------------------------------------
# STDERR IS NOT PART OF THE VALUE, and merging it here produced a wrong
# verdict on this gate's own second use: `nix eval` prints
# "warning: Git tree is dirty" on a dirty tree, `2>&1` folded that line into
# the drvPath, the two sides then differed as strings while naming the SAME
# derivation, and the run reported GLOBAL RE-KEY. Wrong in the alarming
# direction this time, which is the only reason it was read rather than
# believed. Keep the streams apart and require the value to be exactly one
# store path.
drvpath() {
  local out rc
  out=$(nix eval --impure --max-jobs 0 --raw --extra-experimental-features "$F" \
    "$1#packages.x86_64-linux.nix-ninja-task.drvPath" 2>>"$D/eval.err")
  rc=$?
  [ $rc -eq 0 ] || { printf 'EVAL-FAILED-rc%s' "$rc"; return; }
  printf '%s' "$out"
}
valid_drv() { case $1 in /nix/store/*.drv) [ "${1#*$'\n'}" = "$1" ] ;; *) false ;; esac; }
FS_BASE=$(drvpath "$WT"); FS_HEAD=$(drvpath "$HEAD_TREE")
valid_drv "$FS_BASE" && valid_drv "$FS_HEAD" \
  || die "a task drvPath is not a single store path: [$FS_BASE] / [$FS_HEAD]; $(tail -2 "$D/eval.err" 2>/dev/null)"

# fileset control: one byte inside the allowlist must move the drvPath.
CTL=$D/ctl-tree
git -C "$REPO" worktree add --detach --quiet "$CTL" "$BASE_REV" 2>/dev/null \
  || die "could not create the fileset control tree"
printf '\n// emission-price control byte\n' >> "$CTL/crates/nix-ninja-task/src/main.rs" \
  || die "the task binary's main.rs is not where the control expects it"
FS_CTL=$(drvpath "$CTL")
drop_tree "$CTL"; CTL=""
valid_drv "$FS_CTL" || die "the fileset control drvPath did not evaluate: [$FS_CTL]"
[ "$FS_CTL" = "$FS_BASE" ] && die "the fileset control did NOT move the drvPath; the pricing command is blind"

# --- build both drivers ----------------------------------------------------
DRV_BASE=$(nb "$WT#nix-ninja" | tail -1)
DRV_HEAD=$(nb "$HEAD_TREE#nix-ninja" | tail -1)
TASK=$(nb "$HEAD_TREE#nix-ninja-task" | tail -1)
# NAME WHICH ONE, AND SHOW WHY. `nb` sends every build's stderr to one file and
# returns an empty string on failure, so the first version of this loop died
# printing "a binary did not build: " with nothing after the colon and the
# reason discarded with the scratch directory. That is a NO ANSWER dressed as
# a verdict, in the gate's own reporting rather than in the tree it prices.
for spec in "base driver:$DRV_BASE" "head driver:$DRV_HEAD" "task binary:$TASK"; do
  what=${spec%%:*}; p=${spec#*:}
  [ -d "$p" ] || die "the $what did not build: [$p]
$(tail -12 "$D/nix-build.err" 2>/dev/null)"
done

# --- the environment the drivers run in ------------------------------------
px() { nb --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
CC=$(px gcc); CU=$(px coreutils); SH=$(px bash); PE=$(px patchelf)
NIXP=""
for p in $(nb --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default"); do
  [ -x "$p/bin/nix" ] && { NIXP=$p; break; }
done
GATE_PATH="$CC/bin:$CU/bin:$SH/bin:$NIXP/bin:$TASK/bin:$PE/bin"
for t in gcc sh nix nix-ninja-task patchelf; do
  found=""
  IFS=: read -ra dirs <<< "$GATE_PATH"
  for d in "${dirs[@]}"; do [ -x "$d/$t" ] && { found=1; break; }; done
  [ -n "$found" ] || die "$t is not on the gate PATH; nothing would be measured"
done

# --- the corpus ------------------------------------------------------------
# Four shapes, because the classes of edit this gate exists to catch reach
# them differently: a compile route, the cc-route's `deps = gcc` edge (which
# alone carries the compiler wrapper and skips the implicit-input blanket), a
# link (the only shape reading shared-library inputs) and a generator (the
# only one running a shell command with the text-filter PATH).
mk() {
  local dir=$1
  mkdir -p "$dir/build" "$dir/src"
  printf 'int inner(void){return 7;}\n' > "$dir/src/inner.c"
  printf 'int inner(void);\nint outer(void){return inner();}\n' > "$dir/src/outer.c"
  printf 'int inner(void);\nint main(void){return inner();}\n' > "$dir/src/main.c"
  # A CONFIGURE-TIME SONAME ALIAS, because a corpus with none in it cannot
  # see a change that is gated on them, and "identical" from a fixture the
  # change cannot reach is not a free verdict. The link is made here rather
  # than by an edge for the same reason the real ones are: meson and cmake
  # create them at configure time and no ninja edge produces them.
  # THE ALIASED LIBRARY IS BUILT OUTSIDE THE GRAPH, the way a configure step
  # leaves it, and that is not a shortcut: it is the shape the class has.
  # liblapack's soname aliases and the libraries behind them exist before
  # ninja runs, so the build-directory walk registers them and a task can
  # reach them by name. An edge-produced library is a different case, and
  # two fixtures built that way printed IDENTICAL under a driver with the
  # narrowing deleted, which would have been recorded as the narrowing being
  # unnecessary. Neither failure looked like a failure.
  mkdir -p "$dir/build/lib"
  ( cd "$dir/build" && PATH="$GATE_PATH" \
    && gcc -fPIC -shared -Wl,-soname,libinner.so.1 -o lib/libinner.so.1.0.0 ../src/inner.c \
    && ln -sf libinner.so.1.0.0 lib/libinner.so.1 \
    && ln -sf libinner.so.1 lib/libinner.so \
    && gcc -fPIC -shared -Wl,-soname,libouter.so.1 -o lib/libouter.so.1.0.0 ../src/outer.c \
       -Llib -linner ) >/dev/null 2>&1 || die "the shared-library fixture did not build: $( ( cd "$dir/build" && PATH="$GATE_PATH" gcc -fPIC -shared -Wl,-soname,libouter.so.1 -o lib/libouter.so.1.0.0 ../src/outer.c -Llib -linner ) 2>&1 | head -3)"
  cat > "$dir/build/build.ninja" <<'NINJA'
rule cc
  command = gcc -c $in -o $out
rule ccdep
  command = gcc -MD -MF $out.d -c $in -o $out
  depfile = $out.d
  deps = gcc
rule link
  command = gcc $in -o $out
rule exe
  command = gcc $in -o $out -Wl,-rpath-link,lib
rule gen
  command = sh -c 'printf "#define V 1\n" > $out'
build plain.o: cc ../src/inner.c
build dep.o: ccdep ../src/main.c
build app: exe dep.o lib/libouter.so.1.0.0
build prog: link plain.o dep.o
build gen.h: gen
NINJA
}

emit() { # emit DRIVER DIR TARGET
  local out
  out=$(cd "$2/build" && env -i HOME="$D" PATH="$1/bin:$GATE_PATH" \
    NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
    NIX_CONFIG="experimental-features = $F" \
    NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
    "$1/bin/nix-ninja" -t drv "$3" 2>>"$D/emit.err")
  printf '%s' "$out"
}

# gen.h IS THE DISCRIMINATOR FOR THE SONAME NARROWING, not decoration: a
# generator runs in a tree that now HAS an alias, so a soname fix seeded into
# every task moves it while one gated on a shared-library link does not.
# lib/libfoo.so.1.2.3 is the positive side, the only shape here that may move
# when such a fix lands.
SHAPES="plain.o dep.o prog gen.h app"

# --- arm: sensitivity ------------------------------------------------------
mk "$D/sens"
sed -i 's|command = gcc -c \$in -o \$out|command = gcc -DPRICE_PROBE=1 -c $in -o $out|' "$D/sens/build/build.ninja"
mk "$D/ref"
S_REF=$(emit "$DRV_HEAD" "$D/ref" plain.o)
S_MOD=$(emit "$DRV_HEAD" "$D/sens" plain.o)
[ -n "$S_REF" ] && [ -n "$S_MOD" ] || die "the sensitivity arm emitted nothing; $(tail -2 "$D/emit.err")"
[ "$S_REF" = "$S_MOD" ] && die "the sensitivity arm saw NO difference across a changed command line; the comparator is dead"
ARMS="sensitivity"
say "sensitivity  OK  a changed command line moves the emission"

# --- arm: determinism ------------------------------------------------------
mk "$D/det"
D1=$(emit "$DRV_HEAD" "$D/det" plain.o)
D2=$(emit "$DRV_HEAD" "$D/det" plain.o)
[ -n "$D1" ] && [ -n "$D2" ] || die "the determinism arm emitted nothing"
[ "$D1" = "$D2" ] || die "one driver emitted two different derivations for one shape; the noise floor is above the signal"
ARMS="$ARMS determinism"
say "determinism  OK  one driver emits one derivation for one shape"
ARMS="$ARMS fileset-ctl"
say "fileset ctl  OK  a byte in the allowlist moves the task drvPath"
say ""

# --- the price -------------------------------------------------------------
if [ "$FS_BASE" = "$FS_HEAD" ]; then
  say "FILESET  free      $FS_HEAD"
else
  say "FILESET  RE-KEYS EVERY BANKED COMPILE"
  say "  base $FS_BASE"
  say "  head $FS_HEAD"
fi

moved=0
for s in $SHAPES; do
  mk "$D/b-$s"; mk "$D/h-$s"
  a=$(emit "$DRV_BASE" "$D/b-$s" "$s")
  b=$(emit "$DRV_HEAD" "$D/h-$s" "$s")
  if [ -z "$a" ] || [ -z "$b" ]; then
    say "EMISSION $s: NO ANSWER (an emission was empty)"
    moved=2
    continue
  fi
  if [ "$a" = "$b" ]; then
    say "EMISSION $s: identical (${#a} bytes)"
  else
    say "EMISSION $s: MOVED (${#a} -> ${#b} bytes)"
    diff <(printf '%s' "$a" | tr ',' '\n') <(printf '%s' "$b" | tr ',' '\n') \
      | grep -E '^[<>]' | head -8 | sed 's/^/    /'
    [ $moved -eq 0 ] && moved=1
  fi
done

say ""
# THE ARMS ARE RESTATED HERE BECAUSE THE VERDICT TRAVELS AND THEY DO NOT.
# They print before the prices, so any reader taking the tail of this output
# keeps the conclusion and drops the evidence that the instrument was alive.
# That happened six times in one sitting, once into a published document, and
# telling the reader to stop truncating did not work; putting the arms where
# the verdict is does.
say "arms passed before pricing: ${ARMS:-NONE}"
say "configuration A. A free verdict here is a claim about A: an effect gated"
say "on a tool that resolves only inside a sandbox reads as free and is not."
if [ "$FS_BASE" != "$FS_HEAD" ]; then
  say "VERDICT: GLOBAL RE-KEY. The task binary moved; every banked compile is spent."
  exit 1
fi
case $moved in
  0) say "VERDICT: FREE on every shape in the corpus."; exit 0 ;;
  1) say "VERDICT: RE-KEYS THE SHAPES MARKED MOVED. Price those against the bank before landing."; exit 1 ;;
  *) say "VERDICT: NO ANSWER."
     say "  load average now: $(cut -d' ' -f1-3 /proc/loadavg), $(nproc) threads"
     exit 2 ;;
esac
