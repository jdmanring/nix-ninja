# Contributing

Contributions should be made via pull requests. Pull requests will be reviewed
by one or more maintainers and merged when acceptable.

Consider upstreaming anything that is not specific to nix-ninja to reduce
the amount that needs to be maintained in this repository.

## Project Structure

The implementation is split into five crates:

1. nix-libstore: Core Nix data structures
  - Want to use `nix-compat` from [snix] but they only support Nix 2.3 which
    doesn't include content-addressed derivations and dynamic derivations.

2. nix-tool: Helper to spawn Nix commands
  - In the future, we can try generating derivations without depending on
    recursive nix by implementing `nix store add` and `nix derivation add` in
    Rust.

3. deps-infer: Dependency inference for C/C++
  - Ninja depends on gcc to infer header includes but in Nix we need to know
    explicitly all the header inputs upfront.
  - Parses all the include flags from the ninja build rule
  - BFS scans in threads for an include regex to infer header includes
  - Will include more headers than gcc because we match everything instead of
    processing ifdef, etc.

4. nix-ninja-task: Nix derivation builder for `nix-ninja`
  - Since our generated derivations don't depend on `stdenv.mkDerivation` it is
    easier to maintain a builder as a Rust binary.
  - Prepares the source directory, runs the ninja build rule, and copies
    build outputs into Nix placeholders.

5. nix-ninja: Ninja compatible build system using Nix backend
  - Translate Ninja build graph to Nix derivations
  - Generate individual derivations for each build target

## Developing locally

Currently, the easiest way to develop `nix-ninja` is to use [nix@d904921]
directly as your nix daemon. This way, you can build `nix-ninja` itself and the
examples (e.g. when iterating on `nix build .#example-nix`) all in one build.

```nix
# flake.nix
{
  inputs = {
    nix = {
      url = "github:NixOS/nix?ref=d904921eecbc17662fef67e8162bd3c7d1a54ce0";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs: inputs:
    let
      pkgs = import inputs.nixpkgs {
        system = "x86_64-linux";
        overlays = [ inputs.nix.overlays.default ];
      };

    in {
      # ... NixOS module
      nix.extraOptions = ''
        experimental-features = flakes nix-command dynamic-derivations recursive-nix ca-derivations
      '';
      # ...
    };
}
```

### Iterating without touching your system nix

`contrib/devstore.sh` runs a daemon on a throwaway store as your own user, so
you can iterate without making a patched nix your system daemon. The daemon
comes from this repository's own `nix` flake input, so nothing new is fetched.

```
contrib/devstore.sh selftest        # prove the store works
contrib/devstore.sh run -- cargo test
contrib/devstore.sh stop
```

Two settings are required and both fail by appearing to blame the client, so
they are worth knowing before you debug one:

- `trusted-users` must name you in the DAEMON's config. Without it the daemon
  discards client settings and reports `experimental Nix feature
  'ca-derivations' is disabled`, which reads as the client missing a feature
  it actually has.
- `--extra-experimental-features` must be on the DAEMON's own argv. It does
  not read them from the `experimental-features` line of the config file it
  was pointed at, and the error is the same one as above.

The selftest also checks that the daemon advertises `builder-rpc-v0`, which
every dynamic task requires. A daemon without it otherwise fails much later
with a message that does not name the cause.

## Implementation notes

- See [todo] for remaining work.
- See `TODO:` comments in code for open questions.

[nix@d904921]: https://github.com/NixOS/nix/commit/d904921eecbc17662fef67e8162bd3c7d1a54ce0
[todo]: ./docs/todo.md
