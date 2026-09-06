<div align="center">

# nix-ninja

Incremental compilation of [Ninja build files][ninja-build] using
[Nix Dynamic Derivations][dynamic-derivations].

Choosing ninja as the build graph representation lets us support any build
system that outputs ninja like CMake, meson, premake, gn, etc.

[![Demo](docs/demo.gif)](https://asciinema.org/a/711344)

[Key features](#key-features) •
[Getting started](#getting-started) •
[Design notes][design notes] •
[Contributing](CONTRIBUTING.md)

</div>

## Key features

> [!IMPORTANT]
> There are still major todos, and depends on experimental features from an
> unreleased version of Nix. Come help us get nix-ninja to be useful day-to-day
> and working with an official Nix release!

> [!WARNING]
> macOS: the examples below have no macOS attribute to build. This flake
> declares `systems = [ "x86_64-linux" ]`, so
> `nix build github:pdtpartners/nix-ninja#example-hello` on a Mac fails with
> `does not provide attribute 'packages.aarch64-darwin.example-hello'`, or the
> `x86_64-darwin` form on an Intel Mac.
> See [multi-arch support](https://github.com/pdtpartners/nix-ninja/issues/14).
>
> If you hit `experimental Nix feature '...' is disabled` while passing
> `--extra-experimental-features`, that is a separate and not macOS-specific
> problem: a multi-user daemon reads its own `/etc/nix/nix.conf`, so the
> features must be listed there and the daemon restarted
> (`sudo launchctl kickstart -k system/org.nixos.nix-daemon` on macOS). A
> stock multi-user install leaves them out on any platform. Being in
> `trusted-users` does not substitute for it.

- Parses `ninja.build` files and generates a derivation per compilation unit.
- Stores build inputs & outputs in content-addressed derivations for granular
  and Nix-native incrementality.
- Compatible CLI for ninja, so if you set `$NINJA` to `nix-ninja` then meson
  just works.
- Supports running locally (which runs `nix build` on your behalf), or inside a
  Nix derivation (which creates dynamic derivations).

## Getting started

First you need to use Nix 2.30 or later (newer than stable) and enable the
following experimental features:

```sh
export NIX_CONFIG="experimental-features = flakes nix-command dynamic-derivations ca-derivations recursive-nix"
```

Verify by running:
```
$ nix config show | grep experimental-features
experimental-features = ca-derivations dynamic-derivations fetch-tree flakes nix-command recursive-nix
```

Then you can try building the examples:

```sh
# Builds a basic main.cpp.
nix build github:pdtpartners/nix-ninja#example-hello

# Builds a basic main.cpp with dependency inference for its header.
nix build github:pdtpartners/nix-ninja#example-header

# Builds Nix 2.27.1.
nix build github:pdtpartners/nix-ninja#example-nix
```

You can also try running `nix-ninja` outside of Nix, but you'll need both
`nix-ninja` and `nix-ninja-task` to be in your `$PATH`. Make sure
`nix-ninja-task` is from the `/nix/store` as it is needed inside derivations
`nix-ninja` generates.

```sh
export NIX_NINJA=$(nix build --print-out-paths)
export PATH="${NIX_NINJA}/bin:$PATH"
# Meson respects this environment variable and uses it as if its ninja.
export NINJA="${NIX_NINJA}/bin/nix-ninja"
```

Then you can go to an example and run it like so:
```sh
$ nix-shell
$ cd examples/hello
$ meson setup build
$ cd build
$ meson compile hello
$ ./hello
Hello Nix dynamic derivations!
```

## Contributing

We still have major TODOs, so would appreciate any help. We've organize them
under two [GitHub milestones][milestones]:

- `0.1.0` - The first release of `nix-ninja` aiming for correctness.
- `0.2.0` - Major performance features to make incremental builds productive.

Regardless, pull requests are welcome for any changes. Consider opening an issue
to discuss larger changes first, especially when the design space is large.

Please read [CONTRIBUTING](CONTRIBUTING.md) and the [design notes] so you
understand the big picture and prior art.

## Where this fork stands

The milestones above are upstream's, and the state below is this fork's
against them. Upstream cannot close an issue on work that lives here, so
their tracker reads as though less has been done than has.

`0.1.0` is complete here, including the two issues still listed open there. A
CMake project builds end to end through the driver, and phony targets are
supported past the original scope: aliases expand transitively, a phony over
no file is tolerated the way a header-only CMake project emits it, and a
source file that the generator also marks as a phony output still reaches the
task that needs it.

`0.2.0` is complete. The benchmarks exist for both generation and end-to-end
compilation, and a configure step can be cached in its own derivation rather
than re-run per build. A task's depfile is a declared output and the driver
reads that list back, so a compiled translation unit reports the headers it
actually opened instead of the ones a scanner inferred.

The last of the five asked for the store-add path to come off the critical
path, on the premise that generating derivations is slowed by adding almost
every input file to the store one at a time. That premise was measured rather
than acted on, and it does not hold. On a 345 task project the driver makes
36,499 store adds, and the 35,435 of them answered from the stamp cache cost
under a second between them, because that path stats the file and reads an
in-process map without ever reaching the daemon. Every measurable second
belongs to the thousand or so files that genuinely upload, which already
stream and already upload concurrently, and which the stamp cache avoids
repeating on a later run in the same build directory. What that residue
costs depends on the state of the store rather than on the shape of the call
(25 s cold against 4 s warm, at the same number of calls), which is the
reason the split matters and a total does not: moving the cheap majority off
the critical path would buy the second they cost between them.

Beyond that there are no upstream goalposts, and most of what this fork has
built already sits outside them. Driving real packages surfaced failure
classes that no roadmap anticipated: headers a package stages into its own
output, soname aliases whose targets never reach the sandbox, generated files
adopted as sources by a later pass, linker scripts written by one edge and
named by another, and manifests that declare their inputs in their own
markup. Each is closed against a gate that fails on the revision before its
fix, and the gates run as two corpora, one outside any derivation and one
inside an ordinary recursive-nix build, since a verification is a claim about
the configuration it was taken on.

The direction from here is the cost of a change rather than its correctness.
Every task derivation is a cache key, so an edit to the driver is free only
when it emits the same bytes for the work it was not aimed at, and that is
measured before anything lands rather than argued.

## Directories this fork adds

Four directories here do not exist in `pdtpartners/nix-ninja`, and each is
either an offer prepared for it or the tooling that produced one.

- `vendor-n2/` is n2, by Evan Martin, vendored rather than depended on. It
  carries its own Apache-2.0 `LICENSE`, and `vendor-n2/VENDORING.md` records
  the revision it was taken from and the four changes made to it, all of
  which are prepared to be offered back rather than kept.
- `bench/` is the end-to-end and generation benchmarks, answering the
  benchmarking requests in the issue tracker, plus the records they wrote.
- `contrib/` is a throwaway-store development script, which `CONTRIBUTING.md`
  asks contributors for.
- `scripts/` holds one tracked file, a standalone reproducer for a daemon
  wedge, prepared for a `NixOS/nix` issue. Everything else under that name is
  local to this checkout and deliberately untracked.

The MIT license below covers the source developed for nix-ninja. It does not
cover `vendor-n2/`, which is Apache-2.0 and carries its own license file.

## License

The source code developed for nix-ninja is licensed under MIT License.

[design notes]: docs/design.md
[dynamic-derivations]: docs/dynamic-derivations.md
[milestones]: https://github.com/pdtpartners/nix-ninja/milestones
[ninja-build]: https://ninja-build.org/
