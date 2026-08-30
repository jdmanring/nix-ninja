{ self, pkgs, lib, ... }@args:

import ./nix-build.nix {
  flakeOutput = "example-generated-header";
  inputsFrom = [ pkgs.example-generated-header ];
  cmdline = "main";
  expectedStdout = "Hello generated header example!";
} args
