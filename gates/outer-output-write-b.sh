#!/usr/bin/env bash
# A PACKAGE THAT DECLARES AN EDGE OUTPUT INSIDE ITS OWN OUTER OUTPUT.
#
# RED BY DESIGN while the class is open. The sibling gate
# `outer-output-header-b.sh` models nss's READ half, a compile carrying
# `-I$out/private/nss`, and passes at `48a72e4`. nss's gyp build also sets
# `-Dnss_dist_dir=$out`, so edges declare OUTPUTS there, and the package
# still fails on B with the driver's own refusal:
#
#   absolute path <out>/private/nss/basicutil.h does not resolve under
#   build dir /build/source/out/Release; refusing to relocate it silently
#
# `normalize_build_path`, from the `outputs.push` in `new_task`. THE
# SPELLING REACHES THE GUARD UNREWRITTEN, which is why this fixture can
# model the class at all: the path comes from the ninja graph node's own
# name, not from a forwarded environment value, so `outer_rewrite_map`
# never sees it. gyp writes the outer output's absolute path into
# build.ninja and this fixture does the same.
#
# THE CONTROL IS IN THE SAME RUN, not at a revision. An edge whose output
# is an ordinary relative build path must BUILD, from the same command and
# the same driver invocation. Without it a broken fixture and a live class
# print the same thing, which is the apparatus result this corpus has hit
# three times in one day.
#
# The subject scores on the REFUSAL TEXT and not on the exit status: any
# failure exits non-zero, and only that sentence says the guard is what
# refused.
#
#   PASS   the control built AND the subject built (the class is closed)
#   FAIL   the control built AND the subject was refused (the class, today)
#   exit 2 anything else, which is a fact about the fixture
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-outerwrite-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-outer-output-write\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf ];
  dontUnpack = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    B=\$NIX_BUILD_TOP/source/out/Release; mkdir -p \$B/gen; cd \$B
    # nss's own layout: the outer output holds the staged header tree, and
    # the edge that writes there names it absolutely, as gyp does.
    mkdir -p \"\$out/private/nss\"
    printf 'rule STAGE\n  command = \$COMMAND\n' > build.ninja
    printf 'build %s: STAGE\n  COMMAND = printf %%s %s > %s\n' \\
      \"\$out/private/nss/basicutil.h\" \"'define BASICUTIL 1'\" \"\$out/private/nss/basicutil.h\" >> build.ninja
    printf 'build gen/basicutil.h: STAGE\n  COMMAND = printf %%s %s > gen/basicutil.h\n' \\
      \"'define BASICUTIL 1'\" >> build.ninja
    if nix-ninja gen/basicutil.h > \$NIX_BUILD_TOP/control.log 2>&1
      then echo built > \$NIX_BUILD_TOP/control; else echo failed > \$NIX_BUILD_TOP/control; fi
    if nix-ninja \"\$out/private/nss/basicutil.h\" > \$NIX_BUILD_TOP/subject.log 2>&1
      then echo built > \$NIX_BUILD_TOP/subject; else echo failed > \$NIX_BUILD_TOP/subject; fi
    # A SECOND PASS OVER THE SAME DIRECTORY, and it asserts that this
    # geometry is SAFE rather than reproducing a defect. The driver copies
    # the staged write back into the outer output, so the file the edge
    # produces is on disk when the run repeats, which is the shape that
    # makes a task adopt its own output as an input. It does not happen:
    # a store path naming an outer output is refused before it can become
    # one, the same guard the alsa-lib install-time class needed.
    # CONTROLLED AT db17dca, the revision before a widening of the
    # self-outputs set written for this: the second pass builds there too,
    # so that widening was inert and was reverted. Read this arm as a
    # standing check on the placement, not as a discriminator.
    if nix-ninja \"\$out/private/nss/basicutil.h\" > \$NIX_BUILD_TOP/subject2.log 2>&1
      then echo built > \$NIX_BUILD_TOP/subject2; else echo failed > \$NIX_BUILD_TOP/subject2; fi
    # A STAGED OUTPUT WHOSE CONTENT NAMES THE OUTER OUTPUT, which the three
    # arms above never produce: they write \`define BASICUTIL 1\`, so the
    # staging copy ran with nothing to restore and this gate passed whether or
    # not the restore worked at all. Measured, not supposed: with only those
    # arms the run reports outer_stage_placed=1 and outer_stage_restored=0.
    # The task is handed a PLACEHOLDER for the outer output, writes it into the
    # file, and the copy has to swap it back; a plain byte copy ships the
    # placeholder and names a store path that does not exist.
    # ITS OWN BUILD DIR AND MANIFEST: appending the edge to the build.ninja
    # the arms above ran is a re-configure between passes, which the later-
    # pass record refuses by design, so the arm died on
    # \`configure.v1 was written for another configure\` and never reached
    # the restore.
    B2=\$NIX_BUILD_TOP/source/out/Restore; mkdir -p \$B2; cd \$B2
    printf 'rule STAGE\n  command = \$COMMAND\n' > build.ninja
    printf 'build %s: STAGE\n  COMMAND = printf %%s %s > %s\n' \\
      \"\$out/private/nss/prefix.h\" \"prefix=\$out/lib\" \"\$out/private/nss/prefix.h\" >> build.ninja
    if nix-ninja \"\$out/private/nss/prefix.h\" > \$NIX_BUILD_TOP/restore.log 2>&1
      then echo built > \$NIX_BUILD_TOP/restore; else echo failed > \$NIX_BUILD_TOP/restore; fi
  '';
  installPhase = ''mkdir -p \$out/probe; cp \$NIX_BUILD_TOP/control \$NIX_BUILD_TOP/subject \$NIX_BUILD_TOP/subject2 \$NIX_BUILD_TOP/restore \$NIX_BUILD_TOP/control.log \$NIX_BUILD_TOP/subject.log \$NIX_BUILD_TOP/subject2.log \$NIX_BUILD_TOP/restore.log \$out/probe/'';
}"
echo "driving an edge that declares its output inside the outer output;"
echo "a driver build from source can take minutes."
log=$(mktemp -t nn-outerwrite-XXXXXX)
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths \
  --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
