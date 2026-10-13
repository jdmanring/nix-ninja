# Roadmap

The path from a driver that builds one project correctly to one that can be
pointed at a distribution. Milestones, the exit criterion for each, the work
that satisfies it, and the instrument that says whether it is met. No dates: a
milestone ships when its criterion is met and the named gate says so on a clean
tree.

`0.1` and `0.2` are upstream's; they came with the project and both are
complete. From `0.3` on, the numbering is this project's own, and is written
here rather than proposed: it records how the work since has been organized, not
a plan awaiting ratification.

The tree is at **0.3.0** (`Cargo.toml`). The version is a milestone, so it moves
when a criterion below is met and a gate says so, never because time passed.

The state of the tree TODAY, class by class, is a table of 88 rows kept in the
development notes. This file is the ladder behind it. It exists because the two
milestones the project inherited stopped at two, and six months of work
followed that no number named.

## Why this file is long, and what it is for

The project began when `pdtpartners/nix-ninja` stopped. Work continued here: the
tree is several hundred commits past the last upstream commit of May 2026, and
the milestone numbering above two is new. `docs/todo.md` records the state of
the issues that came with the project; this file is what comes after them.

## 0.1 CORRECTNESS (upstream)

A CMake project builds end to end through the driver. Phony targets are
supported past the original scope: aliases expand transitively, a phony over no
file is tolerated, and a source file the generator also marks as a phony output
still reaches the task that needs it. MET.

## 0.2 PERFORMANCE (upstream)

Benchmarks exist for generation and end-to-end compilation; a configure step is
cached in its own derivation; a task's depfile is a declared output the driver
reads back. The store-add path left the critical path: uploads run concurrently,
a NAR streams, and a stamp map skips a file already known. MET, with its
measurement in `CHANGELOG.md`.

## 0.3 REAL-PACKAGE RESILIENCE

THE CLAIM: the driver builds packages it was not written against, and every
failure becomes a class closed against an instrument that fails on the revision
before its fix.

This is where the project's work actually sits. Driving real packages is what
generated it. The classes fall into families, by what the driver or the task
binary had to learn:

- **Build-graph and target resolution.** What ninja's own graph says: scope,
  phony targets, the build-dir walk, and which pass owns a placed product.
- **Include and header resolution.** What the compiler reads, found from the
  command: generated headers, PCH, sibling-directory prefixes, macro arms that
  resolve to a file.
- **Undeclared reads.** A file a COMMAND reads and no edge declares, so the
  sandbox stages nothing. The largest family, and the one still growing:
  `cmake -P` scripts and the modules they include, XInclude, TableGen, rst
  includes, msgfmt ITS, swig `%include`, the `g-ir-scanner` filelist, protoc
  imports, python `-m`.
- **Output placement and aliases.** Getting a produced artifact to the path the
  build expects: co-outputs, symlinks stored as link text, `DT_NEEDED`, a
  byproduct nobody declared, one path claimed by two spellings.
- **ExternalProject.** A build system inside a build system, whose steps write
  trees no edge declares. Seven walls, each found by the next step failing.
- **Outer-output machinery.** An output declared inside the outer output is
  staged and restored, and the placeholder must not survive into a real output.
- **Task-binary walls.** A directory both received and declared, build-dir
  deepening, four encoded lists each unbounded in the size of the package, the
  store-name cap.
- **Emitted PATH and environment.** What a task must be able to exec: a tool
  named by bare name, the compiler for an unnamed `dtrace`, and the variables
  forwarded into the environment.
- **Compiler and linker resolution.** Which compiler, which flags, whether an
  edge is a compile or a link, and the rpath a link carries.

Every one is closed against an instrument that fails on the revision before its
fix, and most of those instruments are covered by a test in this repository. The
trap that recurs: a reader emitting an absolute spelling is dropped in silence
by the wiring filters, so it looks fixed and changes nothing.

