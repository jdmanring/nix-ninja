#!/usr/bin/env bash
# WHICH TOKEN DOES THE DRIVER TREAT AS THE COMMAND'S BINARY?
#
# `task.rs` picks argv[0] with `cmdline.split_whitespace()` and a skip list
# of shell no-ops (`:`, `&&`, `cd <dir>`). Three shapes defeat it, and they
# are separate defects that a single fix may or may not cover:
#
#   A  EMPTY VARIABLE EXPANSION   `command = $cc -MMD -MF ...` where the
#      variable is unset. gyp's ninja generator emits exactly this shape and
#      nss failed 513 tasks on `Failed to find -MMD`. No quotes, no shell
#      operators - just a leading token that is not there.
#   B  LEADING ASSIGNMENT         `PYTHONPATH=../src python3 ...`, valid
#      /bin/sh, which is what ninja runs commands through.
#   C  QUOTED VALUE WITH A SPACE  `-DSHLIB_SUFFIX="so ..."` split inside its
#      quotes (raptor2, 3 tasks).
#
# Each arm prints what the driver said, so a fix can be checked against all
# three rather than against the one it was written for.
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
CORES=${NN_GATE_CORES:-8}
b() { nice -n 10 nix build --cores "$CORES" --extra-experimental-features "$F" \
        --no-link --print-out-paths "$@"; }
