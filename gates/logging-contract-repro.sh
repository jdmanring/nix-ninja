#!/usr/bin/env bash
# IS STDOUT STILL DATA? The logging contract is written in prose and enforced
# by nothing, across 80 stdout call sites in the two binaries.
#
#   stdout is DATA: `--version`, `-t list`, `-t drv` JSON, and a `-v` build's
#   task output. Everything else is stderr with a `nix-ninja:` prefix.
#
# ONE STRAY WRITE TO STDOUT BREAKS A PARSER, and the parsers are not
# hypothetical: `-t drv` JSON is what the emission pricer diffs and what any
# consumer tooling reads. A diagnostic printed to the wrong stream does not
# fail a build, it corrupts a document, and the corruption reads as a
# malformed derivation rather than as a misplaced message.
#
# THE VERBOSE FLAG IS THE PLACEMENT CONTROL, and without it this gate would
# assert that a quiet build is quiet, which a driver that printed nothing at
# all would also satisfy. The same command with `-v` MUST write task output to
# stdout. One flag apart, so a pass means the arm can tell the two apart
# rather than that the fixture never spoke.
#
# Configuration A, seconds.
#   PASS  -t drv is parseable JSON, a plain build's stdout is empty, and the
#         same build with -v is not
set -uo pipefail
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
SRC=${NN_GATE_SRC:-$REPO}
b(){ nix build --extra-experimental-features "$F" --no-link --print-out-paths "$@" 2>/dev/null | tail -1; }

if [ "${1:-}" = "--control" ]; then
  W=$(mktemp -d -t nn-log-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "${2:?revision}" || { echo "NO ANSWER: cannot check out $2"; exit 2; }
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1; git -C "$REPO" worktree prune >/dev/null 2>&1' EXIT
  SRC=$W
fi

NN=$(b "$SRC#nix-ninja");        [ -x "$NN/bin/nix-ninja" ] || { echo "NO ANSWER: the driver did not build"; exit 2; }
TASK=$(b "$REPO#nix-ninja-task");[ -x "$TASK/bin/nix-ninja-task" ] || { echo "NO ANSWER: the task binary did not build"; exit 2; }
px(){ b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.$1" | tail -1; }
CC=$(px gcc); CU=$(px coreutils); SH=$(px bash); PE=$(px patchelf)
# A MULTI-OUTPUT PACKAGE NEEDS THE OUTPUT THAT CARRIES THE BINARY, and taking
# the last printed path picked jq's `man` output, which made the gate report
# a missing tool that was in the store twice over.
JQ=""
for p in $(b --impure --expr "let f=builtins.getFlake \"$REPO\"; p=f.inputs.nixpkgs.legacyPackages.\${builtins.currentSystem}; in p.jq"); do
  [ -x "$p/bin/jq" ] && { JQ=$p; break; }
done
NIXP=""
for p in $(b --impure --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default"); do
  [ -x "$p/bin/nix" ] && { NIXP=$p; break; }
done
GATE_PATH="$NN/bin:$CC/bin:$CU/bin:$SH/bin:$NIXP/bin:$TASK/bin:$PE/bin:$JQ/bin"
for t in gcc sh nix nix-ninja nix-ninja-task jq; do
  found=""; IFS=: read -ra dirs <<< "$GATE_PATH"
  for d in "${dirs[@]}"; do [ -x "$d/$t" ] && { found=1; break; }; done
  [ -n "$found" ] || { echo "NO ANSWER: $t is not on the gate PATH"; exit 2; }
done

D=$(mktemp -d -t nn-log-XXXXXX)
mkdir -p "$D/build" "$D/src"
printf 'int main(void){return 0;}\n' > "$D/src/main.c"
# THE COMMAND MUST SPEAK, or the verbose arm has nothing to relay and reports
# a driver defect for a fixture that was silent: `gcc -c` prints nothing on a
# clean compile, so the first version of this gate failed itself. The marker
# is the discriminator for both stdout arms, one flag apart.
cat > "$D/build/build.ninja" <<'NINJA'
rule cc
  command = sh -c 'gcc -c $in -o $out && echo NN_TASK_SPOKE'
build main.o: cc ../src/main.c
default main.o
NINJA

run() { # run OUTFILE ERRFILE ARGS...
  local out=$1 err=$2; shift 2
  ( cd "$D/build" && env -i HOME="$D" PATH="$GATE_PATH" \
      NIX_NINJA_TASK="$TASK/bin/nix-ninja-task" \
      NIX_CONFIG="experimental-features = $F" \
      NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
      "$NN/bin/nix-ninja" "$@" ) >"$D/$out" 2>"$D/$err"
}

ok=1
note() { printf '  %s\n' "$1"; }

# --- stdout is a document -------------------------------------------------
run drv.out drv.err -t drv main.o
if [ ! -s "$D/drv.out" ]; then
  echo "NO ANSWER: -t drv wrote nothing, so there is no document to judge"
  tail -3 "$D/drv.err" | sed 's/^/  /'; exit 2
fi
if jq -e . "$D/drv.out" >/dev/null 2>&1; then
  note "-t drv stdout parses as JSON ($(wc -c < "$D/drv.out") bytes)"
else
  ok=0
  note "-t drv stdout is NOT parseable JSON; a diagnostic reached the document"
  command grep -nE '^[a-z-]+(-task)?:' "$D/drv.out" | head -3 | sed 's/^/    /'
fi

# --- a plain build says nothing on stdout ---------------------------------
rm -f "$D/build/main.o"
run q.out q.err main.o
if [ -s "$D/q.out" ]; then
  ok=0
  note "a plain build wrote $(wc -c < "$D/q.out") bytes to stdout; nothing per task belongs there"
  head -3 "$D/q.out" | sed 's/^/    /'
else
  note "a plain build wrote nothing to stdout, and the command it ran does speak"
fi
[ -s "$D/q.err" ] || { ok=0; note "the plain build wrote nothing to STDERR either, so the quiet arm proves nothing"; }

# --- the placement control: -v must speak --------------------------------
rm -f "$D/build/main.o"
run v.out v.err -v main.o
if command grep -q NN_TASK_SPOKE "$D/v.out"; then
  note "-v relayed the task's own output to stdout, so the quiet arm can discriminate"
else
  ok=0
  note "-v did not relay the marker; the quiet arm above cannot tell silence from correctness"
  tail -3 "$D/v.err" | sed 's/^/    /'
fi

# --- --version is one line of data ---------------------------------------
run ver.out ver.err --version
if [ "$(command grep -c . "$D/ver.out")" = 1 ] && ! command grep -q '^nix-ninja:' "$D/ver.out"; then
  note "--version is one unprefixed line"
else
  ok=0
  note "--version stdout is $(command grep -c . "$D/ver.out") line(s) and may carry a log prefix"
fi

if [ $ok = 1 ]; then
  echo "PASS: stdout carries documents only, a plain build is quiet, and -v proves the arm discriminates"
  rm -rf "$D"; exit 0
fi
echo "FAIL: the logging contract is not held; kept $D"
exit 1
