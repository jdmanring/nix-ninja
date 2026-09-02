{ self, pkgs, lib, ... }@args:

import ./nix-build.nix {
  flakeOutput = "example-cmake-order-only-header";
  inputsFrom = [ pkgs.example-cmake-order-only-header ];
  # Reaches the derivation as a store path inside `cmakeFlags`, so no
  # *BuildInputs list names it and the harness cannot find it. Cached as a
  # path rather than as an input: it is a FILE, and stdenv sources a
  # non-directory input as a setup hook, which would execute the dispatch
  # script during the closure build.
  extraCachedPaths = [ pkgs.example-cmake-order-only-header.dispatchNinja ];
  cmdline = "app";
  expectedStdout = "generated-4.1.0";
} args
