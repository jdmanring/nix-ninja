# Returns a NixOS test module that strictly exercises the Nix build of an
# output of nix-ninja flake.
#
# It starts the VM with the flake inputs and inputs to the output derivation
# cached, so the Nix build can run offline and only builds the derivation and
# nothing more.

# Output name it should `nix build ${self}#${flakeOutput}`.
{ flakeOutput
# Inputs of packages it should cache in the VM /nix/store.
, inputsFrom
# Cmdline to run binary from built outPath.
, cmdline
# Expected stdout from the binary it builds.
, expectedStdout
# Store paths to cache in the VM that are NOT reachable through any
# *BuildInputs list. `inputsFrom` is walked for those four attributes only, so
# an input that reaches a derivation as a store path inside a string - CMake's
# dispatch script arrives through `cmakeFlags` - is invisible to it and the VM
# has to build it offline. These are cached as PATHS rather than added to an
# input list: a `writeShellScript` output is a FILE, and stdenv SOURCES a
# non-directory input as a setup hook, which runs it.
, extraCachedPaths ? [ ]
}:

{ self, pkgs, lib, ... }:
let
  # Filtered (allowlisted) copy of the flake for the VM to build from, so the
  # test derivation only depends on files that affect what it builds —
  # changes to e.g. README or CI config don't invalidate the tests. The
  # allowlist must cover everything the in-VM `nix build` evaluates,
  # producing filesets identical to the ones the flake modules construct
  # (same contents → same store paths → the prebuilt closures cached in the
  # VM still match).
  flakeSrc = lib.fileset.toSource {
    root = ../../..;
    fileset = self.inputs.globset.lib.globs ../../.. [
      "flake.nix"
      "flake.lock"
      "modules/**"
      "Cargo.lock"
      "**/Cargo.toml"
      "**/*.rs"
      "deny.toml"
    ];
  };

  # Note: no `lib.subtractLists inputsFrom` here (unlike `pkgs.mkShell`, where
  # this pattern comes from): comparing derivations forces their `outPath`,
  # and the `inputsFrom` derivations are content-addressed, so that would
  # require the ca-derivations feature on the host evaluator. The examples
  # never appear in their own input lists anyway.
  mergeInputs = name:
    lib.flatten (lib.catAttrs name inputsFrom);

  # Extracted from `pkgs.mkShell` to capture the closure of inputs of a
  # derivation. I'd like to use `<drv>.inputDerivation` but getting an error
  # from Nix@2.30 atm:
  #
  # ```sh
  # error: derivation names are allowed to end in '.drv' only if they produce a
  # single derivation file
  # ```
  inputsClosure = pkgs.stdenv.mkDerivation {
    name = "inputs-for-${flakeOutput}";
    buildInputs = mergeInputs "buildInputs";
    nativeBuildInputs = mergeInputs "nativeBuildInputs";
    propagatedBuildInputs = mergeInputs "propagatedBuildInputs";
    propagatedNativeBuildInputs = mergeInputs "propagatedNativeBuildInputs";

    phases = [ "buildPhase" ];

    buildPhase = ''
      export >> "$out"
    '';
  };

in {
  nodes.machine = {
    virtualisation = {
      # Closures that are made available to VM, these cache all inputs & flake
      # inputs so that during the NixOS test it only needs to build the dynamic
      # derivation.
      additionalPaths = [
        inputsClosure
      ] ++ extraCachedPaths ++ (builtins.attrValues self.inputs);
    };

    environment.systemPackages = with pkgs; [
      git
      nix-ninja
      nix-ninja-task
      # For the RPATH assertion in the test script below.
      patchelf
    ];

    nix.package = self.inputs.nix.packages.${pkgs.stdenv.hostPlatform.system}.nix;

    nix.extraOptions = ''
      experimental-features = nix-command flakes dynamic-derivations ca-derivations recursive-nix
      extra-system-features = builder-rpc-v0
    '';
  };

  testScript = ''
    start_all()

    result = machine.succeed("nix build --print-out-paths ${flakeSrc}#${flakeOutput}").strip()
    binary = f"{result}/${cmdline}"
    out = machine.succeed(binary)
    assert "${expectedStdout}" in out

    # THE ARTIFACT, NOT ONLY ITS OUTPUT. Every test here built a binary, ran
    # it and checked stdout; none looked at what was linked into it. A
    # mutation replacing compute_new_rpath wholesale - returning an RPATH of
    # one EMPTY entry, which the loader reads as the current directory -
    # survived the entire suite, and the binary still printed the right
    # string. Running successfully on the build machine is exactly what a
    # wrong RPATH does.
    #
    # Three properties, each a defect this project has actually shipped or
    # nearly shipped:
    #   - no empty element      an empty entry means `.` to the loader
    #   - no $ORIGIN            relative to the OUTPUT, which moved into the
    #                           store; it resolved somewhere else at link time
    #   - every entry in /nix/store  a surviving /build/... entry means the
    #                           rewrite did not happen at all
    rpath = machine.execute(f"patchelf --print-rpath {binary}")
    if rpath[0] == 0:
        entries = [e for e in rpath[1].strip().split(":") if e != ""]
        raw = rpath[1].strip()
        if raw:
            assert "" not in raw.split(":"), f"empty RPATH element (a cwd search): {raw!r}"
            assert "$ORIGIN" not in raw, f"unresolved $ORIGIN in RPATH: {raw!r}"
            for e in entries:
                assert e.startswith("/nix/store"), f"RPATH entry outside the store: {e!r} in {raw!r}"
  '';
}
