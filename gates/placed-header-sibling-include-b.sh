#!/usr/bin/env bash
# WHERE A DECLARED HEADER IS MATERIALISED, UNDER THE EXACT /build MIRROR.
#
# The class is a CONJUNCTION OF THREE properties and no two of them
# reproduce it: the header is PLACED AS A SYMLINK into the store; it carries
# SYSTEM-HEADER STATUS, which CMake's generated precompiled-header wrapper
# confers with `#pragma GCC system_header` and which everything it includes
# inherits; and the includer is REACHED AS A QUOTED SIBLING of an already
# relocated header rather than named outright, since a relocated header's
# siblings resolve from the store directory holding that one file. The `-I`
# search path is NOT the mechanism: a header found through `-I` is opened at
# the path the search produced, so nothing relocates. The SPELLING is not a
# term either.
#
# THE VERDICT IS NOT "THE COMPILE SUCCEEDED". A run that stages the tree some
# other way would pass without exercising placement, which is the shape of
# pass this class has already produced twice. The gate makes the preprocessor
# NAME the file it opened: a placed header quote-includes a sibling that
# exists nowhere, so cpp must fail, and the path it prints for the INCLUDER
# is a direct read of where that header was materialised.
#
#   arm 1  ordinary build, the sibling present            must SUCCEED
#   arm 2  a declared header names a sibling that cannot
#          exist, reached WITHOUT the pragma               names SOME
#          includer, and at a build path. WHICH header cpp
#          names is not asserted in advance: relocation
#          moves the failure between hops, so the arm
#          captures whichever header the diagnostic names
#   arm 3  all three conjuncts: a pragma-bearing wrapper
#          names pch.h outright, pch.h reaches
#          SymbolTable.h as a BARE SIBLING, and that one
#          names ../Include/Common.h                       THE DISCRIMINATOR
#
#   PASS   every arm names its includer at a BUILD path
#   FAIL   an includer is named at a store path, which is glslang's class
#   exit 2 the derivation did not build, so NOTHING WAS MEASURED
#
# ARM 3 WAS INERT UNTIL THE MECHANISM WAS KNOWN, and that is why an earlier
# run of this gate returned the same green at the consumer's pin `37672a4`
# as at HEAD: its wrapper carried no pragma and its chain reached every
# header by a spelling resolved against the includer's own directory, so two
# of the three conjuncts were absent and the fixture could not fail. Control
# and subject both clean is an apparatus finding, never a class finding. No
# green taken before the pragma and the searched hop were added says
# anything about placement.
#
# THE FIX IS AT `fab689a` (task binary), which places a header as a COPY, so
# arm 3 must be controlled BELOW that revision to show it discriminating.
#
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")/../.." && pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-sibb-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-sibling-include-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf ];
  dontUnpack = true; dontConfigure = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0

    # The package's own geometry: a build directory UNDER the source root, so
    # every header is declared through a parent reference.
    S=\$NIX_BUILD_TOP/source; B=\$S/build
    mkdir -p \$S/glslang/Include \$S/glslang/MachineIndependent \$B \$S/b2

    printf '#include \"PoolAlloc.h\"\nint common_v(void){ return pool_v(); }\n' > \$S/glslang/Include/Common.h
    printf 'static int pool_v(void){ return 7; }\n' > \$S/glslang/Include/PoolAlloc.h
    printf '#include \"../Include/Common.h\"\nint sym_v(void){ return common_v(); }\n' > \$S/glslang/MachineIndependent/SymbolTable.h
    printf '#include \"glslang/MachineIndependent/SymbolTable.h\"\nint main(void){ return sym_v()==7?0:1; }\n' > \$S/use.c

    RULE='rule c_COMPILER\n  command = gcc -I.. -MD -MQ \$out -MF \$out.d -o \$out -c \$in\n  deps = gcc\n  depfile = \$out.d\n'
    printf \"\$RULE\"'build use.c.o: c_COMPILER ../use.c\n' > \$B/build.ninja
    cd \$B
    if nix-ninja use.c.o > \$NIX_BUILD_TOP/arm1.log 2>&1; then echo built > \$NIX_BUILD_TOP/arm1; else echo failed > \$NIX_BUILD_TOP/arm1; fi

    # arm 2. The includer names a sibling that exists nowhere, so cpp must
    # fail and must print the path it opened the INCLUDER at.
    mkdir -p \$S/probe/Include \$S/probe/Mid \$S/b2
    printf '#include \"NoSuchSibling.h\"\nint probe_v(void){ return 1; }\n' > \$S/probe/Include/Probe.h
    printf '#include \"../Include/Probe.h\"\nint mid_v(void){ return probe_v(); }\n' > \$S/probe/Mid/Mid.h
    printf '#include \"probe/Mid/Mid.h\"\nint main(void){ return mid_v(); }\n' > \$S/probe.c
    printf \"\$RULE\"'build probe.c.o: c_COMPILER ../probe.c\n' > \$S/b2/build.ninja
    cd \$S/b2
    nix-ninja probe.c.o > \$NIX_BUILD_TOP/arm2.log 2>&1 || true
    # The driver aborts naming a derivation; the compiler's own line is in
    # the task log one level down. Pull it back out.
    # THE DRIVER ALREADY RELAYS THE TASK'S OUTPUT in its abort message, so
    # the diagnostic is in arm2.log. 'nix log' is not available in here (no
    # network, no cache dir) and its failure is not a missing measurement.
    # STRIP ANSI FIRST: cc1 colours the path and the severity, so escapes sit
    # between the column number and the word error, and a pattern written
    # against plain text matches nothing and reads as no diagnostic at all.
    sed -r 's/\x1b\[[0-9;]*[A-Za-z]//g; s/\x1b\[K//g' \$NIX_BUILD_TOP/arm2.log > \$NIX_BUILD_TOP/arm2.plain
    : > \$NIX_BUILD_TOP/includer
    grep -haoE '[^ >]*[.]h:[0-9]+:[0-9]+: (fatal )?error' \$NIX_BUILD_TOP/arm2.plain 2>/dev/null \\
      | head -1 > \$NIX_BUILD_TOP/includer || true

    # arm 3. THE PRECOMPILED HEADER, which is the one structural difference
    # left between this fixture and the failing task: glslang's chain is
    # cmake_pch.hxx -> pch.h -> SymbolTable.h -> Common.h, and the edge that
    # fails is the one BUILDING the .gch, not an ordinary compile.
    mkdir -p \$S/g/Include \$S/g/MachineIndependent \$S/b3/CMakeFiles/g.dir
    printf '#include \"PoolAlloc.h\"
