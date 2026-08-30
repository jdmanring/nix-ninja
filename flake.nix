{
  description = "Ninja compatible incremental C/C++ build system with Nix ";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    # WORKAROUND, with a removal trigger, for a bug in nix's own packaging.
    #
    # `packaging/dependencies.nix` passes a `patches` argument to
    # `pkgs.boost.override`, and nixpkgs' `boost/generic.nix` PREPENDS that to
    # its own list rather than replacing it. Since nixpkgs commit b5e044308f1
    # (2026-08-02, "boost: backport regression fixes for boost.context") that
    # list contains two ordered boostorg/context patches, and nix's prepended
    # copy - which is byte-for-byte commit 5883212, one of those very two -
    # breaks the order. `0921b9fd` then fails its hunk at
    # `fiber_fcontext.hpp:87` and boost does not build.
    #
    # It cannot be fixed with an overlay here: nix's flake imports its nixpkgs
    # itself, so nothing in this tree reaches the `pkgs.boost` it overrides.
    # Without this, NOTHING in this flake that needs `nix` builds - every
    # example and all six NixOS VM checks - which is why it is worth carrying.
    #
    # DEFER(upstream nix drops the `patches` argument, or nixpkgs stops
    # carrying 5883212): delete `nixpkgs-for-nix` and restore
    # `inputs.nixpkgs.follows = "nixpkgs"`. Staged as item 12 in
    # `local/upstream/`. Checked 2026-08-29: NixOS/nix master still has it.
    nixpkgs-for-nix.url = "github:NixOS/nixpkgs/6c9e167faa53a09769013922b0c1fc8087f4b7b2";
    nix = {
      url = "github:NixOS/nix";
      inputs.nixpkgs.follows = "nixpkgs-for-nix";
      inputs.nixpkgs-23-11.follows = "";
      inputs.nixpkgs-regression.follows = "";
    };
    globset = {
      url = "github:pdtpartners/globset";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-analyzer-src.follows = "";
    };
    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    flake-compat = {
      url = "github:edolstra/flake-compat";
      flake = false;
    };

    # The Cargo git dependencies, as locked inputs so their narHashes are
    # recorded. That lets evaluation resolve them from the Nix store without
    # network access (Cargo.lock alone records only the rev, which is not
    # enough) — needed by the offline NixOS test VMs, which cache all flake
    # inputs. Keep their revs in sync with Cargo.lock; the vendoring override
    # in `modules/flake/overlays.nix` checks this.
    harmonia = {
      url = "github:nix-community/harmonia";
      flake = false;
    };
    igraph = {
      url = "github:hinshun/igraph?ref=performance-improvements";
      flake = false;
    };
    n2 = {
      url = "github:hinshun/n2?ref=feature/minimal-pub";
      flake = false;
    };
  };

  outputs = inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [ "x86_64-linux" ];
      imports = [ ./modules ];
      flake = { inherit (inputs.nixpkgs) lib; };
    };
}