EXIT CRITERION: the corpus passes on both configurations, and every class
carries a gate arm, a named reason it cannot, or a witness on a real package.
THE THIRD FORM IS NOT THE FIRST, and the rows in it are the conversion work
still owed: a package witness is evidence about the package, while an arm is
evidence about the revision before the fix, which is what survives the next
change.

## 0.4 THE COST OF A CHANGE

THE CLAIM: an edit that does not change what the driver emits does not re-key
the outputs of work already done.

Everything in a nix-ninja build is a cache key, so the question stopped being
"is it correct" and became "what does a change cost". Two axes, and both are
measured before anything lands:

- **The fileset axis.** An edit inside the task fileset moves the task binary,
  which is the builder of every plain task derivation, and re-keys every banked
  compile. An edit to the driver alone moves the dynamic derivations, which
  embed the plain one.
- **The emission axis.** A driver-only edit is free only if it does not change
  what the driver emits: `drv.inputs`, the emitted `$PATH`, `passAsFile`, the
  forwarded environment, the discovered input set. Five shapes are priced
  because one cannot answer it: a plain compile, a `deps = gcc` compile, a link,
  a generator and an app.

Both binaries sit in every task derivation as the builder string and again among
the inputs, while neither is an ingredient of the result. A knob takes both out
by mapping them to stable mount paths and keying on a declared generation
instead, as nix maps `/bin/sh` through `sandbox-paths`. Unset, every emitted
derivation is unchanged.

EXIT CRITERION: a rebuild or rename of either binary costs the banked outputs
nothing while its generation is unchanged, and a change that alters what a task
WRITES moves the generation and pays once, deliberately.

## 0.5 VERIFICATION

THE CLAIM: a change that breaks a closed class is caught before it lands, by
something a reader of this repository can run.

What you can run today, from a clone, with nothing else installed:

    nix flake check

That runs the Rust test suite under `cargo-nextest`, the format, clippy and
dependency checks, and ten NixOS VM tests that exercise dynamic-derivation
builds end to end, one per example plus the generated-header, CMake order-only
and Fortran-module shapes. Each failure class that is unit-testable has a test
here, and those tests are the ones that survive a refactor.

The counts, measured on a clean tree rather than stated: 17 checks, of which 10
are VM tests, and 550 Rust tests across the workspace, all passing. A reader who
wants a different number can run the same command and get it.

The same checks run on every push to `main` and every pull request, through
`.github/workflows/ci.yml`: a `build` job running the format, clippy and test
commands directly, a `lint` job running `nix flake check`, and one integration
job per VM test so a failure names the test rather than the check set.

The examples the README names were each BUILT and their outputs RUN, not merely
resolved: `example-hello`, `example-header`, `example-cmake-hello` and
`example-fortran-module-recursive` each produce a program that prints what the
example is for. This section said "what you can run" for a day before anything
in it had been run; the distinction between a command that resolves and one that
works is the whole point of the section.

Two things to know before running it. The ten VM checks boot a NixOS machine, so
they need `/dev/kvm` and take a while; on a host without it they fail for that
reason and not because the tree is broken. And `nix flake check` needs the
experimental features the README lists, in the DAEMON's configuration as well as
the client's, or it fails with a message that blames the client.

The project also keeps a wider corpus of gate scripts, one per closed class,
each written to reproduce a defect so that it fails on the revision before the
fix. That corpus is not yet in this repository: it grew beside a consumer
distribution and about half of it reaches into trees and paths that are private.
Publishing it is tracked work, not a claim, and until it is published the corpus
counts are deliberately not stated here. What the wider corpus enforces is
worth stating anyway, because the disciplines apply to any test written for this
project:

- A negative control must FAIL, and its break must sit where the subject
  executes. A control whose input the subject cannot fail on is not a control.
- A count claim must be exact or not written: "every X" and "all three" are
  counted, and the count is checked against the population.
- A fixture must survive a garbage collection: no literal store paths.
- A planted value must be DERIVED from the subject, or the arm goes stale at the
  next change and the arm meant to catch staleness is itself stale.
- An instrument that fails to BUILD is not a defect in the tree, and a runner
  must score it apart from a FAIL.
