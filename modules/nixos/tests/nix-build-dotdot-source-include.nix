{ self, pkgs, lib, ... }@args:

import ./nix-build.nix {
  # openblas's shape: a generated source including another source through a
  # SIBLING directory, so the compiler must walk a directory that no input
  # materializes. 2,602 translation units died on it in one round.
  flakeOutput = "example-dotdot-source-include";
  inputsFrom = [ pkgs.example-dotdot-source-include ];
  cmdline = "bin/app";
  # The program's output is the only evidence the include resolved: a build
  # that got the path wrong fails to link, but one that resolved it to the
  # wrong file would still exit 0.
  expectedStdout = "generic impl reached through dotdot";
} args
