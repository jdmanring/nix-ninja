This file provides guidance to Claude Code (claude.ai/code) when working with
code in this repository.

## What this is

`nix-ninja` emits **one Nix derivation per translation unit**. The Nix store
resumes at derivation granularity, so an ordinary build of qtwebengine is one
derivation: a kill at hour three costs three hours, every time. Driven by
nix-ninja the same kill costs one object file, a binary cache holds object
files another machine can substitute, and editing one `.cpp` rebuilds one TU.

This tree is **James Manring's fork of `pdtpartners/nix-ninja`**, and it is
developed **for upstream** - the goal is contribution, not permanent
divergence. It is the only place nix-ninja work happens. The consumer is
ArtNix (`~/Projects/ArtNix`), a distribution that builds every package on one
workstation, which is why per-TU resumability is load-bearing rather than a
nicety.

It was called `nix-ninja-upstream` until 2026-08-29, which read as a read-only
mirror and was wrong in the direction that stops people committing.

## Remotes, and who may push where

    origin     git@github.com:jdmanring/nix-ninja.git     push freely
    upstream   https://github.com/pdtpartners/nix-ninja   FETCH ONLY

`upstream`'s push url is deliberately invalid. **James files upstream; sessions
stage.** Never push to `upstream`, never open a PR against it, never file an
issue there. Staged contributions live in `docs/upstream/` and are James's to
send.

Default branch is `main`. Commits carry **no AI attribution** - no
`Co-Authored-By`, no `Generated with`. James is the sole author.

## THE ONE THING TO UNDERSTAND: what an edit costs

Every change here falls into one of two classes, and the expensive one does not
announce itself. ArtNix has hundreds of thousands of banked per-TU outputs; the
expensive class invalidates all of them.

**A change is FREE if it moves neither the task binary nor what the driver
emits.** Two independent tests, and passing the first is not enough:

1. **The fileset test.** `nix-ninja-task`'s `src` in `modules/flake/overlays.nix`
   is an explicit allowlist: `Cargo.{toml,lock}`, `crates/nix-{libstore,ninja-task}`,
   `crates/deps-infer`, `vendor-n2`. An edit inside it re-keys the task binary,
   which is the builder of every PLAIN task derivation (`task.rs:2139`, and
   `tools.nix_ninja_task` is among its inputs at `:2255`). `crates/nix-ninja` -
   the driver - is NOT in the allowlist.
   **The driver is nevertheless the builder of every DYNAMIC task derivation**,
   and reading only the sentence above will price that edit wrong.
   `build_dynamic_task_derivation` sets the builder to
   `${nix_ninja}/bin/nix-ninja` at `task.rs:2815` and inserts
   `tools.nix_ninja` into `drv.inputs` at `:2820`, so the driver's own store
   path is IN the key of every derivation for an edge with dynamic
   dependencies - and of the final task derivations those emit. A pure
   refactor of `subtool/dynamic_task.rs`, touching nothing about what is
   emitted, still re-keys that whole subset. Free means free of BOTH binaries'
   store paths, not only the task binary's.
   This applies to TESTS too, and a test reads as costless, which is why nobody
   prices it. A regression test added under `crates/deps-infer` moved
   `nix-ninja-task` from `2ai4n9fh` to `da3j2y88` on 2026-08-24, spending every
   banked output to buy coverage that never runs inside a build. Moved to
   `crates/nix-ninja`, reaching the same `pub` function, the hash returned.
   Read that last sentence with the paragraph above it: `nix-ninja-task`'s hash
   returned. `nix-ninja`'s did not, and it is the builder of every dynamic task,
   so the move bought back the plain task derivations and not the dynamic ones.
   For a package with no generated sources that is the whole bank; for a
   codegen-heavy one it is not. There is no location in this tree where a test
   is free of both binaries.

2. **The emission test, which the fileset test cannot see.** A driver-only edit
   is free ONLY if it does not change what the driver EMITS. Anything touching
   `drv.inputs`, the emitted `$PATH`, `passAsFile`, the environment, or the
   discovered dependency set re-keys every emitted derivation even though the
   driver's own hash is irrelevant to them.
   Both commits recovered on 2026-08-29 (`2f7b8f2`, `dc296f3`) live in
   `crates/nix-ninja` and are emission-side: they add `bash` to `drv.inputs`
   and change `virtual_paths`. Reading only the fileset test prices them as
   free, and that reading is wrong.

**Verify by `drvPath`, never by reasoning about what the edit affects:**

    nix eval --impure --max-jobs 0 --raw --expr \
      '(builtins.getFlake "/home/james/Projects/ArtNix").inputs.nix-ninja \
       .packages.x86_64-linux.nix-ninja-task.drvPath'

Take it either side of the change. For the emission side, drive
`example-hello` before and after and compare the emitted task drvPaths - the
fork's examples exist for this.