px() { b --impure --expr "let f = builtins.getFlake \"$REPO\"; p = f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }

NN=$(b "$REPO#nix-ninja") || { echo "NO ANSWER: the nix-ninja binary did not build for NN"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task") || { echo "NO ANSWER: the nix-ninja-task binary did not build for TASK"; exit 2; }
CC=$(px gcc); BU=$(px binutils); CU=$(px coreutils); BASH=$(px bash); PE=$(px patchelf)
NIXP=$(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" | tail -1)

# EVERY ARM RECORDS, so the verdict below is taken from what ran rather than
# from a second set of invocations that could drift from it.
REC=$(mktemp -t nn-argv0-rec-XXXXXX); trap 'rm -f "$REC"' EXIT
record() { # tag dir
  local rc loss; rc=$(sed 's/rc=//' "$2/rc"); loss=0
  grep -q 'cannot find binary path' "$2/log" && loss=1
  printf '%s %s %s\n' "$1" "$rc" "$loss" >> "$REC"
}

run_shape() {
  local tag=$1 rule=$2 preamble=${3:-}
  local D; D=$(mktemp -d -t "nn-argv0-$tag-XXXXXX")
  mkdir -p "$D/build" "$D/src"
  printf 'int main(void){return 0;}\n' > "$D/src/main.c"
  { [ -n "$preamble" ] && printf '%s\n' "$preamble"
    printf 'rule cc\n  command = %s\n' "$rule"
    printf 'build main.o: cc ../src/main.c\ndefault main.o\n'; } > "$D/build/build.ninja"
  ( cd "$D/build"
    set +e
    env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F" \
      "$NN/bin/nix-ninja" main.o > "$D/log" 2>&1
    echo "rc=$?" > "$D/rc" )
  printf '%-22s %s\n' "$tag" "$(cat "$D/rc")"
  grep -m2 -E "Failed to find|cannot find binary|nix-ninja: (FAILED|ABORTED)" "$D/log" |
    sed 's/^/      /' || echo "      (no resolution error)"
  record "$tag" "$D"
  rm -rf "$D"
}

echo "the three shapes, against $(git -C "$REPO" rev-parse --short HEAD):"
echo
run_shape "A-empty-expansion"  '$unset_cc -MMD -MF $out.d -c $in -o $out'
run_shape "B-leading-assign"   'CCACHE_DIR=/tmp gcc -c $in -o $out'
run_shape "C-quoted-space"     'gcc -DSHLIB_SUFFIX="so lib" -c $in -o $out'
run_shape "E-empty-then-quoted" '$unset_cc -DSHLIB_SUFFIX="so lib" -c $in -o $out'
# THE DISCRIMINATOR between "the parser loses a variable" and "the variable
# is genuinely empty". F defines cc at FILE scope and uses it in the rule,
# which is what gyp actually emits; if F passes, expansion works and nss's
# leading token is empty for some other reason, so a fix that merely tolerates
# an empty argv[0] would be treating a symptom.
run_shape "F-defined-at-file"  '$cc -c $in -o $out'  'cc = gcc'
run_shape "D-control-plain"    'gcc -c $in -o $out'
# ARM G: SUBNINJA SCOPE. gyp splits build statements across a subninja per
# target and keeps `cc = ...` in the root file. A subninja gets a scope that
# INHERITS its parent's bindings, so `$cc` must resolve inside it. If it does
# not, the variable is LOST across the boundary and that - not an empty
# argv[0] to be tolerated - is nss's defect. F cannot see this, because F
# defines the variable in the same file as the command.
run_subninja() {
  local D; D=$(mktemp -d -t "nn-argv0-G-XXXXXX")
  mkdir -p "$D/build" "$D/src"
  printf 'int main(void){return 0;}\n' > "$D/src/main.c"
  { printf 'cc = gcc\n'; printf 'subninja sub.ninja\n'; } > "$D/build/build.ninja"
  { printf 'rule compile\n  command = $cc -c $in -o $out\n'
    printf 'build main.o: compile ../src/main.c\ndefault main.o\n'; } > "$D/build/sub.ninja"
  ( cd "$D/build"
    set +e
    env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F" \
      "$NN/bin/nix-ninja" main.o > "$D/log" 2>&1
    echo "rc=$?" > "$D/rc" )
  printf '%-22s %s\n' "G-subninja-scope" "$(cat "$D/rc")"
  grep -m2 -E "Failed to find|cannot find binary|nix-ninja: (FAILED|ABORTED)" "$D/log" |
    sed 's/^/      /'
  record "G-subninja-scope" "$D"
  rm -rf "$D"
}
run_subninja

# ARM H: `include`, which is CMake's shape. CMake keeps rules in
# `rules.ninja` and INCLUDES it. ninja's `include` shares the parent's scope
# outright - a stronger guarantee than subninja's inherited copy - so a
# variable defined in build.ninja must be visible there.
run_include() {
  local D; D=$(mktemp -d -t "nn-argv0-H-XXXXXX")
  mkdir -p "$D/build" "$D/src"
  printf 'int main(void){return 0;}\n' > "$D/src/main.c"
  { printf 'cc = gcc\n'; printf 'include rules.ninja\n'
    printf 'build main.o: compile ../src/main.c\ndefault main.o\n'; } > "$D/build/build.ninja"
  printf 'rule compile\n  command = $cc -c $in -o $out\n' > "$D/build/rules.ninja"
  ( cd "$D/build"
    set +e
    env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F" \
      "$NN/bin/nix-ninja" main.o > "$D/log" 2>&1
    echo "rc=$?" > "$D/rc" )
  printf '%-22s %s\n' "H-include-scope" "$(cat "$D/rc")"
  grep -m2 -E "Failed to find|cannot find binary|nix-ninja: (FAILED|ABORTED)" "$D/log" |
    sed 's/^/      /'
  record "H-include-scope" "$D"
  rm -rf "$D"
}
run_include

# ARM I: `include` with the BUILD STATEMENT in the included file. H puts the
# build in the ROOT, which is CMake's layout and is why CMake never hit this.
# If I fails, the defect is not about include versus subninja at all - it is
# that an edge cannot see variables defined in a DIFFERENT FILE from the one
# holding its build statement.
run_include_build() {
  local D; D=$(mktemp -d -t "nn-argv0-I-XXXXXX")
  mkdir -p "$D/build" "$D/src"
  printf 'int main(void){return 0;}\n' > "$D/src/main.c"
  { printf 'cc = gcc\n'; printf 'include rules.ninja\n'; printf 'default main.o\n'; } \
    > "$D/build/build.ninja"
  { printf 'rule compile\n  command = $cc -c $in -o $out\n'
    printf 'build main.o: compile ../src/main.c\n'; } > "$D/build/rules.ninja"
  ( cd "$D/build"
    set +e
    env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F" \
      "$NN/bin/nix-ninja" main.o > "$D/log" 2>&1
    echo "rc=$?" > "$D/rc" )
  printf '%-22s %s\n' "I-include-build-in" "$(cat "$D/rc")"
  grep -m2 -E "Failed to find|cannot find binary|nix-ninja: (FAILED|ABORTED)" "$D/log" |
    sed 's/^/      /'
  record "I-include-build-in" "$D"
  rm -rf "$D"
}
run_include_build

# ARMS J AND K: THE REVERSE DIRECTION, which G and I cannot see. They ask
# whether a CHILD's binding reaches the PARENT, and ninja's two keywords
# differ on exactly that: `subninja` gets a copy whose definitions do not
# leak back, `include` shares the parent's scope so they do. A fix that
# hands the child a reference rather than a copy gets J wrong silently, and
# a fix that stops at the forward direction leaves K failing - which is the
# state today, and is why K is a KNOWN GAP rather than an omission.
run_reverse() {
  local kw="$1" name="$2"
  local D; D=$(mktemp -d -t "nn-argv0-rev-XXXXXX")
  mkdir -p "$D/build" "$D/src"
  printf 'int main(void){return 0;}\n' > "$D/src/main.c"
  { printf '%s child.ninja\n' "$kw"
    printf 'build main.o: compile ../src/main.c\ndefault main.o\n'; } > "$D/build/build.ninja"
  { printf 'cc = gcc\n'
    printf 'rule compile\n  command = $cc -c $in -o $out\n'; } > "$D/build/child.ninja"
  ( cd "$D/build"
    set +e
    env -i HOME="$D" PATH="$NN/bin:$CC/bin:$BU/bin:$CU/bin:$BASH/bin:$NIXP/bin:$TASK/bin:$PE/bin" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F" \
      "$NN/bin/nix-ninja" main.o > "$D/log" 2>&1
    echo "rc=$?" > "$D/rc" )
  printf '%-22s %s\n' "$name" "$(cat "$D/rc")"
  grep -m2 -E "Failed to find|cannot find binary|nix-ninja: (FAILED|ABORTED)" "$D/log" |
    sed 's/^/      /'
  record "$name" "$D"
  rm -rf "$D"
}
run_reverse subninja "J-subninja-no-leak"
run_reverse include  "K-include-leak-back"

echo
echo "D is the CONTROL: if it does not succeed, the harness is broken and the"
echo "other three say nothing."
echo

# THIS ARM PINS A GAP RATHER THAN A FIX, which is why the expectations below
# include failures. Class 22 is OPEN: an edge cannot see variables defined in
# a file other than the one holding its build statement, so G and I are
# EXPECTED to lose argv[0] today. When that is fixed they flip, this arm goes
# red, and the expectations have to be changed on purpose. That is the point:
# the parser every build.ninja goes through must not change scope behaviour
# without somebody editing this table.
#
# A gate that only ever asserts current-correct behaviour cannot cover an open
# class, and an open class covered by nothing is how three of these reached a
# round.
expect() { # tag rc-zero? loses-argv0? why
  local tag=$1 want_ok=$2 want_loss=$3 why=$4
  local line rc loss
  line=$(grep -m1 "^$tag " "$REC") || { echo "MISSING  $tag (arm did not record)"; return 1; }
  rc=$(echo "$line" | cut -d' ' -f2); loss=$(echo "$line" | cut -d' ' -f3)
  local ok=1
  [ "$want_ok" = yes ] && [ "$rc" != 0 ] && ok=0
  [ "$want_ok" = no  ] && [ "$rc" = 0 ]  && ok=0
  [ "$loss" != "$want_loss" ] && ok=0
  if [ "$ok" = 1 ]; then printf '  ok    %-22s %s\n' "$tag" "$why"; return 0; fi
  printf '  FAIL  %-22s rc=%s loses_argv0=%s, expected %s/%s - %s\n' \
    "$tag" "$rc" "$loss" "$want_ok" "$want_loss" "$why"; return 1
}

echo "pinned behaviour:"
fails=0
expect D-control-plain     yes 0 "the control builds"                        || fails=$((fails+1))
expect C-quoted-space      yes 0 "a quoted define with a space is not the defect" || fails=$((fails+1))
expect F-defined-at-file   yes 0 "file-scope expansion works in its own file" || fails=$((fails+1))
expect H-include-scope     yes 0 "include with the build statement in the root, which is CMake" || fails=$((fails+1))
expect A-empty-expansion   no  1 "nss: empty expansion, KNOWN GAP"           || fails=$((fails+1))
expect E-empty-then-quoted no  1 "raptor2: the same gap with a quote after it, KNOWN GAP" || fails=$((fails+1))
expect B-leading-assign    yes 0 "a leading shell assignment is stepped over, 66b39a0" || fails=$((fails+1))
expect G-subninja-scope    yes 0 "subninja inherits the parent's bindings, which is nss" || fails=$((fails+1))
expect I-include-build-in  yes 0 "so does include when the build statement moves" || fails=$((fails+1))
expect J-subninja-no-leak  no  1 "a subninja's own binding must NOT reach the parent" || fails=$((fails+1))
expect K-include-leak-back no  1 "an include's binding SHOULD reach the parent and does not, KNOWN GAP" || fails=$((fails+1))

echo
if [ "$fails" = 0 ]; then
  echo "PASS: every shape behaves as recorded, gaps included."
  exit 0
fi
echo "$fails expectation(s) moved. If a KNOWN GAP now builds, class 22 or the"
echo "argv0 picker changed: update this table and CLAUDE.md together."
exit 1
