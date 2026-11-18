#!/usr/bin/env bash
# THE gresource MANIFEST CLASS ON CONFIGURATION B.
#
# The A twin (gresource-manifest-repro.sh) resolves the manifest against a
# source tree outside /build, where the driver relativises. Under the exact
# /build mirror the --sourcedir stays absolute and the manifest is reached by
# an absolute path, which is the spelling glib's own edge carries and the one
# an A reduction cannot produce. Same reading as shim-link-include-b.sh.
#
#   PASS   the resource header is generated inside the derivation
#   FAIL   otherwise
#   NN_PROBE_REV=<rev>   build driver and task binary from that revision
#                        (143415f's parent, c72124c, is the control)
set -u
F='nix-command flakes dynamic-derivations ca-derivations recursive-nix'
REPO=$(cd "$(dirname "$0")" && while [ ! -f Cargo.toml ] && [ "$PWD" != / ]; do cd ..; done; pwd)
FLAKE=$REPO
if [ -n "${NN_PROBE_REV:-}" ]; then
  W=$(mktemp -d -t nn-gresb-wt-XXXXXX)
  git -C "$REPO" worktree add -q --detach "$W" "$NN_PROBE_REV" || exit 2
  trap 'git -C "$REPO" worktree remove --force "$W" >/dev/null 2>&1 || true' EXIT
  FLAKE=$W; echo "driver and task binary from $NN_PROBE_REV"
fi
[ -n "${NN_GATE_SRC:-}" ] && FLAKE=$NN_GATE_SRC

EXPR="
let f = builtins.getFlake (toString $FLAKE); sys = builtins.currentSystem; p = f.inputs.nixpkgs.legacyPackages.\${sys};
in p.stdenv.mkDerivation {
  pname = \"nn-gresource-b\"; version = \"0\";
  requiredSystemFeatures = [ \"recursive-nix\" ];
  nativeBuildInputs = [ f.packages.\${sys}.nix-ninja f.packages.\${sys}.nix-ninja-task f.inputs.nix.packages.\${sys}.default p.patchelf p.glib ];
  dontUnpack = true; dontConfigure = true; dontFixup = true;
  buildPhase = ''
    export NIX_CONFIG=\"extra-experimental-features = $F\"
    export NIX_NINJA_TASK=\$(command -v nix-ninja-task)
    export NIX_NINJA_IMPLICIT_INPUTS_LIMIT=0
    S=\$NIX_BUILD_TOP/glib-src; B=\$NIX_BUILD_TOP/build
    mkdir -p \$S/gio/tests \$B/gio/tests
    printf 'digits\n' > \$S/gio/tests/test1.txt
    printf '<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<gresources>\n  <gresource prefix=\"/digit_test\">\n    <file>test1.txt</file>\n  </gresource>\n</gresources>\n' > \$S/gio/tests/111_digit_test.gresource.xml
    GCR=\$(command -v glib-compile-resources)
    printf 'rule CUSTOM_COMMAND\n  command = \$COMMAND\nbuild gio/tests/digit_test_resources.h: CUSTOM_COMMAND %s/gio/tests/111_digit_test.gresource.xml\n  COMMAND = %s --target=gio/tests/digit_test_resources.h --sourcedir=%s/gio/tests --internal --generate --manual-register %s/gio/tests/111_digit_test.gresource.xml\n' \\
      \"\$S\" \"\$GCR\" \"\$S\" \"\$S\" > \$B/build.ninja
    cd \$B
    if nix-ninja gio/tests/digit_test_resources.h > \$NIX_BUILD_TOP/build.log 2>&1; then echo built > \$NIX_BUILD_TOP/verdict; else echo failed > \$NIX_BUILD_TOP/verdict; fi
    [ -f gio/tests/digit_test_resources.h ] && cp gio/tests/digit_test_resources.h \$NIX_BUILD_TOP/header.h || true
  '';
  installPhase = ''
    mkdir -p \$out
    cp \$NIX_BUILD_TOP/verdict \$NIX_BUILD_TOP/build.log \$out/
    [ -f \$NIX_BUILD_TOP/header.h ] && cp \$NIX_BUILD_TOP/header.h \$out/ || true
  '';
}"
log=$(mktemp -t nn-gresb-XXXXXX)
echo "driving the gresource manifest shape on configuration B; a driver build from source can take minutes."
OUT=$(nix build --extra-experimental-features "$F" --impure --no-link --print-out-paths \
        --cores "${NN_PROBE_CORES:-2}" --max-jobs 1 --expr "$EXPR" 2>"$log")
if [ $? -ne 0 ] || [ -z "$OUT" ]; then
  echo "FAIL: the derivation did not build, so NOTHING WAS MEASURED. Tail:"
  tail -20 "$log" | sed 's/^/  /'; exit 2
fi
rm -f "$log"
verdict=$(cat "$OUT/verdict")
echo "result   $OUT"
echo "edge     $verdict"
if [ "$verdict" = built ] && [ -f "$OUT/header.h" ]; then
  echo "PASS: the manifest's listed file is carried under the /build mirror"; exit 0
fi
echo "FAIL: verdict=$verdict header=$([ -f "$OUT/header.h" ] && echo yes || echo no)"
grep -E 'Failed to locate|ABORTED' "$OUT/build.log" | head -3
exit 1
