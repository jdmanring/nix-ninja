# Runs INSIDE the derivation, on configuration B. Two drivers, one fixture,
# five shapes, whole-derivation compare. The verdict crosses as $out because
# a task's stdout reaches no stream of the caller.
#
# NO `env -i` HERE, and that is not style: scrubbing the environment removes
# NIX_BUILD_TOP, which is what puts the driver on configuration B at all. An
# arm that scrubs measures A while reporting B.
set -uo pipefail
mkdir -p "$out"
: "${NN_BASE:?base driver}" "${NN_HEAD:?head driver}"
echo "NIX_BUILD_TOP=${NIX_BUILD_TOP:-unset}"  > "$out/env"
echo "NIX_REMOTE=${NIX_REMOTE:-unset}"       >> "$out/env"
[ -n "${NIX_BUILD_TOP:-}" ] || { echo "NO ANSWER: not inside a build, so this is not configuration B" > "$out/verdict"; exit 0; }

mk() {
  local dir=$1
  mkdir -p "$dir/build/lib" "$dir/src"
  printf 'int inner(void){return 7;}\n' > "$dir/src/inner.c"
  printf 'int inner(void);\nint outer(void){return inner();}\n' > "$dir/src/outer.c"
  printf 'int inner(void);\nint main(void){return inner();}\n' > "$dir/src/main.c"
  ( cd "$dir/build" \
    && gcc -fPIC -shared -Wl,-soname,libinner.so.1 -o lib/libinner.so.1.0.0 ../src/inner.c \
    && ln -sf libinner.so.1.0.0 lib/libinner.so.1 \
    && ln -sf libinner.so.1 lib/libinner.so \
    && gcc -fPIC -shared -Wl,-soname,libouter.so.1 -o lib/libouter.so.1.0.0 ../src/outer.c \
       -Llib -linner ) || return 1
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
build prog: link plain.o dep.o
build app: exe dep.o lib/libouter.so.1.0.0
build gen.h: gen
NINJA
}

# THE DRIVER'S STDERR GOES TO THE LOG, not to a file under `$out`. Sending it
# to `$out/emit.err` destroyed it on the run that needed it: a failed build
# registers no output path, so the one channel explaining a driver that exited
# 127 while printing nothing was deleted by the failure it was describing.
# Second instance of that defect in this same script.
#
# NIX_NINJA_TASK IS SET RATHER THAN RESOLVED. The task binary is on PATH here,
# but leaving the driver to find it makes this arm depend on a lookup that the
# A pricer does not depend on, and a difference between the two instruments is
# a difference in what their numbers mean.
emit() { # emit DRIVER DIR TARGET
  ( cd "$2/build" \
    && NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0 \
       NIX_NINJA_TASK="$(command -v nix-ninja-task)" \
       "$1" -t drv "$3" )
}

SHAPES="plain.o dep.o prog gen.h app"
moved=0; answered=0
{
  echo "configuration B, build dir under $PWD"
  echo "base $NN_BASE"
  echo "head $NN_HEAD"

  echo "task binary: $(command -v nix-ninja-task || echo NOT-ON-PATH)"
  echo "nix: $(command -v nix || echo NOT-ON-PATH)"
  # THE INSTRUMENT MUST BE ABLE TO SAY NO. Same driver, one shape, with and
  # without an extra flag on the command line.
  mk "$PWD/sens"; mk "$PWD/ref"
  sed -i 's|command = gcc -c $in -o $out|command = gcc -DPRICE_PROBE=1 -c $in -o $out|' "$PWD/sens/build/build.ninja"
  a=$(emit "$NN_HEAD" "$PWD/ref" plain.o); b=$(emit "$NN_HEAD" "$PWD/sens" plain.o)
  if [ -z "$a" ] || [ -z "$b" ]; then echo "SENSITIVITY NO ANSWER"; exit 0; fi
  if [ "$a" = "$b" ]; then echo "SENSITIVITY DEAD"; exit 0; fi
  echo "SENSITIVITY OK"

  mk "$PWD/det"
  d1=$(emit "$NN_HEAD" "$PWD/det" plain.o); d2=$(emit "$NN_HEAD" "$PWD/det" plain.o)
  if [ -z "$d1" ] || [ "$d1" != "$d2" ]; then echo "DETERMINISM FAILED"; exit 0; fi
  echo "DETERMINISM OK"

  # BOTH SIDES RUN AT THE SAME PATH, and on this configuration that is not
  # hygiene, it decides the answer. Under the exact mirror the build directory
  # is absolute and is emitted into the derivation, so a base fixture at
  # `b_plain_o` and a head fixture at `h_plain_o` differ for a reason that has
  # nothing to do with the edit: the first run of this gate reported four
  # shapes MOVED at byte-identical lengths. Configuration A hides the mistake,
  # because the driver relativises those paths there.
  for s in $SHAPES; do
    n=$(printf '%s' "$s" | tr '/.' '__')
    one="$PWD/shape_$n"
    mk "$one"
    x=$(emit "$NN_BASE" "$one" "$s")
    rm -rf "$one"
    mk "$one"
    y=$(emit "$NN_HEAD" "$one" "$s")
    if [ -z "$x" ] || [ -z "$y" ]; then echo "EMISSION $s NO-ANSWER"; moved=2; continue; fi
    answered=$((answered+1))
    if [ "$x" = "$y" ]; then
      echo "EMISSION $s identical ${#x}"
    else
      echo "EMISSION $s MOVED ${#x} ${#y}"
      [ $moved -eq 0 ] && moved=1
    fi
    printf '%s' "$x" > "$out/base-$n.drv.json"
    printf '%s' "$y" > "$out/head-$n.drv.json"
  done
  echo "SHAPES ANSWERED $answered"
  case $moved in
    0) echo "VERDICT FREE" ;;
    1) echo "VERDICT MOVED" ;;
    *) echo "VERDICT NO-ANSWER" ;;
  esac
# TEE, NOT A REDIRECT. Sending the whole block into `$out/verdict` alone
# means that when the build FAILS the output path is never registered and
# every diagnostic is destroyed, which is how the first run of this gate
# reported "builder failed with exit code 127" over an empty log. The verdict
# has to reach the build log too, since that is the only channel that
# survives a failure.
} 2>&1 | tee "$out/verdict"
