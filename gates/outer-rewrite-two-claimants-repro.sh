#!/usr/bin/env bash
# ONE BUILD PATH MUST NOT REACH A TASK AS BOTH A REWRITTEN AND A RAW UPLOAD.
#
# `new_opaque_file` rewrites a file naming an outer output to a placeholder
# keyed on the OUTPUT NAME (`nix-ninja-outer-output:out` digested over the
# store alphabet) before the upload is hashed, so the upload and every task
# reading it are stable across changes to the outer derivation. The map it
# uses is built from THIS PROCESS's environment, so a driver whose `$out` is
# unset uploads the same file verbatim, and the two uploads are different
# store objects claiming one build path. The task binary then refuses:
# "Two files claim one build path."
#
# Witnessed by the consumer on libxkbcommon 1.13.2 and again on pipewire one
# round later, at pin 37672a4, on `config.h` differing on ONE line:
#   /nix/store/hjbdq8s5j5dkn2a110sy8zlhfcs4lxsx-libxkbcommon-1.13.2/...  placeholder
#   /nix/store/spvlyjiq9w54bf8kp9a6arv0y9fvb1m2-libxkbcommon-1.13.2/...  real $out
# The first hash is the placeholder for output name `out`, NOT a foreign
# derivation's output; a placeholder has no producing derivation, which is why
# a search for its deriver comes back empty and reads as provenance.
#
# WHY THE EXISTING PASS DID NOT MERGE THEM IS THE OPEN QUESTION, and this gate
# exists to answer it rather than to assert a fix. `9f79161` ("one build path,
# one content") IS in that pin: it groups Opaque inputs by raw `PathBuf` and
# re-reads the file on a collision. Two SPELLINGS of one path make two groups
# of size one and the pass never fires. So the gate REPORTS the build-path
# spellings of every claimant beside the count, because the count alone cannot
# separate a grouping-key defect from an ordering one.
#
# Scoring is on what the driver EMITS, never on the exit status: a driver that
# emits one claimant because it uploaded nothing also exits 0, so arm 1 is the
# positive control and must find exactly one claimant carrying the PLACEHOLDER.
#
# Configuration A, seconds, `-t drv` only (no task is built). Needs a
# builder-rpc-v0 daemon.
# A PASS FROM THESE THREE ARMS IS A STATEMENT ABOUT WHICH GEOMETRIES ARE SAFE
# AND NOT ABOUT THE CLASS, which is the reading every arm here is written to
# make impossible to get wrong. Serial drives cannot reproduce it: arm 2 leaves
# the content alone and the stamp cache returns arm 1's upload, arm 3 changes
# it and the driver emits ONE fresh claimant. Two claimants in one task need
# two uploads inside ONE driver process, which is the file being regenerated
# WHILE discovery scans it - the geometry `9f79161`'s own comment names for
# gperf. That arm is not written, and until it is, this gate scores the safe
# cases and prints the real pair's shape for comparison.
#
#   PASS       arm 1 emits exactly one claimant and its BYTES carry the
#              placeholder; arms 2 and 3 emit at most one claimant each
#   FAIL       arm 2 emits two, and the spellings say which defect it is
#   NO ANSWER  arm 1 emits no claimant at all, so the fixture never
#              exercised the rewrite and arm 2 says nothing
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
SRC=${NN_GATE_SRC:-$REPO}
NN=$(b "$SRC#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$SRC#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf); CC=$(px gcc); GR=$(px gnugrep)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

# The placeholder the driver renders for output name `out`. Computed here from
# the same digest the driver uses, so a change to either spelling fails this
# gate rather than passing quietly.
PLACEHOLDER=$(python3 -c '
import hashlib
a = b"0123456789abcdfghijklmnpqrsvwxyz"
d = hashlib.sha256(b"nix-ninja-outer-output:out").digest()
print("".join(chr(a[x % 32]) for x in d[:32]))')

D=$(mktemp -d -t nn-outer2-XXXXXX); S=$D/src; mkdir -p "$S/build"
[ "${KEEP:-}" = 1 ] || trap 'rm -rf "$D"' EXIT

# A plausible outer output path: 32 store-alphabet characters and a name, so
# outer_rewrite_map accepts it. It need not exist; the map only parses it.
FAKE_OUT=/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-fixture-1.0

# config.h names the outer output, which is the alsa-lib and libxkbcommon
# shape: a generated header carrying a path into $out.
cat > "$S/build/config.h" <<EOF
#define FIXTURE_TOOL_PATH "$FAKE_OUT/libexec/fixture"
EOF
printf '#include "config.h"\nint main(void){return 0;}\n' > "$S/main.c"
printf 'rule cc\n  command = %s/bin/gcc -I. -c $in -o $out\n  deps = gcc\nbuild main.o: cc ../main.c | config.h\n' "$CC" > "$S/build/build.ninja"

cd "$S/build"
export PATH="$NN/bin:$TASK/bin:$NIXP/bin:$CC/bin:$CU/bin:$BASH/bin:$PE/bin:$GR/bin"
export NIX_NINJA_TASK=$TASK/bin/nix-ninja-task NIX_CONFIG="experimental-features = $F"
export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0

drv() { # $1 label; emits the -t drv document
  set +e; nix-ninja -t drv main.o > "$D/$1.json" 2> "$D/$1.err"; rc=$?; set -e
  [ $rc -eq 0 ] || { echo "NO ANSWER: $1 -t drv rc=$rc"; tail -5 "$D/$1.err"; exit 1; }
}

# Every Opaque claimant of build path `config.h`, as `<store path>` lines. The
# encoded form carries the build path, so both spellings of it are visible.
claimants() { command grep -oE '/nix/store/[a-z0-9]{32}-config\.h' "$1" | sort -u; }
# WHETHER A CLAIMANT WAS REWRITTEN IS A PROPERTY OF ITS BYTES, NOT OF THE
# DERIVATION. The placeholder is substituted into the uploaded FILE; the
# emitted document names only the upload's store path, so a first version of
# this gate searched the JSON for the placeholder, found none, and reported the
# fixture inert while the rewrite had fired correctly. Read the object.
rewritten_count() { n=0; for sp in $1; do command grep -q "$PLACEHOLDER" "$sp" 2>/dev/null && n=$((n+1)); done; printf '%s' "$n"; }
spellings() { command grep -oE '[^"]*config\.h' "$1" | sort -u | head -20; }

# ARM 1, POSITIVE CONTROL: with $out exported the upload must be rewritten.
export out=$FAKE_OUT
drv arm1
c1=$(claimants "$D/arm1.json"); n1=$(printf '%s' "$c1" | command grep -c . || true)
r1=$(rewritten_count "$c1")
if [ "$n1" -eq 0 ]; then
  echo "NO ANSWER: arm 1 emitted no config.h claimant, so the rewrite was never exercised"
  spellings "$D/arm1.json"; exit 1
fi
if [ "$r1" -eq 0 ]; then
  echo "NO ANSWER: arm 1 emitted $n1 claimant(s) and none of their BYTES carry the"
  echo "           placeholder $PLACEHOLDER, so the fixture's config.h did not reach"
  echo "           the rewrite and arm 2 says nothing"
  for sp in $c1; do echo "  $sp"; command grep -m1 FIXTURE_TOOL_PATH "$sp" 2>/dev/null; done
  exit 1
fi

# ARM 2, THE CLASS: a second driver process over the SAME directory with $out
# UNSET uploads config.h verbatim. If anything from the first run survives into
# this task's input set, one build path reaches it under two store paths.
unset out
drv arm2
c2=$(claimants "$D/arm2.json"); n2=$(printf '%s' "$c2" | command grep -c . || true)
r2=$(rewritten_count "$c2")

echo "arm 1: $n1 claimant(s), $r1 rewritten"
for sp in $c1; do echo "  $sp"; command grep -m1 FIXTURE_TOOL_PATH "$sp" 2>/dev/null; done
echo "arm 2: $n2 claimant(s), $r2 rewritten"
for sp in $c2; do echo "  $sp"; command grep -m1 FIXTURE_TOOL_PATH "$sp" 2>/dev/null; done
echo "arm 2 build-path spellings:"
spellings "$D/arm2.json"

# ARM 3, THE REAL GEOMETRY, taken from the pair the consumer captured:
# virglrenderer's own config.h at TWO CONTENTS, one naming the running build's
# $out (rewritten to the placeholder) and one naming a DIFFERENT real store
# path of the same package (left verbatim). Arm 2 cannot produce that, because
# with the content unchanged the stamp cache returns arm 1's upload and no
# second claimant exists; a PASS from arms 1 and 2 alone is therefore a
# statement about the cache and not about this class.
export out=$FAKE_OUT
OTHER=/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-fixture-1.0
cat > config.h <<EOF
#define FIXTURE_TOOL_PATH "$OTHER/libexec/fixture"
EOF
drv arm3
c3=$(claimants "$D/arm3.json"); n3=$(printf '%s' "$c3" | command grep -c . || true)
r3=$(rewritten_count "$c3")
echo "arm 3: $n3 claimant(s), $r3 rewritten"
for sp in $c3; do echo "  $sp"; command grep -m1 FIXTURE_TOOL_PATH "$sp" 2>/dev/null; done
echo "arm 3 build-path spellings:"
spellings "$D/arm3.json"

if [ "$n1" -eq 1 ] && [ "$n2" -le 1 ] && [ "$n3" -le 1 ]; then
  echo "PASS: one build path reaches the task under one store path in all three arms"
  exit 0
fi
echo "FAIL: config.h reaches one task under $n2 store paths (arm 1 emitted $n1)"
echo "      Compare the spellings above: two spellings of one path means the"
echo "      grouping key in the one-build-path-one-content pass, one spelling"
echo "      means the pass was reached with a single claimant and the second"
echo "      was added after it ran."
exit 1
