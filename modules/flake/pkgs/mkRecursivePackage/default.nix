# THE CONFIGURATION THE CONSUMER BUILDS ON, which nothing else in this tree
# covers. mkMesonPackage and mkCMakePackage both require `builder-rpc-v0`, so
# their derivations run under RecursiveFlag::RecursiveSubmitted, where the
# daemon permits SubmitOutput and refuses every build operation. A consumer
# using the compiler or ninja drop-in grants plain `recursive-nix` instead and
# runs under RecursiveFlag::Recursive, where the rules are the other way
# round: builds are permitted and SubmitOutput is refused.
#
# The two are mutually exclusive - the flag comes from the derivation's
# requiredSystemFeatures, and one socket carries it - so a package cannot be
# tested on both by one expression, and an example built the other way says
# nothing about this path. A dyndep defect that fails every example here was
# measured working on this configuration in a real round, which is what this
# helper exists to stop happening again.
#
# The driver runs in LOCAL mode: no NIX_NINJA_DRV, so it builds each task
# through the daemon and symlinks the results into the build directory, and
# the derivation installs from there like any ordinary package.
{ lib
, cmake
, coreutils
, nix
, nix-ninja
, nix-ninja-task
, patchelf
, stdenv
}:

{ name
, src
, target
, install ? "mkdir -p $out/bin && cp ${target} $out/bin/"
, nativeBuildInputs ? [ ]
, ...
}@args':

stdenv.mkDerivation (args' // {
  inherit name src;

  nativeBuildInputs = [
    cmake
    coreutils
    nix
    nix-ninja
    nix-ninja-task
    patchelf
  ] ++ nativeBuildInputs;

  # `recursive-nix` and NOT `builder-rpc-v0`: that single word is the whole
  # difference between this helper and mkCMakePackage.
  requiredSystemFeatures = [ "recursive-nix" ];

  cmakeFlags = [
    "-GNinja"
    "-DCMAKE_MAKE_PROGRAM=${nix-ninja}/bin/nix-ninja"
  ];

  dontUseNinjaBuild = true;
  dontUseNinjaInstall = true;
  dontUseNinjaCheck = true;

  preConfigure = ''
    export NINJA="${nix-ninja}/bin/nix-ninja"
    export NIX_CONFIG="extra-experimental-features = nix-command ca-derivations dynamic-derivations recursive-nix"
  '';

  buildPhase = ''
    runHook preBuild
    nix-ninja ${target}
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    ${install}
    runHook postInstall
  '';
})
