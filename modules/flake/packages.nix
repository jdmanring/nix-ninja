{ self, lib, ... }:
{
  perSystem = { pkgs, system, ... }: {
    packages = {
      inherit (pkgs)
        nix-ninja
        nix-ninja-task
        nix-ninja-llvm-coverage
      ;

      default = pkgs.buildEnv {
        name = "nix-ninja";
        paths = with pkgs; [
          nix-ninja
          nix-ninja-task
        ];
      };
    };

    # NOT a dynamic derivation and deliberately not gated with the others:
    # it is an ORDINARY derivation granting `recursive-nix`, which is the
    # configuration a consumer's drop-in builds on. Its whole value is being
    # the one example that does not require `builder-rpc-v0`, so it must stay
    # buildable where the feature-gated ones are not.
    packages.example-fortran-module-recursive =
      pkgs.example-fortran-module-recursive;
    packages.example-fortran-c-interface-recursive =
      pkgs.example-fortran-c-interface-recursive;
    packages.example-dotdot-source-include =
      pkgs.example-dotdot-source-include;
    packages.example-svt-av1-enc-handle = pkgs.example-svt-av1-enc-handle;

    # The examples are dynamic derivations, which can only be instantiated
    # when the `dynamic-derivations` experimental feature is enabled (probed
    # via the feature-gated `builtins.outputOf`).
    legacyPackages = lib.optionalAttrs (builtins ? outputOf) {
      example-hello = pkgs.example-hello.target;
      example-hello-cached-configure = pkgs.example-hello-cached-configure.target;
      example-generated-header = pkgs.example-generated-header.target;
      example-header = pkgs.example-header.target;
      example-multi-source = pkgs.example-multi-source.target;
      example-shared-lib = pkgs.example-shared-lib.target;
      example-run-script = pkgs.example-run-script.target;
      example-dynamic-deps = pkgs.example-dynamic-deps.target;
      example-cmake-hello = pkgs.example-cmake-hello.target;
      example-cmake-order-only-header = pkgs.example-cmake-order-only-header.target;
      example-fortran-c-interface = pkgs.example-fortran-c-interface.target;
      example-fortran-module = pkgs.example-fortran-module.target;
      example-nix = pkgs.example-nix.target;
    };

    devShells.default = pkgs.craneLib.devShell {
      checks = self.checks.${system};

      packages = with pkgs; [
        agg
        gnumake
        just
        meson
        taplo
      ];
    };
  };
}
