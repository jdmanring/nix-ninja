#!/usr/bin/env bash
# THE SIXTH SHAPE: an edge that declares an output INSIDE the outer output.
#
# The pricer's five shapes cannot answer for this class. None of them names
# an outer output at all, so a FREE verdict there means no OTHER shape was
# re-keyed and says nothing about this one. The recorded method is this
# file: drive the class's own subject edge through `-t drv` with the driver
# of each revision and compare the emitted documents.
#
# SCORED ON THE DECLARED OUTPUT'S SPELLING in NIX_NINJA_OUTPUTS, not on the
# byte count. A change that moved nothing but a path length would move the
# bytes and answer a different question.
#
# THE CONTROL IS AN ORDINARY OUTPUT IN THE SAME RUN. Both revisions must
# emit it identically, or a difference on the subject says nothing about
# this class and everything about the two drivers differing in general.
#
#   emission-price-outer-write.sh [BASE_REV]     default b40abd0
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
BASE=${1:-b40abd0}
say() { printf '%s\n' "$*"; }
die() { say "NO ANSWER: $*"; say "  load $(cut -d' ' -f1-3 /proc/loadavg), $(nproc) threads"; exit 2; }
D=$(mktemp -d -t nn-price6-XXXXXX) || die "no scratch directory"
nb() {
  nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" \
    2>>"$D/nix-build.err" | command grep -E '^/nix/store/' || true
}
BT=$(mktemp -d -t nn-price6-wt-XXXXXX)
cleanup() {
  git -C "$REPO" worktree remove --force "$BT" >/dev/null 2>&1
  git -C "$REPO" worktree prune >/dev/null 2>&1
}
trap cleanup EXIT
git -C "$REPO" worktree add -q --detach "$BT" "$BASE" || die "no worktree at $BASE"
say "base $BASE, head working tree"
DRV_BASE=$(nb "$BT#nix-ninja" | tail -1); [ -n "$DRV_BASE" ] || die "the base driver did not build"
DRV_HEAD=$(nb "$REPO#nix-ninja" | tail -1); [ -n "$DRV_HEAD" ] || die "the head driver did not build"
TASK=$(nb "$REPO#nix-ninja-task" | tail -1); [ -n "$TASK" ] || die "the task binary did not build"

# THE EMITTED PATH IS BUILT FROM STORE TOOLS, not from the host's. The driver
# resolves the command's program and refuses a path outside the store, so a
# host PATH makes every emission fail with "bash resolved to a path outside
# the Nix store" and the arm reports NO ANSWER for the control. Same
# construction the five-shape pricer uses.
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

# THE OUTER OUTPUT IS A REAL STORE PATH ON DISK, because both the guard and
# the staging read it as one: a path that does not parse is refused earlier,
# by a different rule, and the arm would report the wrong reason.
OUTER=$TASK

# ONE EDGE PER FIXTURE. Both in one graph made the control unmeasurable: the
# base driver refuses the subject edge while RESOLVING, so a run asking for
# the ordinary output died on an edge it was not asked about and the arm
# reported NO ANSWER for the control. A fixture that cannot answer for the
# control cannot answer for anything.
mk() { # mk DIR subject|control
  mkdir -p "$1/build"
  printf 'rule STAGE\n  command = $COMMAND\n' > "$1/build/build.ninja"
  if [ "$2" = subject ]; then
    printf 'build %s: STAGE\n  COMMAND = printf x > %s\n' \
      "$OUTER/private/nss/basicutil.h" "$OUTER/private/nss/basicutil.h" >> "$1/build/build.ninja"
  else
    printf 'build gen/ordinary.h: STAGE\n  COMMAND = printf x > gen/ordinary.h\n' >> "$1/build/build.ninja"
  fi
}

emit() { # emit DRIVER DIR TARGET
  ( cd "$2/build" && env -i HOME="$D" PATH="$1/bin:$GATE_PATH" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F" \
      NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
      outputs=out out="$OUTER" \
      "$1/bin/nix-ninja" -t drv "$3" 2>>"$D/emit.err" )
}

mk "$D/cbase" control; mk "$D/chead" control
mk "$D/base" subject; mk "$D/head" subject
CB=$(emit "$DRV_BASE" "$D/cbase" gen/ordinary.h); rcb=$?
CH=$(emit "$DRV_HEAD" "$D/chead" gen/ordinary.h); rch=$?
[ $rcb -eq 0 ] && [ $rch -eq 0 ] || die "the control edge did not emit under both drivers"
if [ "$CB" != "$CH" ]; then
  say "CONTROL: the ordinary output MOVED, so the subject says nothing about this class"
  exit 2
fi
say "control  identical  (an ordinary relative output, ${#CH} bytes)"

SB=$(emit "$DRV_BASE" "$D/base" "$OUTER/private/nss/basicutil.h"); rsb=$?
SH=$(emit "$DRV_HEAD" "$D/head" "$OUTER/private/nss/basicutil.h"); rsh=$?

spell() { printf '%s' "$1" | tr ',' '\n' | command grep -o '"NIX_NINJA_OUTPUTS":"[^"]*"' | head -1; }

# AN EMPTY DOCUMENT IS NOT AN ANSWER, and this arm reported one as a verdict
# before the check existed. `-t drv` writes nothing to stdout when the run it
# is part of aborts, and it can still exit 0, so "the head emits" was printed
# over zero bytes with the spelling it claims to score on left blank. That is
# the open `-t drv` class, the same one that makes the llvm gate read its
# verdict from the build rather than from this subtool.
if [ -z "$SH" ] || [ -z "$(spell "$SH")" ]; then
  say "subject  head emitted no document, or none carrying NIX_NINJA_OUTPUTS"
  say ""
  say "NO ANSWER: this shape cannot be priced through -t drv. The subtool"
  say "writes nothing on stdout for an edge whose run aborts, and an empty"
  say "document scores as identical against anything. Take the spelling from"
  say "the derivation a BUILD produces instead: outer-output-write-b.sh at"
  say "each revision, reading NIX_NINJA_OUTPUTS off the task derivation."
  exit 2
fi
if [ $rsb -ne 0 ] && [ $rsh -eq 0 ]; then
  say "subject  base REFUSED, head emits"
  say "  head:  $(spell "$SH")"
  say ""
  say "THE SHAPE DID NOT EXIST AT THE BASE. An edge of this shape could not be"
  say "built there, so it has no banked output and nothing is spent by the"
  say "spelling changing. That is the sixth-shape answer for this class."
  exit 0
fi
if [ $rsb -eq 0 ] && [ $rsh -eq 0 ]; then
  if [ "$SB" = "$SH" ]; then
    say "subject  identical (${#SH} bytes)"; exit 0
  fi
  say "subject  MOVED"
  say "  base:  $(spell "$SB")"
  say "  head:  $(spell "$SH")"
  exit 1
fi
die "the subject emitted under neither driver, or only under the base"
