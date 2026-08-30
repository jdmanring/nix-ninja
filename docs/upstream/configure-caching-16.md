# #16, `mkMesonPackage` configure caching: built, verified, and it does not
# pay for the reason the issue implies

Status: implemented behind `useConfigureCache`, default OFF. Correctness
verified on a package. The caching claim was MEASURED and does not hold as
designed. This file is the evidence, and the measurement is worth more to the
issue than the code is.

## What was built

`mkMesonPackage` grows one optional argument. Passing it moves `meson setup`
into an ordinary derivation with a real `$out` - no `builder-rpc-v0`, no
dynamic derivations, substitutable by any machine - whose output is the
configured build directory. The ninja derivation then replaces its configure
phase with a copy of that tree.

Opting out leaves `args'` without the attribute and the emitted derivation
byte-identical. Verified by `drvPath`, both sides:

    .#example-hello -> /1bls0sny58r0jbihl5w0vfi9b8iq137g9j293p5x13dl1pxm47xc

So it lands without re-keying anything, which is why it could land at all.

`example-hello-cached-configure` is the subject: same sources as
`example-hello`, configured through the split. It builds, links, and the
binary it produces is **byte-identical** to the one the ordinary path
produces (`cmp`, 2026-08-30). A source edit propagates - the edited binary
prints the edited string - so nothing is stale-cached.

## The first thing it taught us, which is not in the issue

**meson will not configure without a ninja it can version-probe:**

    ERROR: Could not detect Ninja v1.8.2 or newer

and on this path the binary answering that probe is nix-ninja itself, which
reports `1.8.2` for exactly this reason. So `nix-ninja` is a CONFIGURE-time
dependency, and splitting configure into its own derivation does not decouple
configure from the driver. The driver's store path is an input of the
configure derivation too.

That bounds what the cache can buy before any measurement: it cannot survive a
driver bump.

## The measurement, which is the point of this file

Edit one source file, rebuild, ask whether the configure derivation was
reused:

    $ sed -i 's/Hello dynamic derivations!/Hello dynamic derivations, edited!/' \
        modules/flake/examples/hello/main.cpp
    $ nix build .#example-hello-cached-configure
    building '...-example-hello-cached-configure-configure.drv'...

**It rebuilt.** `src` is the whole source tree and it is an input of the
configure derivation, so any edit anywhere in the tree re-runs `meson setup` -
which is precisely the case the cache exists to serve.

Put the two together and the split, as the issue describes it, buys nothing on
the common path. Configure already runs once per package build; making it its
own derivation does not reduce that count. It only helps when the configure
inputs are unchanged and something else moved, and both the source tree and
the driver are configure inputs.

## What would make it pay, and what each would cost

Neither is free, and we are not proposing one over the other without the
maintainer's view.

1. **Narrow the configure derivation's source.** `meson setup` reads
   `meson.build`, not `main.cpp`. Filtering the configure input to the build
   definition would make the cache survive source edits, which is the whole
   win. It is not sound in general: `meson.build` can test for a file's
   existence, glob, or read a version out of the tree, and the configure
   output records the source list. A filter that is wrong produces a
   configured tree that disagrees with the sources, which fails late and
   confusingly.
2. **Take the ninja probe off nix-ninja.** nixpkgs' own `ninja` can answer the
   version probe; it is never executed, because the build phase is
   `nix-ninja <target>`. That decouples the configure output from driver
   bumps, which matters a great deal here - a driver bump re-keys every task
   derivation, and keeping configure cached across one is real. The risk is
   that meson records the ninja command in `build.ninja`'s regeneration rule,
   so the graph the driver parses would name a different binary. Unmeasured.

## The other win, which is not caching at all

`docs/todo.md` records a second reason to want this issue's shape, and it
survives the measurement above: `Runner::read_build_dir` calls
`new_opaque_file` PER FILE, so every configured artefact is its own NAR upload
and its own map entry. One directory output would replace N uploads with one
for every package. On `example-nix` that is 32 build-dir inputs; on a large
graph it is not.

That is a driver-side change and independent of whether the configure
derivation is cached, which is worth separating in the issue: the upload win
is available without the caching win, and it is the one with a measured
motivation.

## What is NOT established

One package, and the smallest one in the tree. `example-hello` has no
generated sources, no configure-time codegen, and no subprojects - the cases
most likely to break a split configure are all absent from it. Nothing here
says the split survives a real package; it says the mechanism works and that
the caching benefit is not where the issue puts it.
