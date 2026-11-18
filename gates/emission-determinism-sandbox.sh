#!/usr/bin/env bash
# DOES ONE UNCHANGED TARGET EMIT ONE DERIVATION ON THE ROUTE THE CONSUMER
# BUILDS? Arm 14's question asked in CONFIGURATION B, and the split between
# the two configurations is the whole reason this arm exists.
#
# Arm 14 measures A, where `persistence_enabled` is true and the driver
# writes `.nix-ninja-nar-stamps.v1` into the build directory, moving every
# task's key. THAT MECHANISM IS GATED OFF HERE: `persistence_enabled` is
# `!in_nix_build` (resolve_cache.rs:160), keyed on NIX_BUILD_TOP, which a
# derivation sets. A session read arm 14's instability as the explanation
# for the consumer's in-sandbox counts and was wrong, because no
# reproduction covered B. A verification is a claim about the configuration
# it was taken on.
#
# THE MECHANISM THIS ARM IS FOR NEEDS NO CACHE AT ALL. The implicit-input
# blanket sweeps the build directory, and under a real build that directory
# GROWS - so a task emitted after some output exists carries inputs a task
# emitted before it did not. The consumer measured distinct
# `build_dir_inputs` values per package, 61 packages reporting more than one
# (fmt 26/30/123/129/187/194, libcbor 68/287/432, graphite2 51/153/266).
# So the PRIMARY comparison builds between the two emissions. Two emissions
# with nothing between them is the CONTROL; on its own it would assert only
# what resolve_cache.rs:160 already states in one line, which is
# current-correct behaviour and cannot cover an open class.
#
# WRITTEN BEFORE THE FIRST RUN so the result cannot be fitted to:
#   control  expect IDENTICAL - the stamps file is not written here and
#            nothing else in the directory moves.
#   primary  expect to DIFFER, if the growth mechanism reaches a nine-line
#            reduction. IF IT DOES NOT, that is a fact about this REDUCTION
#            and never a finding that class 27 does not reach configuration
#            B. Two of this project's reductions have already passed over
#            the class they were written for.
#
# NO `env -i` INSIDE THE SANDBOX, and that is not style. Scrubbing the
# environment removes NIX_BUILD_TOP, `persistence_enabled` flips back to
# true, and the arm would measure configuration A while reporting B - the
# exact confusion it was built to end. The inner script asserts
# NIX_BUILD_TOP and NIX_REMOTE and prints both into the verdict.
#
# THE VERDICT CROSSES AS `$out`. A task's stdout reaches no stream of the
# caller; this project's libssh gate already reported CONTROL FAILED once by
# asserting on the wrong stream.
#
# INLINE EXPRESSION, not a file under modules/flake/examples/. A tracked
# .nix file would have to be priced against the emission test, and while it
# sat unstaged every `example-*` target would stop resolving. This arm is as
# free as arm 14: `local/` is gitignored, so it is in neither binary's
# fileset and changes nothing the driver emits.
set -uo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
REPO=$ROOT
INNER="$HERE/emission-determinism-sandbox-inner.sh"
[ -r "$INNER" ] || { echo "FAIL: inner script missing at $INNER"; exit 2; }

EXPR="
let
  f = builtins.getFlake (toString $REPO);
  sys = builtins.currentSystem;
  p = f.inputs.nixpkgs.legacyPackages.\${sys};
  task = f.packages.\${sys}.nix-ninja-task;
in p.stdenv.mkDerivation {
  name = \"nn-emission-determinism-b\";
  dontUnpack = true;
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [
    f.packages.\${sys}.nix-ninja
    task
    f.inputs.nix.packages.\${sys}.default
    p.patchelf p.gcc p.coreutils
  ];
  buildPhase = builtins.readFile $INNER;
  installPhase = \"true\";
  dontFixup = true;
}"

log=$(mktemp -t nn-arm15-XXXXXX)
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link \
        --print-out-paths --expr "$EXPR" 2>"$log")