rc=$?
if [ $rc -ne 0 ] || [ -z "$OUT" ]; then
  echo "NO ANSWER: the derivation did not build, so nothing was measured. Tail:"
  tail -20 "$log" | sed 's/^/  /'; rm -f "$log"; exit 2
fi
rm -f "$log"
control=$(cat "$OUT/probe/control"); subject=$(cat "$OUT/probe/subject")
subject2=$(cat "$OUT/probe/subject2")
echo "control  $control   (an ordinary relative output)"
echo "subject  $subject   (an output inside the outer output)"
echo "second   $subject2   (the same edge again, with the file on disk)"
# THE COUNTER'S ONLY CONTROL, and it is here because this is the only gate
# whose subject edge declares an output inside the outer output. A round
# reporting `outer_stage_placed: 0` is worth nothing until something has shown
# the counter can leave zero, which is the mistake already made twice with
# `.nn-outer`: that grep returned zero whether or not an edge staged an output.
# The stats line is one per run and the subject run's own stderr is kept in the
# probe output, so no extra instrumentation is needed. ABSENT is not ZERO: a
# revision before the counters existed emits no such field, and a control arm
# run with NN_PROBE_REV must not read that as the counter failing.
placed=$(command grep -ho '"outer_stage_placed":[0-9]*' "$OUT/probe/subject.log" 2>/dev/null \
  | tail -1 | sed 's/.*://')
restored=$(command grep -ho '"outer_stage_restored":[0-9]*' "$OUT/probe/subject.log" 2>/dev/null \
  | tail -1 | sed 's/.*://')
if [ -z "$placed" ]; then
  echo "counters absent at this revision (nothing to check)"
elif [ "$placed" = 0 ]; then
  echo "counter  outer_stage_placed=0 on an edge that DID stage an output"
  echo "NO ANSWER: the staging path ran and the counter did not see it, so a"
  echo "  zero from any round is the instrument. Fix the counter, not the round."
  exit 2
else
  echo "counter  outer_stage_placed=$placed outer_stage_restored=${restored:-?} (can leave zero)"
fi

# THE RESTORE ARM, scored on the BYTES THAT LANDED IN THE OUTER OUTPUT rather
# than on the build succeeding: a plain byte copy also succeeds, and ships the
# placeholder. Absent means a revision without the arm, which is not a failure.
if [ -f "$OUT/probe/restore" ]; then
  restore_built=$(cat "$OUT/probe/restore")
  staged=$OUT/private/nss/prefix.h
  echo "restore  $restore_built   (a staged output whose content names the outer output)"
  rstored=$(command grep -ho '"outer_stage_restored":[0-9]*' "$OUT/probe/restore.log" 2>/dev/null \
    | tail -1 | sed 's/.*://')
  if [ "$restore_built" != built ]; then
    echo "FAIL: the staged output naming the outer output did not build"
    tail -5 "$OUT/probe/restore.log" | sed 's/^/  /'; exit 1
  elif [ ! -f "$staged" ]; then
    echo "NO ANSWER: $staged is not in the output, so no bytes were scored"; exit 2
  elif command grep -q "prefix=$OUT/lib" "$staged"; then
    echo "         the real outer path reached \$out, swapped count ${rstored:-?}"
    echo "PASS: an output declared inside the outer output is carried, twice"; exit 0
  else
    echo "FAIL: the staged output carries an unrestored outer path"
    echo "  wanted  prefix=$OUT/lib"
    echo "  got     $(head -c 200 "$staged")"
    exit 1
  fi
fi
if [ "$control" != built ]; then
  echo "NO ANSWER: the control edge did not build, so the subject says nothing"
  tail -5 "$OUT/probe/control.log" | sed 's/^/  /'; exit 2
fi
if [ "$subject" = built ] && [ "$subject2" = built ]; then
  echo "PASS: an output declared inside the outer output is carried, twice"; exit 0
fi
if [ "$subject" = built ] && [ "$subject2" != built ]; then
  echo "FAIL: the first pass carried it and the second did not"
  tail -5 "$OUT/probe/subject2.log" | sed 's/^/  /'
  exit 1
fi
if grep -q 'does not resolve under build dir' "$OUT/probe/subject.log"; then
  echo "FAIL: the driver refuses the declared output (the class, red by design)"
  grep -o 'absolute path [^;]*' "$OUT/probe/subject.log" | head -1 | sed 's/^/  /'
  exit 1
fi
echo "NO ANSWER: the subject failed for some other reason; read $OUT/probe/subject.log"
tail -5 "$OUT/probe/subject.log" | sed 's/^/  /'
exit 2
