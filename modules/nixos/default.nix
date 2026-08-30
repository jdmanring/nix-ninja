{
  perSystem = {
    nixosTests.nix-build-hello = import ./tests/nix-build-hello.nix;
    nixosTests.nix-build-header = import ./tests/nix-build-header.nix;
    nixosTests.nix-build-multi-source = import ./tests/nix-build-multi-source.nix;
    nixosTests.nix-build-shared-lib = import ./tests/nix-build-shared-lib.nix;
    nixosTests.nix-build-run-script = import ./tests/nix-build-run-script.nix;
    nixosTests.nix-build-dynamic-deps = import ./tests/nix-build-dynamic-deps.nix;
    # The generated-header class. Every other test here builds a target whose
    # every input exists before the build starts; this one does not, which is
    # the case three separate defects hid in.
    nixosTests.nix-build-generated-header = import ./tests/nix-build-generated-header.nix;
  };
}