static int common_v(void){ return pool_v(); }
' > \$S/g/Include/Common.h
    printf 'static int pool_v(void){ return 7; }
' > \$S/g/Include/PoolAlloc.h
    printf '#include \"../Include/Common.h\"
' > \$S/g/MachineIndependent/SymbolTable.h
    printf '#include \"SymbolTable.h\"
' > \$S/g/MachineIndependent/pch.h
    printf '#pragma GCC system_header\n#include \"%s/g/MachineIndependent/pch.h\"\n' \"\$S\" > \$S/b3/CMakeFiles/g.dir/cmake_pch.hxx
    printf 'rule pch_COMPILER
  command = gcc -I.. -x c-header -MD -MQ \$out -MF \$out.d -o \$out -c \$in
  deps = gcc
  depfile = \$out.d
build CMakeFiles/g.dir/cmake_pch.hxx.gch: pch_COMPILER CMakeFiles/g.dir/cmake_pch.hxx
' > \$S/b3/build.ninja
    cd \$S/b3
    if nix-ninja CMakeFiles/g.dir/cmake_pch.hxx.gch > \$NIX_BUILD_TOP/arm3.log 2>&1; then echo built > \$NIX_BUILD_TOP/arm3; else echo failed > \$NIX_BUILD_TOP/arm3; fi
    sed -r 's/\x1b\[[0-9;]*[A-Za-z]//g; s/\x1b\[K//g' \$NIX_BUILD_TOP/arm3.log > \$NIX_BUILD_TOP/arm3.plain
    : > \$NIX_BUILD_TOP/pchincluder
    grep -haoE '[^ >]*[.]h:[0-9]+:[0-9]+: (fatal )?error' \$NIX_BUILD_TOP/arm3.plain 2>/dev/null \\
      | head -1 > \$NIX_BUILD_TOP/pchincluder || true

    # arm 5. IS THE COPY RULE'S EXTENSION LIST A CLOSED SET? The mechanism
    # is about a file being an INCLUDER while the guard keys on SUFFIX, so a
    # header-like file with a suffix nobody listed ought to reproduce the
    # class at the fix's own revision. The chain here is arm 3's with the
    # inner files renamed .inl, an ordinary C++ spelling.
    #
    # IT DOES NOT REPRODUCE, AND THIS ARM CURRENTLY PROVES NOTHING. Measured
    # with '-H' at both revisions, the include tree is byte identical and
    # every file is opened at a BUILD path:
    #     . /build/source/x/Mid/xpch.h
    #     .. /build/source/x/Mid/SymTab.inl
    #     ... /build/source/x/Mid/../Include/Com.inl
    # while arm 3, the same chain shape one suffix away, relocates Common.h
    # to a store path at the control.
    #
    # THE COPY LIST IS INCOMPLETE, AND THIS ARM MEASURES BY HOW MUCH.
    # The class is about a file being an INCLUDER while the task binary's
    # copy rule keys on SUFFIX and lists four. The driver already carries the
    # canonical set in `header_like` (h H hh hp hpp hxx h++ inc ipp inl tcc),
    # so the two disagree inside one codebase.
    #
    # `h` is in the list as the POSITIVE CONTROL: at the control revision
    # every suffix fails, and at the fix `h` builds while the rest do not.
    # That pair is what makes the other four a gap rather than a fixture
    # artefact.
    #
    # A FOURTH CONJUNCT, MEASURED HERE AND NOWHERE ELSE: THE BUILD PATH MUST
    # BE LONG ENOUGH. Renaming the directory `MachineIndependent` to `Mid`
    # and changing nothing else makes ARM 3 STOP REPRODUCING at the control,
    # and an arbitrary 18-character name in its place reproduces again. So
    # the term is LENGTH and not the spelling. This is very likely why every
    # short synthetic fixture in this project has failed to reproduce a class
    # the packages exhibit: real build paths are long, fixtures are not. The
    # store object cpp names is a BARE `<hash>-Common.h`, holding that one
    # file with no sibling beside it, which is the same bare-versus-directory
    # distinction the dav1d and dtc collisions turned on.
    #
    # AN EARLIER VERSION OF THIS ARM REPRODUCED NOTHING, and the cause was
    # its own names: with `Mid`, `Com`, `SymTab` and `x_pch.hxx` in place of
    # the package's spellings, every suffix built at both revisions and the
    # gap read as absent. Restoring arm 3's names made all five fail at the
    # control. Two readings were published off the inert version, that the
    # suffix is a term in cpp and that staging differs, and both were wrong.
    # DO NOT RENAME THIS FIXTURE.
    #
    # IT SCORES. The copy rule adopted the driver's `header_like` set, every
    # suffix builds, and a failure here is the divergence returning.
    : > \$NIX_BUILD_TOP/suffixes
    for X in h inl inc ipp tcc; do
      DX=\$S/x\$X; BX=\$S/b5\$X
      mkdir -p \$DX/Include \$DX/MachineIndependent \$BX/CMakeFiles/g.dir
      printf 'static int xp(void){ return 7; }\n' > \$DX/Include/PoolAlloc.\$X
      printf '#include \"PoolAlloc.%s\"\nstatic int xc(void){ return xp(); }\n' \$X > \$DX/Include/Common.\$X
      printf '#include \"../Include/Common.%s\"\n' \$X > \$DX/MachineIndependent/SymbolTable.\$X
      printf '#include \"SymbolTable.%s\"\n' \$X > \$DX/MachineIndependent/pch.h
      printf '#pragma GCC system_header\n#include \"%s/MachineIndependent/pch.h\"\n' \"\$DX\" > \$BX/CMakeFiles/g.dir/cmake_pch.hxx
      printf 'rule pch5
  command = gcc -I.. -x c-header -MD -MQ \$out -MF \$out.d -o \$out -c \$in
  deps = gcc
  depfile = \$out.d
