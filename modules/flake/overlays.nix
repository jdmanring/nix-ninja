{ self, inputs, lib, ... }:
{
  flake.overlays.internal = self: super:
    let
      craneLib = inputs.crane.mkLib self;

      src = lib.fileset.toSource {
        root = ../../.;
        fileset = inputs.globset.lib.globs ../../. [
          "Cargo.lock"
          "**/Cargo.toml"
          "**/*.rs"
        ];
      };

      # The Cargo git dependencies, mapped from a URL substring to the flake
      # input holding their checkout (see the inputs' comment in flake.nix).
      cargoGitDeps = {
        "github.com/nix-community/harmonia" = inputs.harmonia;
        "github.com/hinshun/igraph" = inputs.igraph;
        "github.com/hinshun/n2" = inputs.n2;
      };

      # Vendor the Cargo git dependencies from their locked flake inputs
      # instead of letting crane fetch them with `builtins.fetchGit`, which
      # needs network access at evaluation time.
      #
      # TODO: An alternative that would keep Cargo.lock as the single source
      # of truth (no per-dep flake input, no rev-sync check): vendor the old
      # way on the outside, ship the built vendor dir into the offline NixOS
      # test VMs via `additionalPaths`, and have the inside evaluation take it
      # through an overridable flake input (`nix build --override-input
      # cargo-vendor-dir path:<dir> ...`). The catch is that with
      # input-addressed derivations, the inside evaluation (vendor dir passed
      # in) and the outside evaluation (vendor dir computed) produce different
      # drv hashes for nix-ninja and everything downstream, so the VM would
      # rebuild the whole workspace instead of reusing the cached binaries.
      # This works out only if the derivation producing the vendor dir
      # content-addresses its output: then the output path depends solely on
      # the vendored contents, both evaluations realise the same store path,
      # and the downstream drvs coincide again.
      cargoVendorDir = craneLib.vendorCargoDeps {
        inherit src;
        overrideVendorGitCheckout = ps: drv:
          let
            p = lib.head ps;
            # `source` looks like "git+https://…?branch=…#<rev>".
            lockedRev = lib.last (lib.splitString "#" p.source);
            matches = lib.filterAttrs (infix: _: lib.hasInfix infix p.source) cargoGitDeps;
            input = lib.head (lib.attrValues matches);
          in
          lib.throwIf (matches == { }) ''
            Cargo git dependency ${p.source} has no flake input in
            `cargoGitDeps` (modules/flake/overlays.nix); add one so it can be
            vendored without network access.
          ''
            (lib.throwIfNot (lockedRev == input.rev) ''
              Cargo.lock pins ${p.name} at ${lockedRev}
              but its flake input is at ${input.rev}.
              Re-sync them with `nix flake update` and/or `cargo update`.
            ''
              (drv.overrideAttrs (_: { src = input; })));
      };

      # Common arguments can be set here to avoid repeating them later
      commonArgs = {
        inherit src cargoVendorDir;
        inherit (craneLib.crateNameFromCargoToml { inherit src; }) version;
        strictDeps = true;
        nativeBuildInputs = [
          self.pkg-config
        ];
      };

      # Build *just* the cargo dependencies, so we can reuse
      # all of that work (e.g. via cachix) when running in CI
      cargoArtifacts = craneLib.buildDepsOnly commonArgs;

      craneLibLLvmTools = craneLib.overrideToolchain
        (inputs.fenix.packages.${self.system}.complete.withComponents [
          "cargo"
          "llvm-tools"
          "rustc"
        ]);

    in {
      inherit craneLib;

      # Internal attr for code-reuse across flake modules.
      _nix-ninja = {
        inherit cargoArtifacts commonArgs src;
      };

      mkMesonPackage = self.callPackage ./pkgs/mkMesonPackage {
        inherit (self) nix-ninja nix-ninja-task;
        nix = inputs.nix.packages.${self.system}.nix;
      };
      mkCMakePackage = self.callPackage ./pkgs/mkCMakePackage {
        inherit (self) nix-ninja nix-ninja-task;
        nix = inputs.nix.packages.${self.system}.nix;
      };

      # meson --internal symbolextractor depends on readelf.
      # meson = super.meson.overrideAttrs(o: {
      #   buildInputs = (o.buildInputs or []) ++ [
      #     self.binutils
      #   ];
      # });

      nix-ninja-llvm-coverage = craneLibLLvmTools.cargoLlvmCov (commonArgs // {
        inherit cargoArtifacts;
      });

      # Build the actual crate itself, reusing the dependency
      # artifacts from above.
      nix-ninja = craneLib.buildPackage (commonArgs // {
        inherit cargoArtifacts;
        pname = "nix-ninja";
        cargoExtraArgs = "-p nix-ninja";
      });

      nix-ninja-task = craneLib.buildPackage (commonArgs // {
        inherit cargoArtifacts;
        pname = "nix-ninja-task";
        cargoExtraArgs = "-p nix-ninja-task";
        src = lib.fileset.toSource {
          root = ../../.;
          fileset = inputs.globset.lib.globs ../../. [
            "Cargo.{toml,lock}"
            "crates/nix-{libstore,ninja-task}/Cargo.toml"
            "crates/nix-{libstore,ninja-task}/**/*.rs"
            "crates/deps-infer/Cargo.toml"
            "crates/deps-infer/**/*.rs"
            # deps-infer's n2 is the vendored copy since the rspfile fix;
            # this allowlist is why an untracked or unlisted vendor dir
            # fails only in the flake build while cargo builds fine.
            "vendor-n2/Cargo.toml"
            "vendor-n2/**/*.rs"
          ];
        };
      });

      example-hello = self.mkMesonPackage {
        name = "example-hello";
        src = ./examples/hello;
        target = "hello";
      };

      # UPSTREAM #16's subject. Same sources as example-hello, configured
      # through the cached configure derivation instead of `meson setup`
      # inside the ninja derivation, so the two are directly comparable and
      # the flag has a package behind it rather than an argument.
      example-hello-cached-configure = self.mkMesonPackage {
        name = "example-hello-cached-configure";
        src = ./examples/hello;
        target = "hello";
        useConfigureCache = true;
      };

      # The generated-header class: a header produced during the build and
      # declared order-only on the compile edge, so it does not exist when
      # the driver scans. libvmaf's vcs_version.h and liblapack's
      # VerifyFortran.h are this shape, and nix itself failed three separate
      # ways on it before it built.
      example-generated-header = self.mkMesonPackage {
        name = "example-generated-header";
        src = ./examples/generated-header;
        target = "main";
      };

      example-header = self.mkMesonPackage {
        name = "example-header";
        src = ./examples/header;
        target = "hello";
        nativeBuildInputs = [ self.nlohmann_json ];
      };

      example-multi-source = self.mkMesonPackage {
        name = "example-multi-source";
        src = ./examples/multi-source;
        target = "main";
      };

      example-shared-lib = self.mkMesonPackage {
        name = "example-shared-lib";
        src = ./examples/shared-lib;
        target = "main";
      };

      example-run-script = self.mkMesonPackage {
        name = "example-run-script";
        src = ./examples/run-script;
        target = "main";
      };

      example-dynamic-deps= self.mkMesonPackage {
        name = "example-dynamic-deps";
        src = ./examples/dynamic-deps;
        target = "main";
        nativeBuildInputs = [ self.nlohmann_json self.pkg-config ];
      };

      # A generated header reached through TWO order-only phony hops, which
      # is what CMake emits for `add_dependencies(<lib> <custom target>)`.
      # The meson example above cannot stand in for it: meson writes
      # RELATIVE include directories, so the scan's spelling and the graph's
      # agree there, while CMake writes absolute ones and they do not.
      #
      # It is a reduction of svt-av1's arrangement (a BYPRODUCTS stamp edge,
      # the consumer in a different directory from the generator, one of its
      # three translation units not including the header) and it does NOT
      # reproduce svt-av1's failure, which stands open. What it does cover
      # is the CMake route end to end under NIX_NINJA_DRV, which nothing in
      # this tree covered before.
      example-cmake-order-only-header = self.mkCMakePackage {
        name = "example-cmake-order-only-header";
        src = ./examples/cmake-order-only-header;
        target = "app";
      };

      example-cmake-hello = self.mkCMakePackage {
        name = "example-cmake-hello";
        src = ./examples/cmake-hello;
        target = "hello";
      };

      # Configure-time Fortran/C detection, driven through nix-ninja. It
      # verifies nothing about a TARGET: FortranCInterface_VERIFY() runs
      # during configure, so this example fails at configure or not at all.
      #
      # IT FAILS TODAY, DELIBERATELY, and it is the reproduction of an open
      # defect rather than a test that regressed. mkCMakePackage asks for
      # `builder-rpc-v0`, so the daemon runs as RecursiveSubmitted, and the
      # dyndep pre-build's BuildPathsWithResults is refused:
      #
      #   Caused by: daemon error: BuildPathsWithResults: remote error:
      #   Operation 46 not allowed inside derivation
      #
      # SCOPED TO THAT CONFIGURATION, and the scope was overstated here once.
      # A consumer granting plain `recursive-nix` instead gets the Recursive
      # flag, where that build IS permitted - measured in a distribution
      # round, where this same dyndep step succeeded and a sibling package
      # built 29 dyndep files. So this is a defect of the builder-rpc-v0
      # path, NOT of Fortran, and not of the route ArtNix builds on.
      # Registered in legacyPackages and NOT as a NixOS VM test, so it does
      # not redden `nix flake check` while the defect stands.
      example-fortran-c-interface = self.mkCMakePackage {
        name = "example-fortran-c-interface";
        src = ./examples/fortran-c-interface;
        target = "app";
        nativeBuildInputs = [ self.gfortran ];
      };

      # The smallest build that cannot succeed without ninja dyndep: one
      # Fortran module defined in one file and used in another, so the
      # compile order is not knowable from the graph alone. No
      # configure-time detection, which is what separates it from
      # example-fortran-c-interface above and is why it is the one to
      # measure the blast radius with.
      #
      # IT FAILS TODAY, DELIBERATELY, with the same
      # `Operation 46 not allowed inside derivation` as its neighbour. Two
      # files and one module are enough, which bounds the defect to the
      # dyndep pre-build itself rather than to anything FortranCInterface
      # does - and, with its neighbour, to the `builder-rpc-v0`
      # configuration these examples run under.
      example-fortran-module = self.mkCMakePackage {
        name = "example-fortran-module";
        src = ./examples/fortran-module;
        target = "app";
        nativeBuildInputs = [ self.gfortran ];
      };

      mkRecursivePackage = self.callPackage ./pkgs/mkRecursivePackage {
        inherit (self) nix-ninja nix-ninja-task;
        nix = inputs.nix.packages.${self.system}.nix;
      };

      # The same two Fortran files as example-fortran-module, built on the
      # CONSUMER's configuration: plain recursive-nix, driver in local mode.
      # The pair is the point - one expression per configuration over one
      # source tree is the only way to tell a tool defect from a
      # configuration defect, and this tree had no way to tell them apart.
      example-fortran-module-recursive = self.mkRecursivePackage {
        name = "example-fortran-module-recursive";
        src = ./examples/fortran-module;
        target = "app";
        nativeBuildInputs = [ self.gfortran ];
        # RUN IT. A build that resolved no Fortran task would exit 0 and
        # install nothing this checks, and dyndep's whole job is the ORDER -
        # the module has to be compiled before its user, and the only
        # evidence of that surviving into the output is the program working.
        postInstall = ''
          $out/bin/app | grep -q "hello from a fortran module"
        '';
      };

      # liblapack's failing shape on the CONSUMER's configuration, which is
      # where it actually fails. The builder-rpc-v0 sibling above dies
      # earlier, at the dyndep pre-build, so it can say nothing about the
      # empty mangling section.
      example-fortran-c-interface-recursive = self.mkRecursivePackage {
        name = "example-fortran-c-interface-recursive";
        src = ./examples/fortran-c-interface;
        target = "app";
        nativeBuildInputs = [ self.gfortran ];
      };

      example-nix = self.callPackage ./examples/nix { src = inputs.nix; };
    };

  perSystem = { system, ... }: {
    _module.args.pkgs = import inputs.nixpkgs {
      inherit system;
      overlays = [ self.overlays.internal ];
    };
  };
}
