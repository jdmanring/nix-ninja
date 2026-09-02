{ self, pkgs, lib, ... }@args:

import ./nix-build.nix {
  # THE CONSUMER'S CONFIGURATION, which nothing else gates. Every other
  # example requires builder-rpc-v0 and runs the driver in DRV mode; a
  # consumer's drop-in grants plain recursive-nix and runs it in local mode,
  # where the daemon permits builds and refuses SubmitOutput. The two are
  # mutually exclusive per derivation, so an example on one says nothing
  # about the other.
  #
  # Fortran because dyndep is the thing that differs between them: the driver
  # must realise a dyndep file to read it, which is a daemon build, permitted
  # here and refused under builder-rpc-v0.
  flakeOutput = "example-fortran-module-recursive";
  inputsFrom = [ pkgs.example-fortran-module-recursive ];
  # mkRecursivePackage installs into $out/bin, unlike the DRV-mode examples
  # whose target output IS the binary.
  cmdline = "bin/app";
  # Asserting the OUTPUT rather than the exit status is the point: dyndep's
  # job is the compile ORDER, and a build that resolved no Fortran task would
  # exit 0 just the same. The module having been compiled before its user is
  # only visible in the program working.
  expectedStdout = " hello from a fortran module";
} args
