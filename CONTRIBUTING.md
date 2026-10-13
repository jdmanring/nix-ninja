# Contributing

Contributions should be made via pull requests. Pull requests will be reviewed
by one or more maintainers and merged when acceptable.

Work is tracked in [the roadmap](./docs/ROADMAP.md), which lists the open items
and what each one needs before it can be started. For a bug, [open an
issue](https://github.com/jdmanring/nix-ninja/issues/new/choose); the template
asks for the failing derivation and its leaf log, which is what turns a report
into a fix. For a question, or a design space that is still wide, use
[discussions](https://github.com/jdmanring/nix-ninja/discussions) before writing
code.

## Project Structure

The implementation is split into four crates, plus a vendored dependency:

1. deps-infer: Dependency inference for C/C++
  - Ninja depends on gcc to infer header includes but in Nix we need to know
    explicitly all the header inputs upfront.
  - Parses all the include flags from the ninja build rule
  - BFS scans in threads for an include regex to infer header includes
  - Will include more headers than gcc because we match everything instead of
    processing ifdef, etc.

2. nix-builder-rpc-client: Client for the daemon's `builder-rpc-v0` protocol
  - The dynamic task reads its own derivation back and resubmits it with the
    dependencies it discovered, which is what a dynamic derivation build is.

3. nix-ninja: Ninja compatible build system using Nix backend
  - Translate the Ninja build graph to Nix derivations
  - Generate individual derivations for each build target
  - Parse `build.ninja` through the vendored n2, and drive the build

4. nix-ninja-task: Nix derivation builder for `nix-ninja`
  - Since our generated derivations don't depend on `stdenv.mkDerivation` it is
    easier to maintain a builder as a Rust binary.
  - Prepares the source directory, runs the ninja build rule, and copies
    build outputs into Nix placeholders.

`vendor-n2/` is [n2](https://github.com/evmar/n2) by Evan Martin, vendored
rather than depended on because this project patches it. It is a workspace
member so it builds with the rest, and it carries its own Apache-2.0 license.
`vendor-n2/VENDORING.md` records the revision it was taken from and the changes
made to it.

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

If there's a good UX way of iterating on `nix-ninja` in a tmp store and without
modifying your main nix, please contribute!

### Iterating without touching your system nix

`contrib/devstore.sh` runs a daemon on a throwaway store as your own user, so
you can iterate without making a patched nix your system daemon. The daemon
comes from this repository's own `nix` flake input - the version already
pinned here, rather than one the script picked - built or substituted once
like any other input.

Builds in this store are UNSANDBOXED - it has no build users group and runs as
you - so the host filesystem is visible to them. That is fine for iteration
and wrong for anything anyone else will consume; do not push results from it.

```
contrib/devstore.sh selftest        # prove the store works
contrib/devstore.sh run -- cargo test
contrib/devstore.sh stop
```

Two settings are required and both fail by appearing to blame the client:

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

### The `gh` CLI reads the wrong repository here

This checkout has two remotes: `origin`, which is this project, and `upstream`,
the archived original it was forked from. `gh` resolves a repository from the
remotes it finds and picks `upstream`, so a bare `gh` command answers about the
archived repository:

    $ gh repo view --json nameWithOwner
    pdtpartners/nix-ninja      # not this project

That is silent and it is wrong in a way that reads as a real answer: `gh run
list` shows the archive's workflow history, `gh issue list` the archive's
issues, and neither is this repository's. Pass `--repo jdmanring/nix-ninja` on
every `gh` call, or `gh repo set-default jdmanring/nix-ninja` once. The GitHub
web UI is unaffected; only the CLI.

## Implementation notes

- See [the roadmap] for remaining work.
- See `TODO:` comments in code for open questions.

[nix@d904921]: https://github.com/NixOS/nix/commit/d904921eecbc17662fef67e8162bd3c7d1a54ce0
[the roadmap]: ./docs/ROADMAP.md
