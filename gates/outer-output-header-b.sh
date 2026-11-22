#!/usr/bin/env bash
# A PACKAGE THAT COMPILES AGAINST HEADERS IT STAGED INTO ITS OWN OUTPUT.
#
# DOES NOT COVER THE WRITE SIDE, and nss has one. This gate models a compile
# that READS `-I$out/private/nss`. nss's gyp build also sets
# `-Dnss_dist_dir=$out`, so edges declare OUTPUTS inside the outer output, and
# those are refused by `normalize_build_path` from the `outputs.push` in
# `new_task`. Measured on the package on B: this gate passes and nss still
# fails. A green verdict here is a claim about the read half only.
#
# nss installs its headers into $out during the build phase and puts
# `-I$out/private/nss` on every compile. The outer output is not a valid
# store path while the build producing it is still running, so a task that
# declares it is refused by the daemon:
#
#   AddToStore: remote error: path '...-nss-3.112.5' is not valid
#
# Read from the package's own build log. THE LOG NAMES NO LTO. An earlier
# fixture for this class asserted LTO in its own filename and could never
# pass: it put the header in one place, and the arm meant to be its control
# had that path placeholder-rewritten, so neither arm could open the header
# at any revision.
#
# Three revisions tell the whole story, which is why the control matters:
#   9c2db59  the daemon rejection above (the class as reported)
#   dc33aa2  the outer output is excluded, so the compile now fails with
#            `nssheader.h: No such file or directory` (better, still broken)
#   cc08008  the header is staged under .nn-outer and the compile succeeds
#
# Configuration B, on the compiler drop-in's route: the compile is handed to
# the driver as a one-edge build file (the real compiler by store path,
# `deps = gcc`), run from the compile's directory with the build-dir walk
# off, which is what the drop-in writes for this `gcc -c`.
#   PASS  the object builds
#   FAIL  either failure text above, each named so the two are not confused
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
FLAKE=${NN_GATE_SRC:-$REPO}
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-outer-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi

expr="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-outer-header\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf ];
  dontUnpack = true; dontConfigure = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    # The package stages its own headers into its own output, as nss does,
    # and nothing else on disk holds a copy. That is the whole fixture: if
    # the header is reachable from anywhere else the gate proves nothing.
    mkdir -p \"\$out/private/nss\"
    printf '#define NSS_OK 1\n' > \"\$out/private/nss/nssheader.h\"
    B=\$NIX_BUILD_TOP/build; mkdir -p \$B; cd \$B
    printf '#include \"nssheader.h\"\nint fn(void){return NSS_OK;}\n' > a.c
    mkdir -p \$NIX_BUILD_TOP/.nn-cc
    printf 'rule cc\n  command = %s -I%s -c a.c -o a.o\n  deps = gcc\nbuild a.o: cc a.c\n' \
      \"\$(command -v gcc)\" \"\$out/private/nss\" > \$NIX_BUILD_TOP/.nn-cc/edge.ninja
    if NIX_NINJA_NO_BUILD_DIR_SCAN=1 nix-ninja -C \"\$PWD\" -f \$NIX_BUILD_TOP/.nn-cc/edge.ninja a.o > \$NIX_BUILD_TOP/build.log 2>&1
      then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
  '';
  installPhase = ''mkdir -p \"\$out/probe\"; cp \$NIX_BUILD_TOP/verdict \$NIX_BUILD_TOP/build.log \"\$out/probe/\"'';
}"

echo "driving a compile against a header staged in the package's own output;"
echo "a driver build from source can take minutes."
log=$(mktemp -t nn-outer-XXXXXX)
nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths \
   --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$expr" > "$log" 2>&1
o=$(grep -o '/nix/store/[a-z0-9]*-nn-outer-header-0' "$log" | tail -1)
if [ -z "$o" ] || [ ! -f "$o/probe/build.log" ]; then
  echo "FAIL: no output, so NOTHING WAS MEASURED; the nix build said:"
  tail -25 "$log" | sed 's/^/  /'
  rm -f "$log"; exit 2
fi
rm -f "$log"
verdict=$(cat "$o/probe/verdict")
echo "verdict  $verdict"
if grep -q 'is not valid' "$o/probe/build.log"; then
  echo "FAIL: the task declared the outer derivation's own output (the class)"
  grep -o "path '[^']*' is not valid" "$o/probe/build.log" | head -1
  exit 1
fi
if grep -q 'No such file or directory' "$o/probe/build.log"; then
  echo "FAIL: the outer output is excluded but its header is not carried"
  grep -o '[a-z]*\.h: No such file or directory' "$o/probe/build.log" | head -1
  exit 1
fi
[ "$verdict" = built ] && { echo "PASS: a header staged in the package's own output is carried"; exit 0; }
echo "FAIL: no build and neither known failure; read $o/probe/build.log"
exit 1
