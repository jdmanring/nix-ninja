#!/usr/bin/env bash
# A LINKER SCRIPT NAMED INSIDE -Wl AND WRITTEN BY ANOTHER EDGE, ON B.
#
# The A twin (version-script-generated-repro.sh) names the .vers by an
# absolute host path. Under the exact /build mirror the spelling the driver
# sees is the one compiler-rt's own link carries, which is what the consumer
# hit. Same reading as shim-link-include-b.sh: an A reduction cannot reach a
# mirror-only class.
#
#   PASS   the shared library links inside the derivation
#   FAIL   otherwise
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
#                        (143415f is the control, before e4cb09b)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-versb-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-version-script-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf ];
  dontUnpack = true; dontConfigure = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    S=\$NIX_BUILD_TOP/src; B=\$NIX_BUILD_TOP/build
    mkdir -p \$S \$B/lib/nsan \$B/lib/linux
    printf 'int nsan_init(void){return 0;}\n' > \$S/nsan.c
    printf 'int nsan_dummy(void){return 0;}\n' > \$S/dummy.c
    printf '#!/bin/sh\nprintf \"{ global: nsan_init; local: *; };\\\\n\" > \"\$1\"\n' > \$S/genvers.sh
    chmod +x \$S/genvers.sh
    {
      printf 'rule GEN\n  command = \$COMMAND\n'
      printf 'rule CC\n  command = gcc -fPIC -MD -MF \$out.d -c \$in -o \$out\n  deps = gcc\n  depfile = \$out.d\n'
      printf 'rule LINK\n  command = \$COMMAND\n'
      printf 'build lib/nsan/clang_rt.nsan-dynamic-x86_64.vers: GEN\n  COMMAND = %s/genvers.sh lib/nsan/clang_rt.nsan-dynamic-x86_64.vers\n' \"\$S\"
      printf 'build nsan.o: CC %s/nsan.c\n' \"\$S\"
      printf 'build dummy.o: CC %s/dummy.c | lib/nsan/clang_rt.nsan-dynamic-x86_64.vers\n' \"\$S\"
      printf 'build lib/linux/libclang_rt.nsan-x86_64.so: LINK nsan.o dummy.o\n  COMMAND = gcc -shared -o lib/linux/libclang_rt.nsan-x86_64.so nsan.o dummy.o -Wl,--version-script,%s/lib/nsan/clang_rt.nsan-dynamic-x86_64.vers\n' \"\$B\"
    } > \$B/build.ninja
    cd \$B
    # NO PRE-BUILD. This gate USED TO run the generator first, which put the
    # script on disk and isolated the produced-node skip exactly as arm 1 of
    # the A twin does, so it could not see the existence test sitting above
    # that skip and was GREEN for as long as the class was live. The link is
    # asked for alone, and dummy.o is what orders the generator ahead of it.
    echo asked-for-the-link-alone > \$NIX_BUILD_TOP/gen.log
    if nix-ninja lib/linux/libclang_rt.nsan-x86_64.so > \$NIX_BUILD_TOP/link.log 2>&1; then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
    [ -f lib/linux/libclang_rt.nsan-x86_64.so ] && echo yes > \$NIX_BUILD_TOP/solib || echo no > \$NIX_BUILD_TOP/solib
  '';
  installPhase = ''mkdir -p \$out; cp \$NIX_BUILD_TOP/verdict \$NIX_BUILD_TOP/solib \$NIX_BUILD_TOP/link.log \$NIX_BUILD_TOP/gen.log \$out/'';
}"
log=$(mktemp -t nn-versb-XXXXXX)
echo "driving the generated version script on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths \
        --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
if [ $? -ne 0 ] || [ -z "$OUT" ]; then
  echo "FAIL: the derivation did not build, so NOTHING WAS MEASURED. Tail:"
  tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"
verdict=$(cat "$OUT/verdict"); solib=$(cat "$OUT/solib")
echo "result   $OUT"
echo "link     $verdict, shared object present: $solib"
if [ "$verdict" = built ] && [ "$solib" = yes ]; then
  echo "PASS: a version script written by another edge is carried under the /build mirror"; exit 0
fi
echo "FAIL: verdict=$verdict solib=$solib"
grep -E 'ABORTED' "$OUT/link.log" | head -1
# THE ABORT NAMES A RESOLVED DERIVATION AND NOT THE ERROR, so a FAIL here
# printed no linker words at all and a broken fixture read the same as a live
# class. The compiler's own sentence is one level down, in the leaf's log.
leaf=$(grep -oE "/nix/store/[a-z0-9]{32}-ninja-build\.drv" "$OUT/link.log" | tail -1)
if [ -n "$leaf" ]; then
  nix log "$leaf" 2>/dev/null | grep -oE "cannot open linker script file [^:]*" | head -1
else
  echo "  NO ANSWER: the abort named no derivation to read"
fi
exit 1
