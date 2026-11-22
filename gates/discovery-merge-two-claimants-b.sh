#!/usr/bin/env bash
# ONE BUILD PATH, TWO CLAIMANTS, MEETING IN THE LOCAL-MODE DISCOVERY MERGE.
#
# The A twin (outer-rewrite-two-claimants-repro.sh) cannot reach this site:
# it drives `-t drv`, which emits the STATIC input set and never runs
# discovery, and the `9f79161` pass over that set is not the guard under
# test. The guard is in `update_derivation_with_discoveries`, reached only
# when local mode (B, the consumer's route) discovers a `deps = gcc` edge's
# includes and merges them into an already emitted derivation. `775dc12`
# put the guard there; the unit test beside it feeds the function two
# offers by hand. Nothing before this gate made the two claimants arrive
# through the driver's own pipeline inside a recursive build.
#
# THE TWO CLAIMANTS ARE PRODUCED THE WAY virglrenderer PRODUCED THEM, in
# the order its failing round recorded, which placed the site at the
# local-mode merge. A configure-time `config.h` names
# the outer output, so its declared upload is placeholder-rewritten and
# remembered in `rewritten_uploads`. The compile is LTO, so the `lto_raw`
# arm of `build_task_derivation` swaps that declared input for a RAW
# re-upload (a slim LTO object cannot have a placeholder restored). The
# compile ALSO carries a BUILT input, a header another edge generates, and
# that is the part a first draft of this gate left out: the merge under
# test runs only on the local arm of `handle_derivation_result`, which is
# taken only for a task with a Built input. That arm realises the generated
# header, seeds discovery from it alone (`scan_seeds`), and the generated
# header includes `config.h`, so discovery offers `config.h` AGAIN through
# the placeholder rewrite: one build path, two store paths, identical
# spelling. Before the guard both reach the task and `create_symlinks`
# refuses in sandbox setup; with it the declared claimant is kept, the offer
# dropped, and the driver says so once on stderr.
#
# BOTH HALVES ARE ASSERTED, because each alone is a fake pass. A driver that
# never produced the first claimant builds the object and prints nothing;
# that is why the LTO swap line is required before any verdict. A compile
# with no Built input never enters the merge and also builds; that is why
# the guard's own line must appear, which says a second claimant was
# offered and the merge, not the scanner, resolved it.
#
#   PASS       the object builds, the swap fired, the guard line printed
#              EXACTLY TWICE (the scan offers config.h from the generated
#              seed and from the includer; the merge drops both)
#   FAIL       "Two files claim one build path" (the class), or the object
#              did not build for a reason this gate names
#   NO ANSWER  the derivation did not build, the swap never fired (no first
#              claimant), or the object built with no guard line (no second
#              claimant reached the merge); exit 2, nothing measured
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
#                        (71c5df2, the parent of 775dc12, is the control:
#                        expect FAIL with the class text)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
[ -f "$REPO/Cargo.toml" ] || { echo "NO ANSWER: no Cargo.toml above $0"; exit 2; }
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-merge2-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-discovery-merge-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf ];
  dontUnpack = true; dontConfigure = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    # Discovery must be the only route that can supply the header a second
    # time; the blanket is gated off gcc tasks anyway, but a gate for a
    # discovery mechanism says so rather than relying on that.
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    # One line per discovery seed naming what it offered, so the count of
    # guard lines can be read against the offers rather than guessed at.
    export NIX_NINJA_DEBUG_DISCOVERY=1
    S=\$NIX_BUILD_TOP/src; B=\$S/build; mkdir -p \$B
    # The header names the REAL outer output, so the declared upload is
    # placeholder-rewritten and the LTO swap has something to swap.
    printf '#define FIXTURE_TOOL_PATH \"%s/libexec/fixture\"\n' \"\$out\" > \$B/config.h
    # The generated header is the Built input that routes the compile
    # through the local discovery arm, and it includes config.h so that
    # arm's scan offers config.h a second time.
    cat > \$S/gen.sh <<'GEN'
