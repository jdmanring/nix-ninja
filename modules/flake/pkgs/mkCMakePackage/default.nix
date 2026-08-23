# mkMesonPackage's CMake sibling (upstream #20). The differences are the
# configure tool and two flags: `-GNinja` so CMake emits a ninja graph, and
# `CMAKE_MAKE_PROGRAM` pointed at nix-ninja, which answers the `--version`
# probe CMake runs at configure time (ninja CLI compatibility). Everything
# else - the unset-$out hack, the text-CA outer derivation, the
# `builtins.outputOf` target handle - is the meson builder's, kept in the
# same shape so a fix in one is findable from the other.
{ lib
, cmake
, coreutils
, nix
, nix-ninja
, nix-ninja-task
, patchelf
, stdenv
}:

{ name ? "${args'.pname}-${args'.version}"
, src
, target
, nativeBuildInputs ? [ ]
, ...
}@args':

let
  normalizedTarget = builtins.replaceStrings ["/"] ["-"] target;

  ninjaDrv = stdenv.mkDerivation (args' // {
    name = "${name}.drv";

    # See mkMesonPackage: stdenv's genericBuild assumes `out` is set, and
    # builder-rpc-v0 intentionally leaves it unset.
    out = "/nonexistent";

    nativeBuildInputs = [
      cmake
      coreutils
      nix
      nix-ninja
      nix-ninja-task
      patchelf
    ] ++ nativeBuildInputs;

    requiredSystemFeatures = [ "builder-rpc-v0" ];

    cmakeFlags = [
      "-GNinja"
      "-DCMAKE_MAKE_PROGRAM=${nix-ninja}/bin/nix-ninja"
    ];

    preConfigure = ''
      export NIX_NINJA_DRV="true"
      export NINJA="${nix-ninja}/bin/nix-ninja"
      export NIX_CONFIG="extra-experimental-features = nix-command ca-derivations dynamic-derivations"
    '';

    buildPhase = ''
      runHook preBuild
      nix-ninja ${target}
      runHook postBuild
    '';

    # stdenv adds a -rpath with a self reference but self references are not
    # allowed by text output.
    NIX_NO_SELF_RPATH = true;

    __contentAddressed = true;
    outputHashMode = "text";
    outputHashAlgo = "sha256";

    passthru = {
      target = builtins.outputOf ninjaDrv.outPath normalizedTarget;
    };
  });

in ninjaDrv
