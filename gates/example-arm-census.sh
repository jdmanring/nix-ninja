#!/usr/bin/env bash
# WHICH CODE PATH DOES EACH EXAMPLE ACTUALLY EXERCISE?
#
# `handle_derivation_result` has three arms and the driver counts them
# separately: `sandbox_adddrv_calls` when it builds a DYNAMIC derivation
# inside a sandbox, `dyn_adddrv_calls` when it discovers locally and re-adds
# a PLAIN one, and `plain_adddrv_calls` when there was nothing to discover.
# Nothing said which examples reach which arm, and the cost of not knowing is
# on the record: a session read `dyn_adddrv_calls` as evidence a dynamic
# derivation existed, searched a store corpus for it, got zero, and published
# the dynamic class as BROKEN. Both halves were wrong and both are visible
# here in one table.
#
# NO ANSWER IS NOT ZERO, and this gate exists partly to keep that distinction.
# A stats line appears only in a build that RAN; a cached example prints
# nothing and a discarded log answers nothing. Either reports `no stats` and
# never `0`.
#
#   local/gates/example-arm-census.sh [--build]
#
# Without --build it reads `nix log` for what the store already knows, which
# is seconds. With --build it builds every example first, which is minutes to
# tens of minutes and is what a cold store needs.
set -u
ROOT=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
cd "$ROOT" || exit 2
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
BUILD=0
[ "${1:-}" = "--build" ] && BUILD=1
D=$(mktemp -d -t nn-census-XXXXXX) || exit 2
EXPR=$D/e.nix
trap 'rm -rf "$D"' EXIT

# The overlay route reaches BOTH attribute sets with one spelling. A
# `legacyPackages` entry is `pkgs.<name>.target`, an output of a dynamic
# derivation with no `drvPath` of its own, so asking the flake attribute
# directly answers for four examples and errors for the other thirteen.
# BUILD THE ATTRIBUTE A PERSON BUILDS. An overlay route reaching `pkgs.<name>`
# directly is tempting, because it gives a `drvPath` for the thirteen
# `legacyPackages` entries whose flake attribute is an `outputOf` with none.
# It also instantiates a DIFFERENT derivation than the flake does, so its logs
# do not exist and every row reads `no stats`. That version of this gate
# printed a full table of nothing.

# BOTH ATTRIBUTE SETS, because which one an example sits in decides its
# configuration: `packages` holds the four `mkRecursivePackage` ones, which
# run local mode, and `legacyPackages` holds the DRV-mode ones behind
# `builtins ? outputOf`. Reading one set answers about one configuration.
list_attrs() {
  nix eval --extra-experimental-features "$F" ".#$1.x86_64-linux" \
    --apply builtins.attrNames --json 2>/dev/null \
    | python3 -c "
import json,sys
for n in json.load(sys.stdin):
    if n.startswith('example-'):
        print('$1.x86_64-linux.' + n + '\t' + n)"
}
ATTRS=$(printf '%s\n%s\n' "$(list_attrs packages)" "$(list_attrs legacyPackages)" | grep .)
names=$(printf '%s' "$ATTRS" | cut -f2 | sort -u)
[ -n "$names" ] || { echo "NO ANSWER: no example attributes evaluated"; exit 2; }

sandbox_seen=0; answered=0; total=0; drvbytes_seen=0
printf '%-46s %-9s %8s %8s %8s %8s\n' example arm sandbox dyn plain drvbytes
for n in $names; do
  total=$((total+1))
  attr=$(printf '%s' "$ATTRS" | grep -m1 "	$n\$" | cut -f1)
  line=""
  if [ "$BUILD" = 1 ]; then
    # THE LAST STATS LINE, not the first: an example with a cached configure
    # phase runs the driver twice and only the second is about the target.
    line=$(nix build --no-link -L --extra-experimental-features "$F" \
             ".#$attr" 2>&1 | grep -o 'nix-ninja-stats {[^}]*}' | tail -1)
  fi
  if [ -z "$line" ]; then
    # A CACHED EXAMPLE PRINTS NOTHING, which is not the same as an example
    # whose driver reported no arms. Both would read `0` from a naive
    # counter and only one of them is a fact about the code.
    st=cached; [ "$BUILD" = 1 ] || st="not built"
    printf '%-46s %-9s %8s %8s %8s %8s\n' "$n" "$st" - - - -
    continue
  fi
  answered=$((answered+1))
  read -r sb dy pl db <<EOF
$(printf '%s' "$line" | sed 's/nix-ninja-stats //' | python3 -c '
import json,sys
d=json.load(sys.stdin)
print(d["sandbox_adddrv_calls"], d["dyn_adddrv_calls"], d["plain_adddrv_calls"], d.get("drv_bytes", -1))')
EOF
  arm=plain
  [ "${dy:-0}" != 0 ] && arm=local-dyn
  [ "${sb:-0}" != 0 ] && { arm=sandbox; sandbox_seen=$((sandbox_seen+1)); }
  # THE drv_bytes FIELD IS ASSERTED, because a counter that has only ever
  # read zero is inert: on configuration A the uploaded-drv map never fills,
  # so every A run prints 0 and a regression to a constant zero would pass
  # the whole corpus. The `packages` examples run on B, where in_drv is true
  # and the map fills, so a non-zero reading here is the field's validation
  # number. -1 means the field is absent, which is a pre-`c5ce789` driver.
  [ "${db:-0}" -gt 0 ] && drvbytes_seen=$((drvbytes_seen+1))
  printf '%-46s %-9s %8s %8s %8s %8s\n' "$n" "$arm" "${sb:-?}" "${dy:-?}" "${pl:-?}" "${db:--}"
done

echo
echo "examples: $total, answered: $answered, exercising the sandbox arm: $sandbox_seen, drv_bytes non-zero: $drvbytes_seen"
# THE VERDICT IS ABOUT COVERAGE, not about any example passing, and the exit
# status means one thing only: 1 says the dynamic-derivation class has NO
# witness among the examples that answered. That is worth failing on, because
# the class keys on the DRIVER and is the half a driver-only bump re-keys, so
# a corpus without one cannot say anything about the change that costs most.
# 2 is NO ANSWER, which is a statement about what was built and not about
# coverage, and the two are kept apart because a zero read off an unbuilt
# corpus is the exact mistake this gate exists to prevent.
if [ "$answered" = 0 ]; then
  echo "NO ANSWER: nothing built and reported; re-run with --build"
  exit 2
fi
if [ "$sandbox_seen" = 0 ]; then
  echo "FAIL: no answered example builds a dynamic derivation, so the class that keys on the driver has no witness"
  exit 1
fi
# ONLY WITH --build. The field is populated inside a derivation, so a run that
# read cached logs (or read A) would show zeros that say nothing about the
# counter, and failing on them would be the "zero off an unbuilt corpus"
# mistake this gate already names. With --build the `packages` examples run on
# B and at least one must report a non-zero drv_bytes, or the counter is
# either absent (a pre-`c5ce789` driver) or regressed to a constant.
if [ "$BUILD" = 1 ] && [ "$drvbytes_seen" = 0 ]; then
  echo "FAIL: no answered example reported a non-zero drv_bytes, so the counter is absent or inert"
  exit 1
fi
echo "PASS: the dynamic-derivation arm has $sandbox_seen witness(es); drv_bytes non-zero in $drvbytes_seen"
