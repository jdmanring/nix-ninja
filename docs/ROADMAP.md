# Roadmap

The path from a driver that builds one project correctly to the best resumable
build system in the world. Nine milestones. `0.1` through `0.5` ask whether the
driver is right, `0.6` whether a stranger can run it, `0.7` whether it is worth
choosing, `0.8` whether it holds at scale and across machines, and `0.9` whether
anything depends on it. Each has an exit criterion, the work that satisfies it,
and the instrument that says whether it is met. No dates: a milestone ships when
its criterion is met and the named gate says so on a clean tree.

`0.1` and `0.2` are upstream's; they came with the project and both are
complete. From `0.3` on, the numbering is this project's own, and is written
here rather than proposed: it records how the work since has been organized, not
a plan awaiting ratification.

The tree is at **0.3.0** (`Cargo.toml`). The version names the milestone the
work is currently toward, and it advances when the milestone before it is met
and a gate says so, never because time passed. So `0.3.0` means `0.1` and `0.2`
are met and `0.3` is in progress rather than finished, which is why its open
classes are listed below. A reader should not take the number as a claim that
the milestone it names is complete.

The state of the tree TODAY, class by class, is a table of 136 rows. It is not in
this repository: it grew beside a private distribution and is development
material rather than a public document, which is the same reason the gate corpus
is not here (`0.5`). What this file carries is the part that is publishable now,
the ladder and its criteria, and what each rung is worth.

## Contents

The milestones, in order, each with its claim and exit criterion:

