# THE INNER HALF OF arm 15, kept as its own file so the nix expression can
# read it verbatim. Embedding it in a nix string would need three layers of
# escaping over a script that itself carries heredocs, and every `$` would
# be a chance to measure the wrong thing silently.
#
# `set +e` IS LOAD-BEARING AND WAS LEARNED HERE. stdenv runs a buildPhase
# under `set -e`, so the first nonzero command kills the phase with NO
# OUTPUT AT ALL - the first run of this arm printed nothing past
# "Running phase: buildPhase" while having actually built its ninja edge.
# An emission that fails is this arm's subject, so a failing command must
# be READ rather than fatal. Diagnostics also have to leave the sandbox:
# they were written to files under $NIX_BUILD_TOP that nothing copied out,
# which is this project's "an OUTPUT crosses the sandbox and nothing else
# does" arriving in its own gate.
set +e
set -uo pipefail

# THE CONFIGURATION IS ASSERTED, NEVER ASSUMED. `persistence_enabled` is
# `!in_nix_build`, keyed on NIX_BUILD_TOP (resolve_cache.rs:160). If that is
# empty the stamps file IS written and this is arm 14 wearing B's name.
[ -n "${NIX_BUILD_TOP:-}" ] || { echo "FAIL: NIX_BUILD_TOP empty, not configuration B"; exit 1; }
[ -n "${NIX_REMOTE:-}" ]    || { echo "FAIL: NIX_REMOTE empty, so no daemon"; exit 1; }

# TWO TARGETS, AND THE SECOND ONE IS WHY. The obvious shape - emit, build
# THAT target, emit it again - cannot be used: `-t drv` for a target whose
# output is already in the directory ABORTS, in both configurations and with
# a different message in each (A: "parsing drv aterm: expected Derive( at
# position 0"; B: "not in uploaded_drvs cache"). That is its own finding and
# is recorded in CLAUDE.md; here it would have destroyed the subject.
# So the directory is grown by building `grow.o` and the SUBJECT is `main.o`,
# which is never built. This is also nearer the consumer's shape, whose
# varying `build_dir_inputs` are values across DIFFERENT tasks of one package.
mkdir -p src build
printf 'int main(void){return 0;}\n' > src/main.c
printf 'int grow(void){return 1;}\n' > src/grow.c
cat > build/build.ninja <<'NJ'
rule cc
  command = gcc -c $in -o $out
build main.o: cc ../src/main.c
build grow.o: cc ../src/grow.c
default main.o
NJ
cd build
export NIX_NINJA_TASK="$(command -v nix-ninja-task)"
ERR="$NIX_BUILD_TOP/emit.err"
emit(){ nix-ninja -t drv main.o 2>>"$ERR"; }

A=$(emit)
B=$(emit)
nix-ninja grow.o >"$NIX_BUILD_TOP/build.err" 2>&1
brc=$?
C=$(emit)
if [ $brc -ne 0 ]; then
  # A FAILED BUILD MEANS THE PRIMARY COMPARISON NEVER HAPPENED. Reporting
  # "stable" off two emissions with no build between them would be arm 15
  # silently degrading into its own control.
  echo "FAIL: the in-sandbox build failed (rc=$brc), so the directory never grew"
  echo "  and the primary comparison has no subject. Its output:"
  tail -30 "$NIX_BUILD_TOP/build.err" | sed 's/^/  /'
  exit 1
fi

# TWO EMPTY EMISSIONS COMPARE EQUAL AND READ AS STABILITY. Arm 14 shipped
# that defect once, so the guard travels with the idiom.
for v in A B C; do
  eval "val=\$$v"
  [ -n "$val" ] || { echo "FAIL: emission $v was empty, so this arm asserts nothing"; tail -20 "$ERR"; exit 1; }
done

h(){ printf '%s' "$1" | sha256sum | cut -c1-16; }
mkdir -p "$out"
{
  echo "NIX_BUILD_TOP  $NIX_BUILD_TOP"
  echo "NIX_REMOTE     $NIX_REMOTE"
  echo "driver build   rc=$brc"
  echo "control  1     $(h "$A")"
  echo "control  2     $(h "$B")"
  echo "after build    $(h "$C")"
  if [ "$(h "$A")" = "$(h "$B")" ]; then echo "CONTROL STABLE"; else echo "CONTROL UNSTABLE"; fi
  if [ "$(h "$B")" = "$(h "$C")" ]; then echo "PRIMARY STABLE"; else echo "PRIMARY UNSTABLE"; fi
  echo "build_dir_inputs values seen:"
  grep -o 'build_dir_inputs=[0-9]*' "$ERR" | sort | uniq -c | sed 's/^/  /'
  echo "objects in build dir after the build:"
  ls -1 ./*.o 2>/dev/null | sed 's/^/  /' || echo "  none"
} > "$out/verdict"
cat "$out/verdict"