build CMakeFiles/g.dir/cmake_pch.hxx.gch: pch5 CMakeFiles/g.dir/cmake_pch.hxx
' > \$BX/build.ninja
      cd \$BX
      if nix-ninja CMakeFiles/g.dir/cmake_pch.hxx.gch > \$NIX_BUILD_TOP/arm5-\$X.log 2>&1; then R=built; else R=failed; fi
      # HOW WAS IT PLACED? The include trace says where cpp OPENED a file,
      # never how the task binary materialised it. This edge declares the
      # same files and lists them, so the sandbox itself reports symlink or
      # regular file. No '-c', so it stays a generator edge.
      printf 'rule stat5
  command = ls -l \$in > \$out
build stage.txt: stat5 ../x%s/MachineIndependent/SymbolTable.%s ../x%s/Include/Common.%s
' \$X \$X \$X \$X > \$BX/stage.ninja
      nix-ninja -f stage.ninja stage.txt > \$NIX_BUILD_TOP/arm5-\$X-stage.log 2>&1 || true
      if grep -q ' -> /nix/store/' \$BX/stage.txt 2>/dev/null; then P=symlink; else P=copy; fi
      printf '%s %s %s\n' \$X \$R \$P >> \$NIX_BUILD_TOP/suffixes
      cp \$BX/stage.txt \$NIX_BUILD_TOP/arm5-\$X.stage 2>/dev/null || true
    done

    # arm 4. THE APPARATUS CONTROL, IN THE RUN RATHER THAN AT A REVISION.
    # Arms 1 to 3 all pass once the fix is in, so at HEAD they cannot tell a
    # live fixture from a dead one, and this gate shipped TWO dead fixtures
    # that each read as a pass. Here the symlink is placed BY HAND rather
    # than by the task binary, so the geometry is the class whatever the
    # driver does: a pragma-bearing wrapper, a relocated pch.h, and its bare
    # sibling. It MUST fail. If it builds, the fixture has lost its
    # discriminating power and arms 1 to 3 say nothing.
    mkdir -p \$S/ctl/store \$S/ctl/src/Mid \$S/ctl/src/Include \$S/ctl/b
    printf 'static int cc(void){ return 7; }\n' > \$S/ctl/src/Include/CCommon.h
    printf '#include \"../Include/CCommon.h\"\nstatic int cs(void){ return cc(); }\n' > \$S/ctl/store/CSym.h
    printf '#include \"CSym.h\"\n' > \$S/ctl/store/cpch.h
    ln -s \$S/ctl/store/CSym.h \$S/ctl/src/Mid/CSym.h
    ln -s \$S/ctl/store/cpch.h \$S/ctl/src/Mid/cpch.h
    printf '#pragma GCC system_header\n#include \"%s/ctl/src/Mid/cpch.h\"\n' \"\$S\" > \$S/ctl/b/cwrap.hxx
    if gcc -x c-header -c \$S/ctl/b/cwrap.hxx -o \$S/ctl/b/cwrap.hxx.gch > \$NIX_BUILD_TOP/arm4.log 2>&1; then
      echo built > \$NIX_BUILD_TOP/arm4
    else
      echo failed > \$NIX_BUILD_TOP/arm4
    fi
    # ITS POSITIVE HALF. A compile that fails for ANY reason satisfies the
    # assertion above, so the same geometry runs once more with the pragma
    # removed and nothing else changed. It MUST build. Together the two say
    # the fixture is live and the PRAGMA is what discriminates; failed twice
    # means gcc, the paths or the fixture are broken and neither arm speaks.
    printf '#include \"%s/ctl/src/Mid/cpch.h\"\n' \"\$S\" > \$S/ctl/b/cwrap-nopragma.hxx
    # ITS SUFFIX TWIN, WITH NO DRIVER IN THE QUESTION AT ALL. Identical
    # geometry, identical hand-placed symlinks, .inl instead of .h. If cpp
    # relocates on geometry alone this must fail like arm 4; if the suffix is
    # a term in cpp, it builds. Either way the driver is not involved, so
    # this answers the suffix sweep above without depending on placement.
    mkdir -p \$S/ctl2/store \$S/ctl2/src/Mid \$S/ctl2/src/Include \$S/ctl2/b
    printf 'static int dp(void){ return 7; }\n' > \$S/ctl2/src/Include/DPool.inl
    printf '#include \"DPool.inl\"\nstatic int dc(void){ return dp(); }\n' > \$S/ctl2/store/DCom.inl
    printf '#include \"../Include/DCom.inl\"\n' > \$S/ctl2/store/DSym.inl
    printf '#include \"DSym.inl\"\n' > \$S/ctl2/store/dpch.inl
    ln -s \$S/ctl2/store/DSym.inl \$S/ctl2/src/Mid/DSym.inl
    ln -s \$S/ctl2/store/dpch.inl \$S/ctl2/src/Mid/dpch.inl
    printf '#pragma GCC system_header\n#include \"%s/ctl2/src/Mid/dpch.inl\"\n' \"\$S\" > \$S/ctl2/b/dwrap.hxx
    if gcc -x c-header -c \$S/ctl2/b/dwrap.hxx -o \$S/ctl2/b/dwrap.hxx.gch > \$NIX_BUILD_TOP/arm4c.log 2>&1; then
      echo built > \$NIX_BUILD_TOP/arm4c
    else
      echo failed > \$NIX_BUILD_TOP/arm4c
    fi
    if gcc -x c-header -c \$S/ctl/b/cwrap-nopragma.hxx -o \$S/ctl/b/cwrap-nopragma.hxx.gch > \$NIX_BUILD_TOP/arm4b.log 2>&1; then
      echo built > \$NIX_BUILD_TOP/arm4b
    else
      echo failed > \$NIX_BUILD_TOP/arm4b
    fi
  '';
  installPhase = ''
    mkdir -p \$out
    cp \$NIX_BUILD_TOP/arm1 \$NIX_BUILD_TOP/arm1.log \$NIX_BUILD_TOP/arm2.log \$NIX_BUILD_TOP/includer \$out/
    [ -f \$NIX_BUILD_TOP/arm2.plain ] && cp \$NIX_BUILD_TOP/arm2.plain \$out/ || true
    cp \$NIX_BUILD_TOP/arm3 \$NIX_BUILD_TOP/arm3.log \$NIX_BUILD_TOP/pchincluder \$out/ || true
    cp \$NIX_BUILD_TOP/arm4 \$NIX_BUILD_TOP/arm4.log \$out/ || true
    cp \$NIX_BUILD_TOP/arm4b \$NIX_BUILD_TOP/arm4b.log \$out/ || true
    cp \$NIX_BUILD_TOP/arm4c \$NIX_BUILD_TOP/arm4c.log \$out/ || true
    cp \$NIX_BUILD_TOP/suffixes \$out/ || true
    cp \$NIX_BUILD_TOP/arm5-*.log \$NIX_BUILD_TOP/arm5-*.stage \$out/ 2>/dev/null || true
    [ -f \$NIX_BUILD_TOP/arm3.plain ] && cp \$NIX_BUILD_TOP/arm3.plain \$out/ || true
  '';
}"
log=$(mktemp -t nn-sibb-XXXXXX)
echo "driving the sibling-include shape on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths \
        --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