- [0.1 CORRECTNESS (upstream)](#01-correctness-upstream) and
  [0.2 PERFORMANCE (upstream)](#02-performance-upstream), both MET
- [0.3 REAL-PACKAGE RESILIENCE](#03-real-package-resilience), the milestone in
  progress
- [0.4 THE COST OF A CHANGE](#04-the-cost-of-a-change)
- [0.5 VERIFICATION](#05-verification)
- [0.6 THE DESTINATION](#06-the-destination)
- [0.7 THE COMPARISON](#07-the-comparison)
- [0.8 SCALE](#08-scale)
- [0.9 THE ECOSYSTEM](#09-the-ecosystem), after which [`1.0`](#06-the-destination)
  is a state rather than further work

And the supporting sections:

- [What does not work today](#what-does-not-work-today), the open list
- [The next concrete work, item by item](#the-next-concrete-work-item-by-item),
  nine items with a first action each
- [What could stop this](#what-could-stop-this), the risks
- [How a milestone ships](#how-a-milestone-ships)

## Where this file comes from

The project began when `pdtpartners/nix-ninja` stopped. Work continued here:
the tree is more than 800 commits past the last upstream commit of May 2026,
and the milestone numbering above two is new. `docs/todo.md` records the state
of the issues that came with the project; this file is what comes after them.

## What this project is not trying to do

Named so a reader does not infer them, and so that a proposal in one of these
directions can be answered by pointing here rather than by argument.

- **It is not a new build system.** The input is a ninja file. Anything that
  emits one works, and anything that does not is out of scope. A cargo backend
  is named in `0.9` as an open question, not as a commitment.
- **It is not a replacement for ninja.** Real ninja runs the parts the driver
  cannot serve, and the drop-in routes to it rather than reimplementing it.
- **It is not a package manager or a distribution.** It builds packages; what
  goes into them is the consumer's decision.
- **It does not vendor or fork Nix.** It depends on experimental features and
  tracks Nix master through its own flake input. Where a defect is in Nix, the
  fix belongs in Nix, and the project says so rather than patching around it.
- **It is not a remote-execution protocol.** Distribution is expected to come
  from Nix's own builders and binary caches rather than from an implementation of
  `REAPI`, which `0.8` tests rather than builds.

## 0.1 CORRECTNESS (upstream)

A CMake project builds end to end through the driver. Phony targets are
supported past the original scope: aliases expand transitively, a phony over no
file is tolerated, and a source file the generator also marks as a phony output
still reaches the task that needs it. MET.

The criterion is upstream's, stated in its README; the milestone is this
project's to mark MET, and the phony work that finished the last of it landed
here in September, after upstream stopped.

## 0.2 PERFORMANCE (upstream)

Benchmarks exist for generation and end-to-end compilation; a configure step is
cached in its own derivation; a task's depfile is a declared output the driver
reads back. The store-add path left the critical path: uploads run concurrently,
a NAR streams, and a stamp map skips a file already known. MET, with its
measurement in `CHANGELOG.md` and the run it came from in `bench/records/`.

Same reading as `0.1`: the criterion is upstream's, and the depfile work that
satisfies it landed here.

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
  resolve to a file. The macro-arm reader has four axes and all four are closed:
  the delimiter a body is spelled with, the count of arms a macro has, an arm
  that sits above the build directory, and a define that lives in the
  translation unit's own body rather than in a header it includes.
- **Undeclared reads.** A file a COMMAND reads and no edge declares, so the
  sandbox stages nothing. The largest family, and the one still growing:
  `cmake -P` scripts and the modules they include, `cmake -E cmake_autogen`
  plans, XInclude, TableGen, rst includes, msgfmt ITS, swig `%include`, the
  `g-ir-scanner` filelist, protoc imports, valac `--pkg`, python `-m`, the
  `glib-mkenums` template-and-header arguments, and `glib-genmarshal`'s
  positional list. Every member is the same shape and is settled the same way: the
  CMake rule naming the command declares a dependency on none of the files that
  command reads, because the convention assumes a shared filesystem, so the
  test is never whether a `DEPENDS` exists. It is whether real ninja succeeds
  while the sandbox stages nothing, which puts the line in the driver.
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
fix. How many of those instruments are in THIS repository is a question the
gate cell answers differently by how it is read: counting a row whose GATE cell
names a file under `tests/` or a `*_tests` symbol gives 5 of 95, also allowing
a test named in prose gives 20, and allowing "mutation-check" as well gives 24.
These are read from the table, not from this paragraph, so they move when a row
lands; re-derive them by parsing the gate cell rather than copying these
numbers. Whichever predicate is used the answer is a minority, so the honest
statement is that most classes are verified by the gate corpus, which is not
published yet, and a reader has this file's word rather than a test they can
run; closing that is `0.5` and work item 1. The earlier form of this paragraph
said "most of those instruments are covered by a test in this repository",
which every reading of the count inverts. The trap that recurs: a reader
emitting an absolute spelling is dropped in silence by the wiring filters, so
it looks fixed and changes nothing.

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
builds end to end, one per example shape: hello, header, multi-source,
shared-lib, generated-header, CMake order-only header, the dotdot source
include, dynamic deps, a run script and the C++20 Fortran-module example.
`example-nix` has no VM test, because it does not build (below). Some closed
classes also carry a unit test here, and those are the ones that survive a
refactor; a class whose only instrument is a corpus gate is listed in `0.3` and
is what work item 1 is about. This sentence previously claimed every
unit-testable class had a test here, a universal form the count in `0.3` does
not support.

The counts, measured on a clean tree rather than stated: 17 checks, of which 10
are VM tests, and 550 Rust tests across the workspace, all passing. A reader who
wants a different number can run the same command and get it.

The same checks run on every push to `main` and every pull request, through
`.github/workflows/ci.yml`: a `build` job running the format, clippy and test
commands directly, a `lint` job running `nix flake check`, and one integration
job per VM test so a failure names the test rather than the check set.

Four of the examples the README names were BUILT and their outputs RUN, not
merely resolved: `example-hello`, `example-header`, `example-cmake-hello` and
`example-fortran-module-recursive` each produce a program that prints what the
example is for. Four more run under the VM tests rather than as a local build,
and `example-nix` does not build at all (below). This section said "what you
can run" for a day before anything in it had been run; the distinction between
a command that resolves and one that works is the whole point of the section.

Two things to know before running it. The ten VM checks boot a NixOS machine, so
they need `/dev/kvm` and take a while; on a host without it they fail for that
reason and not because the tree is broken. And `nix flake check` needs the
experimental features the README lists, in the DAEMON's configuration as well as
the client's, or it fails with a message that blames the client.

The project also keeps a wider corpus of gate scripts, one per closed class,
each written to reproduce a defect so that it fails on the revision before the
fix. That corpus is not yet in this repository: it grew beside a consumer
distribution, and 11 of its 92 scripts reach into trees and paths that are
private. Publishing it is tracked work rather than a claim, and the triage under
[the next concrete work](#the-next-concrete-work-item-by-item), item 1, is what
decides the order. What the wider corpus enforces is worth stating anyway,
because the disciplines apply to any test written for this project:

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

## 0.6 THE DESTINATION

`0.1` through `0.5` answer whether the driver is RIGHT. This one answers whether
it is ADOPTABLE, which is what decides whether the rest mattered.

It was missing until now, and its absence is why the numbered list read as
open-ended. A ladder is not a plan until it says what the top is, and this file
described five rungs and then a list of things that were not milestones at all.
Three of the four items it carried were already work items below, restated.

THE CLAIM: someone who is not this project adopts nix-ninja from the published
repository alone, and points it at a distribution.

The destination is partly reached in one instance. One distribution builds
through the driver at per-translation-unit granularity, and of its three editions
that is the server; the two desktop editions are refused before their first
iteration, by the consumer's own resumability gate rather than by a build
failure, and the work to clear that refusal is recorded on the consumer's side.
So even the worked example is not yet a whole distribution through the driver.

What is missing is larger than that: a STRANGER cannot reach the same place. The
difference is the published apparatus rather than the driver, the corpus, the
failure signatures, and the record of what is already known to break.

The route is the nine items below, which is where each one's first action lives.
What belongs here is the criterion they add up to.

EXIT CRITERION: a clone of this repository, with nothing installed but an
experimental Nix, runs `nix flake check` green and runs the wider corpus green,
and every closed class carries a gate arm rather than a package witness. Two
things are deliberately outside it, each named with what blocks it rather than
left to be assumed: multi-arch is blocked on hardware (item 5), and a second
backend on whether the Nix data structures can come from `snix` at all, which is
an open question, not scheduled work.

`1.0` is not a further milestone. It is the version this project takes once
`0.6` has held across a release with no new class appearing, `0.7` has numbers on
all four axes, `0.8` has held at distribution scale across two machines, and
`0.9` has an adopter who is not the consumer. It is the marker for the state
where the remaining work is maintenance rather than development: the first moment
the claim describes the project rather than the last round of work, and the first
moment someone chooses it rather than reaching for it out of curiosity.

## 0.7 THE COMPARISON

THE CLAIM: nix-ninja is competitive with other resumable build systems, and the
comparison is measured rather than asserted.

Nothing in this file or in `README.md` compares this project to any other
builder. The design notes compare two internal variants of the include scan and
nothing else. `bench/records/README.md` says of the one end-to-end record set:
"no comparison against plain ninja, which is the comparison a performance claim
actually wants. Anyone quoting a speedup from this file is quoting something
that was not measured."

So the milestone begins by admitting it has no numbers, which is why it is a
milestone and not a section. Four claims, each measured on its own axis, each
against a named baseline, because one number cannot answer "is it better".

- **A source edit rebuilds one translation unit.** Against plain ninja on the
  same tree, same machine. This is the mechanism's central claim: one edge
  changed, one task re-keyed, the rest substituted from the Nix store. It is also
  the one most likely to hold, since the granularity is per edge by construction.
- **A killed build resumes without redoing completed work.** Against a restart
  from zero. Already true per translation unit, because a completed task is a
  valid store path, but never measured as a number, and the two desktop editions
  are currently refused before their first iteration by the consumer's own
  resumability gate rather than resumed.
- **A compiled object substitutes across machines and users.** Through an
  ordinary Nix binary cache, with no remote-execution protocol to implement.
  This is the capability that is genuinely hard to match without reimplementing
  `REAPI`, and the most distinctive thing here if it holds.
- **Total wall clock from scratch.** Against plain ninja, `distcc` and `ccache`.
  The bench README names this as the comparison a performance claim wants. It is
  also the axis most likely to lose, because a derivation per translation unit
  adds driver and store overhead that a plain `ninja` does not pay.

EXIT CRITERION: each of the four axes carries a recorded number against its
named baseline, produced by a harness in this repository, with the machine,
the package and the store state recorded beside it. Axes that lose are published
as losses. THE THIRD FORM IS NOT THE FIRST, as in `0.3`: a number from a machine
nobody else has is evidence about that machine, and the harness is what makes it
evidence about the project.

This milestone is deliberately placed before the two that follow, and it is the
one most likely to change the others: a measured loss on total wall clock does
not invalidate `0.3`, but it would change what the README should claim.

## 0.8 SCALE

`0.7` asks whether the driver is competitive at the size of a package. This one
asks whether it is competitive at the size of a build farm, which is where a
resumable builder either becomes the best in the world or stays a clever local
tool.

Three measured facts say the current answer is no. Each is carried with its
numbers in item 6 below, and is named here without them so that the figures
live in one place:

- **Where the wall clock goes.** Most of a campaign's time is unaccounted for by
  the driver's own instruments, and the term that holds it is a daemon round trip
  the driver could batch or memoise.
- **Retained memory.** The driver holds a little more per task than it releases,
  linearly, and nothing bounds it because it runs outside the build cgroup.
- **The daemon's serialization point.** Realise latency under a round is
  libstore's single state mutex, which no amount of local parallelism removes.
- Realise latency under a round is the daemon's single SQLite state mutex, which
  is a serialization point no amount of local parallelism removes.

THE CLAIM: nix-ninja builds a distribution the size of a desktop, or larger,
without the driver becoming the bottleneck, and it does so across more than one
machine.

Three axes, and the third is the one that separates this from every local
incremental builder:

- **The driver stops being the bottleneck.** Both measured slopes have to be
  explained before they can be fixed, and neither is diagnosed. This is the same
  first action item 6 already names: read the timers on a real build, profile the
  retention, then decide. A batch and a memo are different shapes and this
  project has twice tuned the wrong one by picking first.
- **A build survives the daemon's serialization point.** The mutex is in libstore
  and is not this project's to remove, so the question is whether the driver can
  keep the daemon's work queue full enough that the mutex is not the constraint.
  That is a measurement before it is a design.
- **A build runs on more than one machine.** Nix already distributes ordinary
  derivations to remote builders and substitutes them through a binary cache,
  and every task here IS an ordinary derivation, so this capability should be
  inherited rather than built. Whether it is, at per-translation-unit
  granularity, is unmeasured. If it works, this is the claim no other
  per-translation-unit builder can make without implementing `REAPI`, and if it
  does not, the reason is a defect worth finding.

EXIT CRITERION: a real distribution builds through the driver with the machine's
own resources never saturated by the driver, and the same build completes across
two machines with objects substituted rather than recompiled. Recorded as
numbers on the same axes `0.7` defines, not as a demonstration.

This milestone is where the project stops being a faster local build and becomes
infrastructure. It is deliberately after `0.7`: there is no point scaling a build
whose per-unit cost is unmeasured.

## 0.9 THE ECOSYSTEM

`0.8` makes the builder fast and distributed. This one makes it something other
people build on, which is the difference between a good tool and the standard.

THE CLAIM: this project is depended on by someone who is not its author, and the
apparatus that makes its claims checkable by a stranger is public.

Nothing here is required for the driver to be excellent, which is why it is last
and why each part is separately declinable. It is what "the best resumable build
system in the world" means beyond this repository's own numbers.

- **A second frontend.** The driver reads ninja files, so it already serves every
  build system that emits one. A build system that does NOT emit ninja is
  unreachable, and the honest question is whether that set matters. Named rather
  than assumed: `cargo` and `go` are the two whose scale would matter most.
- **A public cache.** Substitution is the mechanism's whole advantage and it
  currently depends on a cache the consumer runs. What it takes to offer one, and
  whether it is this project's to run, is a question before it is work.
- **The failure corpus, published.** `0.5` tracks this. It is the instrument that
  makes the claims here checkable by someone else, and until it is in the
  repository the wider corpus is evidence this project has and a reader does not.
- **Adoption by a distribution that is not the consumer's.** The strongest
  verification available, and the only one that tests the documents rather than
  the code, since a stranger has to follow them without the author present.

EXIT CRITERION: at least one build system outside ninja, or one distribution
outside the consumer's, depends on this project, and the corpus is public enough
that a third party can run the check that says so.

There is no milestone after this one. A project that is correct, verified,
competitive, scalable, distributed and adopted is finished in the sense that the
next work is maintenance, and `1.0` is the marker for that state.

## What does not work today

Named here because the README sends a reader to this file for the first of
them, and a roadmap that lists only the work done reads as a claim that
nothing is outstanding.

- **`example-nix` does not build.** It builds Nix itself per translation unit
  and it is the largest example; it aborts with `ABORTED: submitting outer
  output`, caused by a daemon refusal to submit a duplicate output named `out`.
  The abort is reproducible at revisions before any recent change, so it is not
  a property of the current tree. What is established: the two claimants are
  dynamic derivations with no declared inputs and identical names, and their
  emitted output lists differ, one naming a CMake try-compile scratch edge and
  the other the real link edge. What is NOT established is the mechanism, and
  the driver-side fixes for both candidate mechanisms were falsified by
  measurement before landing, so neither is proposed again. Settling it needs
  root and a stopped daemon. The README therefore does not offer this example as
  a command to run.
- **Nothing here has been compared to another builder.** Not on any axis. This
  is the largest gap on this page and the one a reader is most likely to assume
  away, because the mechanism sounds like it must be faster and no number
  contradicts them. `0.7` names the four axes and item 7 says how each is
  measured. Until then the honest statement is that the granularity is per
  translation unit by construction, which is a fact about the design, and that
  no measurement here says what that is worth.
- **Four open classes in `0.3`**, whose first actions item 2 below names, and
  **three inherited items** carried with no diagnosis, in item 6.
- **The driver has never been run at distribution scale on hardware it owns.**
  The two slopes in item 6, `dyn` at 0.084 s per task and retention at 0.074 MiB
  per task, both project to large numbers at 30,000 translation units, and
  neither has been observed at that size, because the machine that would run it
  is the one the consumer builds on. `0.8` states what has to be true; nothing
  here has tested it.
- **Nothing here has ever been built on more than one machine.** The capability
  is expected to come free from Nix, and an expectation is not a measurement.
  Item 9 is one afternoon that settles it.

## The next concrete work, item by item

Each item names the artifact it changes and the first action that makes it
checkable. This is the part a contributor needs and an exit criterion does not
provide: what to open, and how to know the change is right.

### 1. Publish the wider corpus, cheapest group first

The instrument that verifies every closed class is not in this repository. A
triage over the whole corpus, measured rather than asserted, splits 92 gate
scripts three ways:

1. **Gates that reach for a store path that can be collected.** ONE script,
   and it is a documented exception rather than a defect. A first pass counted
   ten scripts here by matching `/nix/store/<hash>-` in the code, and nine of
   those hold synthetic fixture paths (`aaaa…-fixture-1.0`) that never exist in
   a store and cannot be collected. The one real literal is
   `round-intercept-sweep.sh`'s retained round's leaf log, which is NOT
   derivable: resolving the same subject by attribute returns a different
   derivation. It stays, and fails loud when the path is gone.
2. **Gates that are self-contained.** 80 scripts. A copy into the repository,
   plus the one path assumption in the runner, which resolves its own directory
   and then expects the gates beside it.
3. **Gates coupled to a private tree on the executable path.** 11 scripts,
   judged on the code rather than on a comment, since a gate that merely
   mentions a consumer path in a comment is not coupled. Each needs its subject
   turned into a fixture first. Two read a consumer shim by design and report
   NO ANSWER when it is absent, which is correct and stays.

The split is exact and exhaustive: 1 + 80 + 11 = 92, and no script falls in two
groups. The work is therefore almost entirely group 2, which is a copy and one
runner assumption, and group 3, which is the real cost.

**Publishing is a copy; RUNNING is the cost, and the two are not the same
size.** Measured over the same corpus: 78 of the 92 scripts invoke nix at all
(a `build`, an `eval` or a `develop`), counting an invocation in CODE rather
than any mention of the word, because one probe prints the words "nix builds at
once" in an explanatory line and needs no nix whatsoever. Of the 14 that do not,
most read a file this tree does not carry: `CLAUDE.md` is gitignored, so a clone
has none of it, and a gate that reads it FAILS SPURIOUSLY there rather than
reporting an answer - measured by hiding the file and running one, which printed
`grep: CLAUDE.md: No such file or directory` and then two FAIL lines.

4 of the 92 EXECUTE in a clean clone, checked out with `git clone` and run
there with `local/` absent, no argument and nothing installed. Two of those 4
assert about this repository's own source and are therefore worth publishing as
corpus arms: `stdout-contract-sweep.sh` exits 0, and
`duplicate-predicate-sweep.sh` returns its real verdict because a clone carries
the git history it reads. The other two measure something else -
`cmake-byproducts-target-expression.sh` asserts about CMAKE, and
`daemon-gap-cpu-probe.sh` about the running machine - so they belong in the
repository as tools rather than as corpus arms. The two are different KINDS of
tool and that matters for CI: the cmake one prints a verdict and CI asserts on
it, while the daemon probe samples live workers and prints none, so a runner
can only run it by hand. TWO EARLIER COUNTS OF THIS FIGURE WERE
WRONG and both are worth keeping: a grep for scripts containing no `nix` call
said 13, and a probe that scored any non-zero exit as "ran" said 5. The second
counted `round-intercept-sweep.sh`, which prints a usage line because it needs
a round log and its live arm reads a retained leaf log from the store.

DONE for the first four: `stdout-contract-sweep.sh`,
`duplicate-predicate-sweep.sh`, `cmake-byproducts-target-expression.sh` and
`daemon-gap-cpu-probe.sh` are tracked under `gates/` and each runs in a clone
with nothing installed. The first two are self-locating, walking up to
`Cargo.toml` rather than assuming a depth, because the private corpus reaches
the same file through a symlink one level deeper, so there is one physical copy
and no second version to diverge; the other two read nothing in the tree and
needed no such change. CI runs the THREE that print a verdict, matching each
gate's verdict text rather than its exit code, because the duplicate-predicate
sweep exits non-zero by design.

What remains is 87 scripts, and each carries its reason in the corpus rather
than in this repository: a `REASONS.md` beside the gates states one for every
script, derived from the script itself rather than catalogued. The 77 that
invoke nix need the experimental features in the daemon, a working store and
time, which is the same prerequisite `nix flake check` has and the README
states, and the other 10 need either an argument, a parent gate, a gitignored
file, or a subject turned into a fixture. Those reasons are stated there and
not here for the reason the whole corpus is: publishing it is work item 1, and
a reason stated in this file would be a second copy to keep in step.

Do them in that order: the first group proves the corpus can run outside its
original environment before the second depends on it.

### 2. Close the four open classes

- **A macro arm.** CLOSED, and it took four axes rather than the one this item
  named. The reader now accepts the delimiter a body is spelled with
  (angle-bracket as well as double-quoted), keeps a single-arm macro when that
  arm is an angle include, resolves an arm against the directories the command
  names even when they sit above the build directory, and reads a define that
  lives in the translation unit's own text rather than only in its includes.
  Each axis was unmasked by the previous one closing, so the first three were
  landed before the package stopped failing: fixing one leaves the next fatal.
  The fixture that hid the third axis is the lesson worth carrying - it built
  two arms because its author reasoned "one arm is the ordinary case", which is
  the shape that passes rather than the shape production has. A control written
  to the passing shape asserts nothing.
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

### 6. Open in the inherited issue list

None of these is a milestone item, because none has a diagnosis. They are
carried here so that the inherited list is not the only record of them.

- **Local-mode derivation caching.** A local drive re-derives instead of
  reading a persisted graph, so it adopts neither n2's database nor ninja's
  deps log, and the scheduler does no mtime dirtying. First action: decide
  whether the local route is worth a cache at all, since the consumer's route
  is inside a derivation and never sees it.
- **Where a campaign's wall clock goes.** `dyn` read 82 percent of it and its
  instrument named none of that, because three `add_drv_to_store` call sites
  were untimed. The timers now exist and print `plain adddrv` and
  `sandbox adddrv`, and no build has produced a number from them yet, so the
  0.084 s per task figure remains an elimination rather than a measurement.
  First action: read that number on a real build. Do not choose between the
  memo and the batch before reading it; this project has twice tuned the wrong
  one by picking first.
- **Driver memory that is retained and never released.** Measured at a clean
  0.074 MiB per task, linear, continuing after NAR uploads and scanning have
  stopped; about 2.2 GiB projected at 30,000 translation units, in the user
  session where no cgroup bounds it. Not diagnosed. First action: profile
  before writing anything, because the named candidates are guesses.

Both figures above come from the distribution builds recorded in `docs/todo.md`,
which is a development note rather than a `bench/records/` run, so they are
reproducible from this repository but not from a benchmark record; the README
says which of its own numbers trace to a record and which do not.

The inherited item on aarch64 hardware is item 5 above.

### 7. Build the comparison harness

`0.7` states four axes and has no instrument for any of them. The first action is
not a build change, it is a harness: take a real project, build it identically
under plain ninja and under the driver, on the same machine with the same store
state recorded beside each, and record both. `bench/e2e.sh` already produces the
driver's half of such a record and says in its own comments what it does not
establish; what is missing is the baseline beside it and a way to say which of
the two the numbers came from.

Do the four in this order, each measured before the next is started, because the
result of one may change what the next should claim:

1. Incremental edit, against plain ninja. A one-line change to one source file,
   both builders, wall clock and the count of translation units actually
   rebuilt. The mechanism predicts one; the measurement is whether the driver
   adds overhead that eats it.
2. Resume after interruption. Kill a build partway, restart it, and count work
   redone rather than resumed. Needs a way to distinguish the two in a log.
3. Cross-machine substitution. Build on one machine, realise the same derivation
   on another through a binary cache, and assert the object is the one the first
   machine produced rather than a rebuild.
4. Total wall clock from scratch. Last, because it is the axis most likely to
   lose and the numbers on the other three should exist before this one is run.

The rule from `0.3` applies unchanged: a measurement needs a baseline that can
fail to differ from it. A plain-ninja run and a driver run that both took the
same wall clock because neither did any work is not a comparison, and the
harness has to be able to tell that apart from a real tie.

### 8. Diagnose the two measured slopes, then fix them

`0.8` states the claim and both limits are already measured as numbers, and both
are carried in item 6. Neither has a cause, and the first action is the same for
both: read what the instruments already print on a real build before designing
anything. The two entries are not restated here, so that a figure that moves
moves in one place; item 6 is the record.

### 9. Establish whether a remote builder already works

The cheapest and most valuable measurement in `0.8`, and it may be a measurement
rather than work. Nix distributes ordinary derivations to remote builders and
substitutes them through a binary cache; every task here is an ordinary
derivation; so the capability is either already present or blocked by something
specific. First action: run one real build with a `builders` line naming a second
machine and read whether the tasks are scheduled there or refused. The failure
mode is specific and worth one afternoon, and the answer decides whether `0.8`'s
third axis is a feature to build or a claim to verify.

## What could stop this

Every milestone above assumes the project keeps moving. These are the ways it
might not, named so that a stall is visible as one rather than as slow progress.
None is hypothetical: the first three are why upstream is dormant.

- **The dependency is experimental and moves under the project.** `0.4` exists
  precisely because an edit anywhere can re-key everything; the same is true of a
  Nix change the project does not control. This is not hypothetical: `docs/todo.md`
  records a nixpkgs boost/context patch that blocked all six NixOS VM checks from
  2026-08-29 until the nixpkgs pin moved in October, at which point they passed
  again without a change here. A single upstream decision cost six weeks of green
  checks and could as easily have cost a milestone's worth of work. The only
  hedge is tracking master closely and reading the failure as its own class
  rather than as a defect here, which the entry does.
- **The bus factor is one.** There is one developer and one consumer
  distribution. `0.9` names adoption by someone else as the mitigation, which is
  also its exit criterion, so this risk and that milestone are the same problem
  seen from two directions.
- **The verification apparatus is private.** The 90-row class table and the
  whole gate corpus, all 92 scripts, live outside this repository. `0.5` and
  work item 1 exist to publish them, and until they land, every claim here is
  checkable only by its
  author, which is the property that made upstream's work hard to continue.
- **The scale claim is a projection.** `0.8`'s two slopes are measured at small n
  and extrapolated to 30,000 translation units. A linear fit over a small range is
  the classic way to be wrong, and the failure would appear first as a build that
  is merely slow rather than broken, which is the hardest kind to notice.
- **The comparison may lose.** `0.7` says so in advance. If total wall clock is
  worse than plain ninja, the honest response is to publish it and re-aim the
  claim at substitution and resumability, not to stop measuring.

## How a milestone ships

1. The work lands on `main` and the class it closes gains a gate that FAILS on
   the revision before the fix.
2. The gate joins the corpus on the configuration it can reach, and its
   registration names its verdict text.
3. The corpus passes on both configurations with the machine to itself.
4. A row is added to `CHANGELOG.md` pinning the commit and naming the gate.
5. The version moves only when its criterion is met, and never on its own while
   a re-key would land on a running build.