#!/bin/sh
printf '%s\n' '#include \"config.h\"' '#define GEN 1' > \"\$1\"
GEN
    chmod +x \$S/gen.sh
    printf '#include \"config.h\"\n#include \"gen.h\"\nint fn(void){return GEN;}\n' > \$S/main.c
    {
      printf 'rule gen\n  command = %s/gen.sh gen.h\n' \"\$S\"
      printf 'rule cc\n  command = gcc -flto -I. -c \$in -o \$out\n  deps = gcc\n'
      printf 'build gen.h: gen\n'
      printf 'build main.o: cc ../main.c | config.h gen.h\n'
    } > \$B/build.ninja
    cd \$B
    if nix-ninja main.o > \$NIX_BUILD_TOP/build.log 2>&1; then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
    # stdenv runs this phase under set -e and pipefail, so a grep that
    # matches nothing (the SUCCESS case) must not abort the phase; the
    # first run of this gate died exactly there and measured nothing.
    # The task's own log is NOT readable from in here: the recursive daemon
    # answers 'build log ... is not available' for every derivation the
    # abort names, measured on both. The host reads it after the build, so
    # the derivation names are all this phase carries out.
    grep -oE '/nix/store/[a-z0-9]{32}-ninja-build[^ ]*\.drv' \$NIX_BUILD_TOP/build.log | sort -u > \$NIX_BUILD_TOP/task.drvs || true
  '';
  installPhase = ''
    mkdir -p \$out/probe
    cp \$NIX_BUILD_TOP/verdict \$NIX_BUILD_TOP/build.log \$NIX_BUILD_TOP/task.drvs \$out/probe/
  '';
}"
log=$(mktemp -t nn-merge2-XXXXXX)
echo "driving an LTO compile whose config.h names the outer output, on B;"
echo "a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths \
        --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
if [ $? -ne 0 ] || [ -z "$OUT" ] || [ ! -f "$OUT/probe/build.log" ]; then
  echo "FAIL: the derivation did not build, so NOTHING WAS MEASURED. Tail:"
  tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"
P=$OUT/probe
verdict=$(cat "$P/verdict")
# THE REFUSAL LIVES IN THE TASK DERIVATION'S LOG, read from the host. The
# ABORTED line names the unresolved derivation and the RESOLVED one, and
# only the resolved one was built; every name is asked so neither order
# assumption can hide the text.
TASKLOG=$(mktemp -t nn-merge2-task-XXXXXX)
while read -r drv; do
  [ -n "$drv" ] && nix log "$drv" >> "$TASKLOG" 2>/dev/null || true
done < "$P/task.drvs"

swap=$(command grep -c 'LTO task keeps real outer paths' "$P/build.log"); swap=${swap:-0}
guard=$(command grep -c 'config.h is already claimed by a declared input' "$P/build.log"); guard=${guard:-0}
# BOTH REFUSAL WORDINGS. A header is placed as a COPY since fab689a, so the
# collision the task reports is the copy one ("is already a file, and a
# second input wants it to hold"); the symlink wording ("Two files claim one
# build path") is what an alias or non-header collision prints. The first
# run of this gate matched only the second and read a real collision as an
# unexplained failure.
class=$(cat "$P/build.log" "$TASKLOG" | command grep -cE 'Two files claim one build path|is already a file, and a second input wants'); class=${class:-0}
echo "result   $OUT"
echo "object   $verdict"
echo "swap     $swap   (LTO raw re-upload of the declared config.h; the first claimant)"
echo "guard    $guard   (merge dropped a second offer for config.h)"
echo "class    $class   (task refused two claimants)"

if [ "$swap" -eq 0 ]; then
  echo "NO ANSWER: the LTO swap never fired, so no first claimant existed and the"
  echo "           merge had nothing to guard; the fixture is inert on this tree"
  command grep -m3 -E 'nix-ninja' "$P/build.log" | sed 's/^/  /'
  exit 2
fi
if [ "$class" -gt 0 ]; then
  echo "FAIL: two claimants of config.h reached one task (the class)"
  cat "$P/build.log" "$TASKLOG" | command grep -m1 -E 'Two files claim|is already a file, and a second input wants' | cut -c1-200 | sed 's/^/  /'
  exit 1
fi
# EXACTLY TWO, measured on 48bd866 and pinned: discovery offers config.h
# once from the generated seed and once from the includer. A count of one
# says one route stopped offering, which is a scanner change this gate
# would otherwise wave through; more than two says a new route appeared.
# Either is a finding about the tree, not a pass.
if [ "$verdict" = built ] && [ "$guard" -eq 2 ]; then
  echo "PASS: the second claimant was offered twice and the merge kept one"
  exit 0
fi
if [ "$verdict" = built ] && [ "$guard" -eq 0 ]; then
  echo "NO ANSWER: the object built but the guard line never printed, so no"
  echo "           second claimant reached the merge and the guard was not exercised"
  exit 2
fi
if [ "$verdict" = built ]; then
  echo "FAIL: the object built with the guard line printed $guard time(s), not 2;"
  echo "      the number of routes offering config.h changed, read the discovery lines"
  command grep -n 'discovery ' "$P/build.log" | sed 's/^/  /'
  exit 1
fi
echo "FAIL: the object did not build and the class text is absent; read $P/build.log"
command grep -m2 -E 'ABORTED|error' "$P/build.log" | cut -c1-200 | sed 's/^/  /'
exit 1