**A re-key costs nothing until something is BUILT.** The bill arrives once, at
the first build, and it is the same bill whether one thing changed or ten. So
batch every pending re-keying change and pay once. Landing them one at a time
pays it once per change, which is how this project has lost progress before.

**Corollary: do not garbage collect while a re-key is unproven.** The outputs
at the old hash stop being referenced the moment hashes move, which makes them
look like garbage. They are the fallback.

## How ArtNix consumes this

By **revision, from GitHub** - `inputs.nix-ninja` in ArtNix's `flake.nix`
names `github:jdmanring/nix-ninja/<rev>`. Never a `path:` input, and that is
not tidiness:

- A `path:` input is consumed by CONTENT, so an uncommitted edit in this tree
  is in every build with nothing recording it. That happened twice in August
  2026 and re-keyed the closure both times, unattributed.
- It copies the WHOLE directory. `.gitignore` is a git mechanism and nix does
  not consult it, so `target/` rides into the store beside the source.
- A path under `/home/james` cannot be built by anyone else, and ArtNix is
  headed for public release.

So the loop is: **commit here, push to `origin`, then bump the rev in ArtNix's
flake.** A change that has not been pushed does not exist to the consumer.

## Cargo's build directory is redirected, deliberately

`.cargo/config.toml` sends `target-dir` to `~/.cache/cargo-target/nix-ninja`,
and the ignore lives in `.git/info/exclude` so this fork carries ZERO
divergence from upstream for a purely local choice.

Do not build in a way that bypasses it. On 2026-08-28 something did, leaving
1.7 GiB of in-tree `target/`. Under the `path:` input then in force, that was
being copied into the store on every hash-changing edit - the very defect the
redirect exists to prevent.

## Open failure classes

Four packages ArtNix cannot drive through nix-ninja. Traced 2026-08-28 in
`docs/incidents/2026-08-28-four-failure-classes.md`. Code has since landed for
three of them and NONE of it has been verified on a package, which is a
different state from fixed; the per-class notes below say which commit and what
is still owed. Read the verification rule at the end of this section before
reporting any of them as done.

1. **bison** - `installCheckPhase` recompiles through the `cc` shim, producing
   objects unlike the originals: 698 test failures. Fix is a passthrough hook
   on ArtNix's side, written and never deployed. Cheapest of the four.
   Latent hazard beside it: a package setting `installCheckPhase` as a STRING
   bypasses the default function, so `runHook preInstallCheck` never fires and
   the passthrough is silently skipped.
2. **hicolor-icon-theme** - `Tools::new()` resolves `cc` eagerly at startup, so
   a package with zero compile targets dies on `cc: command not found`. Lazy
   resolution landed in `7fb756e`; hicolor-icon-theme has not been driven
   through it.
3. **libvmaf** - the include scanner reads `#include "vcs_version.h"` off disk,
   but meson's `vcs_tag()` generates it during the build. Compile tasks are
   also blanket-excluded from `build_dir_inputs`.
4. **liblapack** - CMake's FortranCInterface verify step, same class as 3:
   `VerifyFortran.h` is generated into the build directory and invisible to the
   task derivation.

3 and 4 share a fix and should be ONE change, so the closure re-keys once.
That change is the `virtual_paths` map for generated headers, recovered from
dangling commits on 2026-08-29 (`2f7b8f2`, `dc296f3`, live at
`crates/nix-ninja/src/task.rs:2299`). Neither package has been built with it.
`docs/opt-out-triage.md` reads dav1d, p11-kit and valgrind as the same class,
so testing one small package here is the cheapest work in the tree: it can
retire up to five opt-outs with no new code.

**Verify every fix on a single small package before it reaches a closure.**
Each of these has been "fixed" at least once without that, and one was reported
as done while never having been deployed at all.

## Working rules that cost something to learn

- **Never bypass a package to make a build pass.** An opt-out means nix-ninja
  does not drive it, so its objects are not per-TU resumable, which is the
  entire reason this tool exists. An opt-out entry needs a named defect and a
  route to removing it.
- **The bootstrap is the one real exclusion**: nix-ninja is written in Rust, so
  nothing Rust itself is built from can be built by it. That constrains WHICH
  packages, and is never a reason to run a world build the ordinary way.
- **Say what a build will cost before starting it, and say when it has
  started.** Silence during a several-hundred-package rebuild is what has
  destroyed progress here.
- `cargo check -p nix-ninja` and `-p nix-ninja-task` are seconds against the
  warm cache. Compiling to confirm something a `drvPath` already answered is
  not real work.

## How the pieces fit

Two binaries, and the split between them is the same split as the cost model
above.