rc=$?
if [ $rc -ne 0 ] || [ -z "$OUT" ]; then
  echo "NO ANSWER: the derivation did not build, so nothing was measured (rc=$rc). Tail:"
  tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"
arm1=$(cat "$OUT/arm1")
inc=$(cat "$OUT/includer")
echo "result   $OUT"
echo "arm 1    $arm1"
echo "arm 2    includer named as: ${inc:-<no diagnostic captured>}"

fails=0
[ "$arm1" = built ] || { echo "FAIL arm 1: the ordinary build did not succeed under the mirror"; fails=1; }
if [ -z "$inc" ]; then
  echo "NO ANSWER arm 2: the probe printed no diagnostic naming the includer, so"
  echo "  the measurement did not happen; this is not a verdict about placement."
  exit 2
fi
case "$inc" in
  /nix/store/*) echo "FAIL arm 2: the includer was materialised at a STORE path, which is"
                echo "  glslang's class reproduced under the mirror"; fails=1 ;;
  *)            echo "  arm 2 OK  the includer was materialised at its build path" ;;
esac
arm3=$(cat "$OUT/arm3" 2>/dev/null || echo missing)
pchinc=$(cat "$OUT/pchincluder" 2>/dev/null || true)
pchincluder_seen=$pchinc
echo "arm 3    precompiled header edge: $arm3${pchinc:+, includer named as: $pchinc}"
case "$pchinc" in
  /nix/store/*) echo "FAIL arm 3: under a PRECOMPILED HEADER the includer was materialised"
                echo "  at a STORE path, which is glslang's class reproduced"; fails=1 ;;
esac
if [ "$arm3" != built ] && [ -z "$pchincluder_seen" ]; then
  echo "NO ANSWER arm 3: the pch edge failed and named no includer, so the"
  echo "  discriminating arm did not run; see arm3.log. This is not a pass."
  exit 2
fi

arm4=$(cat "$OUT/arm4" 2>/dev/null || echo missing)
echo "arm 4    hand-placed symlink geometry: $arm4 (must be failed)"
echo "arm 5    suffix sweep, every entry must build:"
while read -r X R P; do
  printf '           .%-4s %-7s placed as %s\n' "$X" "$R" "$P"
done < "$OUT/suffixes"
arm4c=$(cat "$OUT/arm4c" 2>/dev/null || echo missing)
echo "         hand-placed .inl, no driver involved: $arm4c (arm 4's .h twin: failed)"
echo "         every suffix must build: all five fail at 17c03af, and .h alone"
echo "         built at fab689a, so a failure here is the copy-list gap returning"
while read -r X R P; do
  if [ "$R" != built ]; then
    echo "FAIL arm 5: .$X did not build, placed as $P; the placement rule and the"
    echo "  driver's header predicate have diverged again"
    fails=1
  fi
done < "$OUT/suffixes"

arm4b=$(cat "$OUT/arm4b" 2>/dev/null || echo missing)
echo "arm 4b   same geometry, pragma removed: $arm4b (must be built)"
if [ "$arm4" != failed ] || [ "$arm4b" != built ]; then
  echo "NO ANSWER arm 4: the hand-placed control did not show the pragma"
  echo "  discriminating (with=$arm4, without=$arm4b; want failed/built), so the"
  echo "  fixture is not reproducing the class and arms 1 to 3 are not evidence."
  exit 2
fi

if [ $fails -eq 0 ]; then
  echo "PASS: a declared header is materialised at its build path under the mirror"
  exit 0
fi
exit 1