rc=$?
if [ $rc -ne 0 ] || [ -z "$OUT" ]; then
  echo "FAIL: the derivation did not build, so NOTHING WAS MEASURED."
  echo "  This is not a result about emission. Its tail:"
  tail -25 "$log" | sed 's/^/  /'
  exit 2
fi
rm -f "$log"

cat "$OUT/verdict"
echo
ctl=$(grep '^CONTROL' "$OUT/verdict")
pri=$(grep '^PRIMARY' "$OUT/verdict")
[ -n "$ctl" ] && [ -n "$pri" ] || { echo "FAIL: the verdict file carries no comparison line"; exit 2; }

# THE ARM PINS WHAT IT MEASURED, like arm 14 and arm 13. Reporting the
# comparison without asserting it would pass whatever happened, and an arm
# that cannot go red covers nothing. Both expectations were written into
# this file BEFORE the first run and both were met on the first measurement:
#   CONTROL STABLE    the stamps mechanism is gated off in B, as
#                     resolve_cache.rs:160 says it is
#   PRIMARY UNSTABLE  the blanket sweeps a build directory that GREW, so a
#                     task emitted after another target was built carries an
#                     input it did not carry before
# When the blanket stops sweeping the build's own outputs, PRIMARY flips to
# STABLE, this arm goes RED, and the table below is edited on purpose.
# THAT HAPPENED. The blanket now skips a `build_dir_inputs` entry the graph
# declares as an output (`File.input.is_some()`), PRIMARY measured STABLE,
# CONTROL was unmoved, and the expectation below is flipped deliberately
# rather than to make the arm green. The arm's job now inverts: it holds
# the fix in place, and a revert of that filter turns PRIMARY back to
# UNSTABLE and fails here.
# **AND THE BLANKET COUNT DID NOT MOVE, WHICH IS THE TRAP THIS RUN FOUND.**
# Values were 1, 1, 2 before the fix and 1, 1, 2 after it. The filter is at
# the INSERT and not at the walk - deliberately, since `build_dir_inputs`
# has a second reader on the cmdline path - so the map still grows and the
# printed line still reports its LENGTH. So the per-package ascending
# values (fmt 26, 123, 187) are NO LONGER a proxy for this class once the
# fix lands: they will keep ascending while the emission is stable. The
# measurement that means anything after this is emitted-derivation
# stability, which is what this arm takes.
# THE EXPECTATION IS NAMED ONCE. Writing it in the test and again in the
# failure message let the two disagree, and they did: the first mutation
# check of this arm printed "UNEXPECTED: PRIMARY UNSTABLE (expected PRIMARY
# UNSTABLE)" because only the test had been flipped. A gate whose red
# message contradicts itself is worse than no message.
EXPECT_CTL="CONTROL STABLE"
EXPECT_PRI="PRIMARY STABLE"
fail=0
[ "$ctl" = "$EXPECT_CTL" ] || { echo "UNEXPECTED: $ctl (expected $EXPECT_CTL)"; fail=1; }
[ "$pri" = "$EXPECT_PRI" ] || { echo "UNEXPECTED: $pri (expected $EXPECT_PRI)"; fail=1; }
if [ $fail -ne 0 ]; then
  echo
  echo "One of the two recorded behaviours moved. If the blanket now excludes"
  echo "the build's own outputs, that is the class 27 fix landing: update this"
  echo "arm's expectations and update class 27. Do not edit the table to make"
  echo "the arm green without deciding which of those it is."
  exit 1
fi

echo "PASS: emission measured in CONFIGURATION B; both recorded behaviours held."
echo "  $ctl"
echo "  $pri"
echo "  The growth the consumer measured across 61 packages still happens -"
echo "  the walk still sees the built output - but it no longer reaches the"
echo "  emitted derivation, because the blanket skips what the graph declares."
echo "  build_dir_inputs values are therefore NO LONGER a proxy for this"
echo "  class: they keep ascending while emission is stable. The stamps"
echo "  mechanism arm 14 pins in configuration A does not fire here."
echo
echo "READ IT AS A CLAIM ABOUT B AND NOTHING ELSE. Arm 14 is the same"
echo "question in A, and the two arms disagreeing is the finding rather"
echo "than a fault in either."
