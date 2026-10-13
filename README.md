<div align="center">

# nix-ninja

**Every translation unit is a content-addressed Nix derivation.** A killed build
costs one object. Editing one `.cpp` rebuilds one object. A binary cache
substitutes the rest, on any machine, for any user.

Incremental compilation of [Ninja build files][ninja-build] using
[Nix Dynamic Derivations][dynamic-derivations]. Choosing ninja as the build
graph representation lets us support any build system that outputs ninja like
CMake, meson, premake, gn, etc.

[![Demo](docs/demo.gif)](https://asciinema.org/a/711344)

*An incremental rebuild driven by nix-ninja. The recording is the project's
original demonstration and shows an earlier layout;
`nix build .#legacyPackages.x86_64-linux.example-hello` is the current
equivalent.*

[![CI](https://github.com/jdmanring/nix-ninja/actions/workflows/ci.yml/badge.svg)](https://github.com/jdmanring/nix-ninja/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/jdmanring/nix-ninja)](https://github.com/jdmanring/nix-ninja/releases)

[Key features](#key-features) •
[Getting started](#getting-started) •
[Roadmap](docs/ROADMAP.md) •
[Design notes][design notes] •
[Ask a question](https://github.com/jdmanring/nix-ninja/discussions) •
[Contributing](CONTRIBUTING.md)

</div>

## Key features

> [!IMPORTANT]
> nix-ninja depends on experimental features from an unreleased version of Nix.
> The tree is under active development and the roadmap is in
> [docs/ROADMAP.md](docs/ROADMAP.md).

> [!WARNING]
> macOS: there is no macOS attribute to build. This flake declares
> `systems = [ "x86_64-linux" ]`, so `nix build .#example-hello` on a Mac fails
> with `does not provide attribute 'packages.aarch64-darwin.example-hello'`, or
> the `x86_64-darwin` form on an Intel Mac.
> See [multi-arch support](docs/ROADMAP.md#the-next-concrete-work-item-by-item),
> item 5, and [the destination](docs/ROADMAP.md#06-the-destination) for what is
> deliberately outside the scope.
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

These features are unreleased. `dynamic-derivations` and `ca-derivations` are
experimental and their interface moves, so the tree is developed against Nix
master, which is what its `nix` flake input pins; read the revision from
`flake.lock` rather than from a release number. The inherited README named 2.30
as a floor, which was never re-verified here and is best read as a lower bound.
Enable the following experimental features:

```sh
export NIX_CONFIG="experimental-features = flakes nix-command dynamic-derivations ca-derivations recursive-nix"
```

Verify by running:
```
$ nix config show | grep experimental-features
experimental-features = ca-derivations dynamic-derivations fetch-tree flakes nix-command recursive-nix
```

Then you can try building the examples. The examples are dynamic derivations,
so they are exposed under `legacyPackages` and only instantiate when
`dynamic-derivations` is enabled:

```sh
# Builds a basic main.cpp.
nix build .#legacyPackages.x86_64-linux.example-hello

# Builds a basic main.cpp with dependency inference for its header.
nix build .#legacyPackages.x86_64-linux.example-header

# The CMake route, end to end.
nix build .#legacyPackages.x86_64-linux.example-cmake-hello
```

`example-nix`, which builds Nix itself per translation unit, is the largest
example and is currently KNOWN NOT TO BUILD: it aborts on a daemon refusal to
submit a duplicate output. That is reproducible at revisions before any recent
change and is not a property of the current tree. See
[docs/ROADMAP.md](docs/ROADMAP.md).

If a build fails with `does not provide attribute ... example-hello`, the
experimental features are not enabled in the DAEMON's configuration; the
`legacyPackages` set is empty without them.

Without cloning, every attribute works through the flake URL
(`nix build github:jdmanring/nix-ninja#legacyPackages.x86_64-linux.example-hello`);
the clone form is used throughout because it is what a contributor runs.

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

Point it at any project that emits a ninja file. The examples live under
`modules/flake/examples/`. A few are ORDINARY derivations rather than dynamic
ones, so they sit under `packages` and build without the feature-gated set:

```sh
nix build .#example-fortran-module-recursive
nix build .#example-dotdot-source-include
```

See `modules/flake/overlays.nix` for the full set: `example-hello`,
`example-header`, `example-multi-source`, `example-shared-lib`,
`example-generated-header`, `example-cmake-hello`, `example-nix`, and the
Fortran and C++20 module examples that need `-recursive` variants.

## Contributing

The work is tracked in [docs/ROADMAP.md](docs/ROADMAP.md), which states each
milestone's exit criterion and the gate that says whether it is met. Bugs and
questions have their own channels: [open an
issue](https://github.com/jdmanring/nix-ninja/issues/new/choose) for a failure,
with the failing derivation and its leaf log, or start a
[discussion](https://github.com/jdmanring/nix-ninja/discussions) for a question
or a wide design space before writing code.

Pull requests are welcome and reviewed. Please read [CONTRIBUTING](CONTRIBUTING.md)
and the [design notes] so you understand the big picture and prior art.

## What works today

`0.1` and `0.2`, the two milestones this project inherited, are complete.

`0.1` - correctness. A CMake project builds end to end through the driver, and
phony targets are supported past the original scope: aliases expand
transitively, a phony over no file is tolerated the way a header-only CMake
project emits it, and a source file that the generator also marks as a phony
output still reaches the task that needs it.

`0.2` - performance. The benchmarks exist for both generation and end-to-end
compilation, and a configure step can be cached in its own derivation rather
than re-run per build. A task's depfile is a declared output and the driver
reads that list back, so a compiled translation unit reports the headers it
actually opened instead of the ones a scanner inferred.

The last of the five issues asked for the store-add path to come off the
critical path, since generating derivations meant adding almost every input file
to the store one at a time. Uploads now run concurrently against a connection
pool rather than serially, a NAR streams instead of being built in memory, and a
stamp map keyed on path, length and mtime skips a file already known, persisting
across runs in the same build directory.

The instrument that says what those are worth arrived after them, and the split
it measures is in `bench/records/`, alongside the run it came from. On a 345 task
project the driver offers 35,569 files to the store and the stamp map answers
34,412 of them, because that path stats the file and reads an in-process map
without ever reaching the daemon. The measurable time belongs to the 1,157 files
that genuinely upload, and what they cost depends on the state of the store
rather than on the shape of the call.

The figures that stand behind the store-add close, measured on the same project
in September, are a distinct-call reading of the same runs: 35,435 store adds,
34,371 answered from the cache, 1,064 uploaded. They are stated in the commit
that corrected them, `7540642`, and not in a record here, so they are checkable
but not reproducible from this repository.

That split is worth more than the total it comes from, because it says where
further effort would go and where it would not. Roughly 97 percent of the calls
are already a memo lookup, so moving them off the critical path buys the second
they cost between them; the residue is real uploads that have to happen once
each, and they already stream and already run concurrently.

## Beyond the inherited milestones

Most of what this project has built sits outside the two milestones it
inherited. Driving real packages surfaced failure classes that no roadmap
anticipated: headers a package stages into its own output, soname aliases whose
targets never reach the sandbox, generated files adopted as sources by a later
pass, linker scripts written by one edge and named by another, and manifests that
declare their inputs in their own markup. Each is closed against a gate that
fails on the revision before its fix, and the gates run as two corpora, one
outside any derivation and one inside an ordinary recursive-nix build, since a
verification is a claim about the configuration it was taken on.

The direction from here is the cost of a change rather than its correctness.
Every task derivation is a cache key, so an edit to the driver is free only when
it emits the same bytes for the work it was not aimed at, and that is measured
before anything lands rather than argued.

Both binaries sit in every task derivation, as the builder string and again
among the inputs, while neither is an ingredient of the result: the same compile
driven at two revisions of the task binary yields the same object byte for byte.
Three environment variables key a task on a declared generation instead, the way
`sandbox-paths` already maps `/bin/sh` so that a busybox upgrade re-keys nothing.
A fix to either binary then costs no rebuild unless the generation moves, which
is the whole point: most fixes here repair a task that failed, and a task that
failed banked no output to invalidate. Unset, every emitted derivation is
unchanged. Set, they need a matching sandbox path entry in the daemon's own
configuration, and a builder path given without a generation, or spelled without
a leading slash, is refused rather than allowed to key on a generation that does
not exist. The contract is in the [design notes].

## Layout

- `crates/nix-ninja` is the driver. It reads a `build.ninja`, turns each edge
  into a derivation, and drives the build.
- `crates/nix-ninja-task` is the builder of those derivations. It materialises
  inputs, runs the edge's command, and places outputs.
- `crates/deps-infer` infers the headers a translation unit opens, which Nix
  needs declared upfront.
- `crates/nix-builder-rpc-client` speaks the daemon's `builder-rpc-v0` protocol.
- `vendor-n2/` is n2, by Evan Martin, vendored rather than depended on. It
  carries its own Apache-2.0 `LICENSE`, and `vendor-n2/VENDORING.md` records the
  revision it was taken from and the four changes made to it.
- `bench/` holds the end-to-end and generation benchmarks, and the records they
  wrote. The measured figures this README quotes trace to a run under
  `bench/records/`, whose README names what each record is and which figures it
  cannot account for. Not every number in the project does: the roadmap's two
  scaling slopes and its 30,000-unit projection come from distribution builds
  recorded in `docs/todo.md`, which is a development note rather than a bench
  record.
- `contrib/` holds a throwaway-store development script, which `CONTRIBUTING.md`
  describes.

The MIT license below covers the source developed for nix-ninja. It does not
cover `vendor-n2/`, which is Apache-2.0 and carries its own license file.

## Origins

nix-ninja was started by [PDT Partners][pdt] as
[pdtpartners/nix-ninja][upstream], and this repository continues it. The
original design, the `0.1` and `0.2` milestones, and the vendored n2 are theirs,
and their MIT license is retained in `LICENSE` below.

Upstream's last commit was in May 2026 and development has since moved here,
where it is active. The lineage matters for the parts that were inherited; the
roadmap and everything after `0.2` are this project's.

## License

The source code developed for nix-ninja is licensed under MIT License.

[design notes]: docs/design.md
[dynamic-derivations]: docs/dynamic-derivations.md
[pdt]: https://github.com/pdtpartners
[upstream]: https://github.com/pdtpartners/nix-ninja
[ninja-build]: https://ninja-build.org/