- The instrument's own negative must be reachable: a mutation ignoring the
  reading must fail the self-test.

EXIT CRITERION: `nix flake check` passes on a clean clone, and the wider corpus
is either in the repository or each unported class carries a dated reason it is
not.

## 0.6 AND BEYOND

Named so they are not silently assumed:

- MULTI-ARCH. The flake declares `systems = [ "x86_64-linux" ]`. The hardcoded
  system string is fixed; the hardware is not.
- A SECOND BACKEND. Whether the Nix data structures could come from `snix`; the
  gap is specific and recorded.
- THE OPEN CLASSES. Two mechanisms across four rows: an output read at a path
  that is not the one it was written to, and a generator that reads or execs by
  a name the sandbox does not supply.
- CONFIGURE CACHING. Built, and it does not yet pay across a source edit,
  because `src` is an input of the configure derivation. Narrowing it is the
  work.

## The next concrete work, item by item

Each item names the artifact it changes and the first action that makes it
checkable. This is the part a contributor needs and an exit criterion does not
provide: what to open, and how to know the change is right.

### 1. Publish the wider corpus, cheapest group first

The instrument that verifies every closed class is not in this repository. The
sequence, measured, is:

1. **27 gates that reach only for a literal store path.** Mechanical: each path
   becomes an attribute or a by-revision lookup, which is what the corpus
   already does elsewhere. No behavior changes, so each is verifiable by
   running it before and after and getting the same verdict.
2. **49 gates that are self-contained.** A copy into the repository, plus the
   one path assumption in the runner, which resolves its own directory and then
   expects the gates beside it.
3. **13 gates coupled to a private tree on the executable path.** Each needs its
   subject turned into a fixture first. Two of them read a consumer shim by
   design and report NO ANSWER when it is absent, which is correct and stays.

Do them in that order: group 1 proves the corpus can run outside its original
environment before 49 more files depend on it.

### 2. Close the four open classes

- **An alias output read at its store path.** What aborts is the include
  scanner's read, and the row says which task is still unknown. First action:
  keep the build directory of a failing drive and read `build.ninja` for the
  edge that consumes the alias. The fix is not in the outputs-into-one-path
  code, which is correct by design.
- **An encoded entry that is not an absolute store path.** Two mechanisms print
  the same text and the derivation name cannot tell them apart. First action:
  read the RAW value, not the message. Then it is either the split already fixed
  or a build-directory-relative spelling, which is a different fix.
- **A generator that execs a tool by bare name.** One member is closed. Members
  are found by driving real packages, so the action is a sweep of a round's
  failures for `command not found`, not a code change. Widening the emitted
  PATH has a measured re-key bill and is a batching decision, not a default.
- **A generator reading data by its own location.** Package-side; whether the
  copy rule should grow is the open question. First action: decide whether a
  generator's own data directory is the driver's to carry, which is a design
  question before it is a code one.

### 3. Narrow the configure derivation's source

The configure cache is built and does not pay, because `src` is an input, so a
source edit rebuilds it. First action: split the configure derivation's inputs
into what configure actually reads, and measure whether a source edit then
leaves it alone. A measurement, then a change.

### 4. The cost-of-a-change instruments

`0.4` states the claim and the priced shapes. What it does not yet have is a
pricing instrument in the repository: the pricer lives in the development
directory with the rest of the corpus. It belongs in group 1 above, and the
first action is the same as that group's.

### 5. Multi-arch

Blocked on hardware, not on code. The emitted platform comes from the host, so
the aarch64 path can be exercised without an aarch64 machine, but the VM tests
would need a builder. Do not add a second system to the flake until the checks
are per-system or a builder exists.

## How a milestone ships

1. The work lands on `main` and the class it closes gains a gate that FAILS on
   the revision before the fix.
2. The gate joins the corpus on the configuration it can reach, and its
   registration names its verdict text.
3. The corpus passes on both configurations with the machine to itself.
4. A row is added to `CHANGELOG.md` pinning the commit and naming the gate.
5. The version moves only when its criterion is met, and never on its own while
   a re-key would land on a running build.