- **`crates/nix-ninja`** is the DRIVER. It parses `build.ninja` through
  `vendor-n2` (a vendored fork of evmar's n2, consumed as a library), and
  `task.rs` turns every edge into a derivation, which is why that file is what
  the emission test is really about. The build goes through
  `nix-builder-rpc-client`, not a `nix build` subprocess; `NIX_NINJA_DRV`
  (`cli.rs`, `is_output_derivation`) only decides what happens to the result:
  submit the one output derivation back to the daemon when running inside a
  derivation, or symlink the built paths into the build directory
  (`local.rs`). `dyndep.rs` and `resolve_cache.rs` handle ninja's dyndep and
  the resolution memo.
- **`nix-ninja -t dynamic-task`** (`subtool/dynamic_task.rs`) is the second
  half, and it is not something you invoke: `task.rs` emits derivations whose
  builder IS this subtool, so each task re-reads its own drv inside the
  sandbox, discovers dependencies, and submits the updated derivation. That
  is where per-TU dynamic dependencies actually happen.
- **`crates/nix-ninja-task`** is the BUILDER of every emitted derivation. It
  runs inside each per-TU derivation, materialises inputs, runs the command,
  and patches rpaths (`patchelf.rs`) because shared libraries built by sibling
  dynamic derivations are not on any normal search path.
- **`crates/deps-infer`** scans C/C++ sources for includes so a compile task
  can declare its real inputs. Failure classes 3 and 4 in the list above both
  live here in substance: it reads headers off disk, and generated headers are
  not there yet.
- **`crates/nix-builder-rpc-client`** speaks `builder-rpc-v0` to the daemon,
  which is how a derivation adds derivations to the store and submits its own
  output. This is the crate the daemon precondition below is about.

Packages enter through `modules/flake/pkgs/mkMesonPackage` and `mkCMakePackage`:
they set `NINJA=nix-ninja`, `NIX_NINJA_DRV=true`, ask for the
`builder-rpc-v0` system feature, and expose the real result as
`passthru.target = builtins.outputOf ninjaDrv.outPath <target>`. That is why
`packages` holds only the two binaries while every `example-*` lives under
`legacyPackages`, gated on `builtins ? outputOf`. ArtNix consumes the same two
helpers, so an edit to either is emission-side.

`modules/flake/examples/` is the emission-side test bed the cost model asks
you to use: `example-hello` for the smallest possible emission diff,
`example-header` and `example-dynamic-deps` for dependency inference,
`example-nix` for a real build.

## Commands

    cargo check -p nix-ninja          # seconds against the warm cache
    cargo check -p nix-ninja-task
    cargo test -p nix-ninja           # 80 unit tests, all inline
    cargo test -p nix-ninja --test include_dir_shadow   # one test FILE
    cargo test -p deps-infer c_include_parser::         # one module's tests
    nix flake check                   # clippy -D warnings, rustfmt, taplo,
                                      # cargo-audit, cargo-deny, nextest
    nix develop                       # meson, taplo, just, agg, gnumake

The devShell ships `just`, but there is no justfile in the tree.

**Before `cargo test`, read the fileset test above.** Tests under
`crates/deps-infer`, `crates/nix-ninja-task` or `vendor-n2` are inside the
task binary's allowlist and re-key it; the same test reaching the same `pub`
function from `crates/nix-ninja` is free. This is not hypothetical, it is the
2026-08-24 incident.

Driving a single example end to end needs the experimental features on the
invocation, not a machine-wide edit:

    nix build --extra-experimental-features \
      'nix-command flakes dynamic-derivations ca-derivations recursive-nix' \
      .#example-hello

## Preconditions on the workstation

- The daemon must implement `builder-rpc-v0`, which the dynamic task requires
  by name. In nix 2.36.0pre, NOT in 2.35.2, and NOT grantable through
  `system-features`. `nix store info` reports the DAEMON's version;
  `nix --version` reports whatever binary is on PATH.
- `dynamic-derivations`, `ca-derivations` and `recursive-nix` must be in
  `experimental-features`. That setting REPLACES rather than extends, so a
  user-level `~/.config/nix/nix.conf` is the whole list for the client; reach
  for `--extra-experimental-features` on the invocation rather than editing a
  machine-wide file to serve one experiment.

## Where things are

| subject | file |
|---|---|
| the design, and why dynamic derivations | `docs/design.md` |
| dynamic dependencies, and generated sources | `docs/dynamic-deps.md` |
| ninja `dyndep`, Fortran `.mod` and C++20 modules | the header comment of `crates/nix-ninja/src/dyndep.rs`, which is where that subject is written down; `docs/` has nothing on it |
| the daemon wedge: incident and failed reproduction | `docs/daemon-wedge.md` |
| what is staged for upstream, and its plan | `docs/upstream/` |
| incidents, per date | `docs/incidents/` |
| ArtNix's thirteen opt-outs, in six families | `docs/opt-out-triage.md` |
| dynamic derivations, from the beginning | `docs/dynamic-derivations.md` |
| what is next | `docs/todo.md` |
| commit and prose conventions | `docs/conventions.md` |

ArtNix's own record of this work, including the 2026-08-29 recovery and the
consolidation it belongs to, is `~/Projects/ArtNix/docs/nix-ninja-recovery.md`.
