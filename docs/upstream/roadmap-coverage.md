# Their roadmap, and what this fork already has against it

`README.md` and `pr-plan.md` are indexed by OUR pull requests. That is the
right index for sending, and it is the wrong one for answering "what of the
maintainer's own list is already done", which nobody could read off either
file. This is that index, against `docs/todo.md` as it stands at `9a07e67`.

Read it before proposing new work. Two items below are already covered by
code already written, and neither was visible as covered.

| their todo item | issue | state here |
|---|---|---|
| Phony target support | #5 | **substantially built**, on a different mechanism from PR 43's; 2026-08-23 adds the empty-phony no-op (header-only cmake `all`, dcd132a) |
| Depfile support | #17 | **built end to end** 2026-08-23: declared CA output (dd73947), named via NIX_NINJA_DEPFILE, and the read-back - a fresh on-disk depfile replaces the include scan, guarded by mtime against staleness in the fail-toward-the-scan direction (50fad25). The dynamic task keeps scanning by design: its build dir is reconstructed fresh |
| `mkMesonPackage` configure caching | #16 | not built; we have a measured argument FOR it |
| Writing derivation caching for local mode | - | **partly built**: `resolve_cache.rs` |
| `nix store add` async | #18 | **built** 2026-08-23, both halves: batched per-task adds (be123e1) and a NAR stamp map that survives a restart (cfba53e, flushed at end of run by 4a7edff). This row said "not built" until 2026-08-29 while the sweep below said landed, which is the summary contradicting its own body |
| Benchmarks for generating derivations | #4 | the instrument exists (per-phase timers, see the second sweep); no benchmark against their reference tree |
| Benchmarks for end-to-end compilation | #7 | same instrument, same gap |
| CMake example (`help wanted`) | #20 | **built** - `mkCMakePackage` plus `example-cmake-hello`, which builds and runs (0c53804); absent from `upstream/main`, so it is ours to offer. Still gated on PR #43, see below |
| meson `-fuse-ld` linker missing from task PATH | #52 | **built** 2026-08-23 (50fad25): the requested linker resolves outside the sandbox and rides in as input + PATH; bfd exempt, unresolved keeps the compiler's own error |

## The two that are covered and did not look it

**Phony targets.** This fork resolves phonies at the CLI target boundary and
expands them during input assembly, which is the item's last two bullets
("virtual targets e.g. if user inputs list of targets"). It is a DIFFERENT
mechanism from RCoeurjoly's in PR 43, and the difference is a design argument
that belongs in #5 rather than a claim that the item is closed. What we can
say without arguing: the item is not untouched, and whoever picks it up has
two implementations to compare rather than none.

**Writing derivation caching for local mode.** The todo says "adopt something
similar to n2's db or ninja's deps cache" and "scheduler needs to do mtime
dirtying". `resolve_cache.rs` is not that scheduler work, and saying it closes
the item would be false. What it IS: cross-round persistence of the expensive
resolve memos, appended beside `build.ninja` and replayed at start, with
entries validated at first hit rather than trusted. It attacks the same cost -
measured 76 s of resolve time re-reached task 7,500 on a restart, all of it
store round trips for content the daemon already held - from the memo side
instead of the mtime side. The two compose; neither replaces the other.

## The one with an argument and no code

**#16, `mkMesonPackage` configure caching.** Their plan is to split configure
into its own derivation. We have a cost of NOT doing it that the issue does
not state: because the configure phase goes to whichever nixpkgs setup hook
claims the slot first, and `mkMesonPackage` always sets `buildPhase` with
meson listed first in `nativeBuildInputs`, a CMake project driven through the
builder needs `dontUseMesonConfigure = true` plus explicit `cmakeFlags`.
Splitting the phase is what removes that. Evidence for their design, from a
consumer, which is worth more than agreement.

**THAT WAS WRITTEN AS A THREE-ARGUMENT WORKAROUND AND IT IS FIVE, corrected
2026-08-21 by RUNNING it rather than by re-reading the hooks.** The three were
derived from `ninja/setup-hook.sh`, `cmake/setup-hook.sh` and
`mkMesonPackage/default.nix`, and they are each necessary and not sufficient.
Driving qtsvg through the builder needs two more, and neither is reachable by
reading those files:

    dontWrapQtApps = true;              # qtbase in buildInputs makes the Qt
                                        # hook refuse without a wrapping
                                        # decision; fails in qtPreHook, before
                                        # configure runs at all
    nativeBuildInputs = [ cmake ninja ] # CMake validates CMAKE_MAKE_PROGRAM at
                                        # CONFIGURE time and refuses -GNinja
                                        # with no ninja binary to name

The second looks like it contradicts the point of the tool and does not: ninja
is there to WRITE `build.ninja`, never to execute it. The build phase is still
`nix-ninja <target>`, and that is confirmed by the provenance of the failure
rather than deduced from hook precedence - the run failed with
`Failed to build task derivation`, which only `crates/nix-ninja/src/task.rs`
emits.

The correction is worth more to #16 than the original claim was. A workaround
that grew from three arguments to five the first time somebody drove a
non-meson project is a better argument for splitting the phase than a
three-argument one, and the two additions are exactly the kind a maintainer
cannot enumerate from the source.

## The three genuinely open, in the order they are worth taking

1. **#17 depfiles - STARTED, step one done.** The issue asks for three
   things: emit the depfile as an extra content-addressed output, collect and
   parse the results into a build-dir cache, then skip inference on a second
   run for tasks with `deps = gcc`. Step one is built and measured (`dd73947`).
   It needed no new machinery: `nix-ninja-task` already copies every declared
   output into its placeholder, so appending the depfile to the output list is
   the whole mechanism. Gated on `deps = gcc` rather than on `depfile` alone,
   because a declared output the command does not produce fails the task, and
   only `deps = gcc` is ninja's own statement that one gets written.
   Verified on a two-rule fixture carrying its own negative control: the
   `deps = gcc` edge emits outputs `[hello.o, hello.o.d]` with
   `NIX_NINJA_DEPFILE=hello.o.d`, the edge without emits `[plain.o]` and no
   variable, and the realized output holds gcc's real list including glibc's
   `stdc-predef.h`.
   **Steps two and three are the ones with design questions left**, and they
   are where the value is: the cache format (n2's db, ninja's deps log, or the
   append-and-replay shape `resolve_cache.rs` already uses here), and where
   the skip decision goes in the runner. Neither is blocked on the maintainer.
   This is still the item that makes most of our own largest PR unnecessary,
   which is the argument for finishing it rather than defending the
   heuristics.
2. **#18 async `nix store add`.** The maintainer's own note ranks it the
   biggest perf bottleneck and hedges the ranking. Our driver timers now attribute the realize RPC
   separately, so we can measure whether that holds at our scale, which is a
   cheaper first contribution than the change itself.
3. **#16 configure caching.** Above.

None of the three is started, and none is an easy win. Recording them as a
ranked list rather than a backlog is the point: this file exists so the next
session does not re-derive the ranking or, worse, start item 1 by writing more
inference rules.

## The one that became answerable on 2026-08-21, and it is not in the todo file

**#20, "Add example for CMake project", `help wanted`, open since
2025-04-01.** It is absent from `docs/todo.md`, which is why this index did not
carry it until now: the index was built against the todo file, and the todo
file is not the issue list. An item can be labeled `help wanted` for sixteen
months and never appear in the roadmap this file was checking.

It is now the most actionable item here, because the thing #20 asks for is a
worked CMake recipe and the recipe is known and RUN rather than reasoned:

    dontUseMesonConfigure = true;
    nativeBuildInputs = [ cmake ninja ];
    cmakeFlags = [ "-GNinja" ];
    dontWrapQtApps = true;          # only for a Qt module
    buildInputs = [ qt6.qtbase ];   # only for a Qt module

Driven against qtsvg 6.11.1 on 2026-08-21. Configure completed, `build.ninja`
was written, the graph was parsed, and the run reached task-derivation
generation before stopping on a defect in `task.rs` (a leading `cd` from a
CMake custom command resolved as a binary; `cd` is a shell builtin). So the
packaging half is established end to end and the tool half has a named,
located blocker.

**PR #43 must be dealt with before anything is offered into #20.**
RCoeurjoly's "feat: add CMake example and support ninja phony targets" is OPEN,
CONFLICTING, and untouched since 2026-02-27, and it is an attempt at this same
issue. Whatever goes into #20 either builds on that PR or says explicitly why it
does not - the same defect this directory just made with #26 and item 7, and
the second instance is the one that would be inexcusable.

**What is NOT established, and it bounds the offer.** The run did not measure
TU granularity for a Qt module, which is the property the exercise exists to
demonstrate; it stopped at the first build task. A CMake example offered into
#20 today would be an example that CONFIGURES, not one that builds through. The
honest sequence is: fix the `cd` resolution, drive qtsvg to completion, measure
granularity, and only then offer an example. Offering it before that is
offering a recipe whose last step nobody has seen work.

An example smaller than a Qt module is the likelier fit for #20 anyway - the
maintainer asked for an example, not a Qt port - and the three non-Qt arguments
are the whole of what a plain CMake project needs.

## Audit

Round 1 (2026-08-21). Findings applied while drafting:

- the first version marked phony support "done" and the local-mode caching
  item "done". Both were overclaims read off our own commit subjects rather
  than off their todo text: the phony item is a different mechanism whose
  merits are #5's argument, and the caching item names scheduler mtime
  dirtying, which `resolve_cache.rs` does not do. Both rewritten to say what
  exists and what it does not close;
- the table originally carried a count of covered items. Cut - the honest
  statement is per-row, and a count over a list that moves on their schedule
  is stale by construction.

Round 2 (2026-08-21): the index was built against `docs/todo.md` and the todo
file is not the issue list, so #20 - `help wanted`, open sixteen months - was
absent from a document whose stated purpose is "read it before proposing new
work". A coverage index keyed on the wrong source answers cleanly and
incompletely. Fixed by adding #20; NOT fixed generally, because nothing here
reconciles the todo file against the issue list and a session could make the
same omission tomorrow for #14, #31, #41 or #52.

Not attacked by an independent reader. Status: internal working document, not
for sending. If any row is quoted outward it must be re-read against their
`docs/todo.md` first, which moves without telling us.


## 2026-08-23: the whole-graph classes, added while driving a distribution build

Seven defect classes surfaced by building an entire NixOS-style system
(ArtNix's server edition, ~1,200 derivations) through the driver, each fixed
at root, each validated by the next full-build attempt building past the
package that exposed it, and each carried by a unit test where the shape is
unit-testable:

| class | package that bought it | commit | test |
|---|---|---|---|
| out-of-build-dir output: two spellings, one graph node | openfec | b97a3d2 | campaign only (needs a graph) |
| empty phony target is a no-op | opencl-headers | dcd132a | campaign only |
| a command that is a graph output | orc (orcc) | 15152f2 | campaign only |
| rel_path must not climb out of the store output | openfec consumers | fbcbc76 + e9b9f68 | `new_built_file_tests::rel_path_never_climbs` |
| both rel_path construction sites share one function | attempt-9 drift | e9b9f68 | same test covers the shared fn |
| cross-file computed includes (`#include MACRO`) | lzo | 27fcdd9 | `computed_include_through_cross_file_define_is_declared`, with a never-defined negative control |
| depfile read-back replaces the scan when fresh | upstream #17 | 50fad25 | `depfile_read_back_tests::fresh_stale_and_empty` |

The `test`/`install` targets are handled in the CALLER's shim (ArtNix
`scripts/nn-ninja-shim.sh`, 30-row argv table): both build `all` with every
output materialized and then run the build system's own installer or test
driver in the outer derivation. That policy belongs at the shim layer, not
the driver, because only the caller knows the outer derivation's phases -
recorded here so a reader looking for the mechanism in this tree knows where
it lives and why it is absent.

## 2026-08-23, second sweep: #18 landed, plus four more classes

Roadmap movement this sweep:

- **#18 (async `nix store add`): both halves built.** Within a run,
  `new_opaque_files` batches a task's source-file adds over bounded scoped
  threads (8 per task; the connection pool stays the daemon-side bound), and
  deliberately does NOT dedup so emitted derivations are byte-identical to
  the sequential path (4a commit be123e1). Across runs, the per-file NAR
  stamp map (size, mtime, store path) persists to
  `.nix-ninja-nar-stamps.v1` beside build.ninja and seeds the client at
  startup, so a restart's uploads become stat calls; entries still validate
  per hit against the live file, and a store path missing on disk drops at
  load (cfba53e). Flushed on the 500-task tick AND at end of run - the
  drop-in mode runs one task per driver and never reached the tick (4a7edff).
- **#4 / #7 (benchmarks): the instrument already exists.** The driver
  prints a per-phase breakdown (`resolve`, `realise`, `dyn`, `nar`, `scan`) every 500
  tasks with counters behind each; a generation-phase benchmark is one run
  of that instrument with a warm daemon, where `realise` cost is early-cutoff
  only. What this fork can offer upstream is the phase-timer mechanism and
  campaign-scale readings, not a NixOS/Nix-tree reproduction of their
  reference numbers - that workload should be timed on their hardware with
  the same instrument.

New whole-graph classes, same format as the table above:

| class | package that bought it | commit | test |
|---|---|---|---|
| `#  define` / `#  include` (whitespace after the hash) invisible to the computed-include scanner | lzo, second failure on the same symptom | 41dc03a | `directive_with_space_after_hash_still_scans`, with a `#definexyz` negative |
| files named inside `-Wl,` groups (version scripts) never declared | json-c | 1fb92f8 | `wl_groups_yield_files_and_skip_output_flags` |
| store-add errors name neither the string nor the call | libconfig ("string is too long") | 4509ca7 | instrumentation, not a rule |
| one build path uploaded at two contents | gperf (config.h) | 9f79161 (guard) + 95b1b9a (cause) | campaign only; deterministic repro on the gperf drv |

Correction, same day: the gperf two-contents row was first attributed to
automake regenerating config.h mid-build. Wrong - the named-add-error
instrumentation showed the empty spelling was a TRUNCATED TEMP COPY: the
batched adds raced two uploads of one file onto a pid-keyed nn-outer temp
name (95b1b9a is the cause fix; 9f79161 stays as defense for genuinely
mutating files). A mechanism claim written before the instrument had
answered.

Third sweep, same day:

- **#20 (CMake example): done end to end.** `mkCMakePackage`
  (modules/flake/pkgs/mkCMakePackage) mirrors the meson builder with two
  differences: `-GNinja`, and `CMAKE_MAKE_PROGRAM` pointed at a dispatch
  script that sends CMake's configure-time cmTC_* try-compiles to real
  ninja - in NIX_NINJA_DRV mode a probe would submit the outer output and
  be refused as a duplicate - and everything else to nix-ninja.
  `example-cmake-hello` builds and runs (0c53804).
- New class: **quoted includes must resolve from the SPELLED parent.**
  nspr's dist/include tree is symlinks; the BFS canonicalized every queued
  file before scanning, so a symlinked header's nested quoted include was
  declared only at its source spelling and the sandbox compile died at
  the dist one. Reproduced first as a failing test in the exact symlink
  shape (f56bc0f, `symlinked_include_dir_declares_nested_quoted_include_at_both_spellings`).
- The empty-`config.h` conflict root cause: pid-keyed temp names raced
  under the batched adds (95b1b9a); see the correction note above.

## Fourth sweep, 2026-08-26 to 2026-08-29

Read against the forge on 2026-08-29: 12 issues open (4, 5, 6, 7, 14, 16, 17,
18, 20, 31, 41, 52) and 7 pull requests (8, 26, 30, 37, 42, 43, 56). A search
over every open and closed issue for `dyndep`, `fortran` and `c++20` returns
nothing, which is what decides the first row below.

| work | commits | upstream thread | what it is |
|---|---|---|---|
| ninja `dyndep`: parse the file, fold it into the loaded graph, follow `include`/`subninja` when scanning, and load every dyndep file before scheduling | e9c959b, d1838a6, db555fb | NONE, and that is the finding | Fortran and C++20 modules declare their real edges through dyndep; without it those graphs cannot be driven at all |
| a link edge carries `deps = gcc`, so every link was being treated as a compile | 127adcc | none | found alongside the dyndep work, same graph reading |
| generated headers materialize into the task through the `virtual_paths` map, and task outputs are excluded from it | 2f7b8f2, dc296f3 | none | this is the fix for our libvmaf and liblapack classes; recovered from dangling commits on 2026-08-29 |
| a trailing colon in `RPATH` survives patchelf's round trip | f8bb3bd | closed #1 is the mechanism's own issue | patchelf drops it, and the empty entry is meaningful |
| `cc` resolves lazily, so a package with no compile targets does not need one | 7fb756e | none | `Tools::new()` resolved it at startup and killed hicolor-icon-theme |
| a directory on the include path is not a header | 4e591a0 | none | `canonicalize_cached` was gated on `path.exists()`, true for a directory, so `#include <memory>` resolved to a directory and `fs::read` hit EISDIR. Regression test verified failing first |

**dyndep is the one that needs a question before a patch.** It answers no open
issue, and this directory's own rule is that answering an open question earns
the read an unsolicited PR does not. Ask whether upstream wants dyndep in tree
at all, in the same breath as round 5's pre-PR question, rather than arriving
with 880 lines.

**The five bug fixes need no permission question.** They are in their crates,
each has a failure that reproduces, and `pr-plan.md` already argues this class
must travel separately from the inference heuristics rather than being held
hostage to that argument. Drafted as `bugfix-batch-pr.md`.

Not offered, and deliberately: the input bump and the harmonia
`DerivationInputs` port (2e8bc87, 45f90dc) are fork-local maintenance against
our own flake pins, and the ArtNix opt-out triage (ce2d2a1, 99846d2) is a
record of the consumer's failures, not of nix-ninja's.

## Fifth sweep, 2026-08-29: developed or not, per open issue, read from the code

Earlier sweeps map commits to issues. This one asks the blunter question and
answers it the same way for every row: is there CODE IN THIS TREE that
implements it. Not staged, not drafted, not discussed - developed. Twelve
issues open on the forge, read the same day.

DEVELOPED - code exists and was located, not recalled:

| # | Where it lives |
|---|---|
| 52 | `task.rs:2795-2806`, and the comment names the issue: `-fuse-ld=` is stripped from the cmdline and the linker resolved into the task's PATH |
| 20 | `modules/flake/pkgs/mkCMakePackage` and `modules/flake/examples/cmake-hello`, neither of which exists in `upstream/main` |
| 5 | `build.rs:125-139` distinguishes a phony from a real output when resolving requested targets |
| 18 | `nix-builder-rpc-client/src/lib.rs:566` and `:1494` stream NARs through `tokio::io::duplex` rather than materialising them |
| 17 | `task.rs:5888` `depfile_read_back`, the freshness-guarded parse. Step one of the issue, not all of it |

ANSWERED, and there is nothing for us to develop:

| # | Why |
|---|---|
| 31 | The error is nix's, not nix-ninja's: the daemon has to implement building a dynamic derivation end to end and the container image's does not. Our contribution is the configuration that works plus a four-second probe, drafted in `issue-replies.md`. No code of ours can fix somebody else's daemon |

NOT DEVELOPED - no code in this tree, stated plainly:

| # | What is actually there |
|---|---|
| 16 | Nothing. `mkMesonPackage/default.nix` contains zero occurrences of `configure`; splitting configure into its own derivation is unstarted, and `docs/todo.md` has carried it unchecked since it was written |
| 14 | Nothing. `aarch64` appears nowhere in `flake.nix` or `modules/flake/` |
| 7 | Nothing of ours. The `[[bench]]` sections and the `divan` dependency are vendor-n2's own and predate this fork; no benchmark of end-to-end NixOS/Nix compilation exists here |
| 4 | Nothing of ours, same evidence as 7. No benchmark of derivation GENERATION exists |
| 41 | Nothing. Testing n2 with mmap has not been attempted |
| 6 | Nothing. A snix backend has not been investigated |

**So six of twelve are developed or answered and six are not.** That is the
honest count and it has not been written down in this form before, which is
why "what is covered" kept reading better than the tree deserved.

Two of the six are the cheapest work available and are labelled `help wanted`
by the maintainer: 4 and 7 are both benchmarks, and this fork already has the
instrumentation they would report - `RESOLVE_MS`, the `dyn` breakdown, and the
`plain adddrv` pair added in `e285861`. A benchmark is the natural home for
numbers this fork keeps producing ad hoc and then arguing about in prose.

`docs/todo.md` lists both as unchecked items, in upstream's own words, which
means upstream wrote them down, labelled them help wanted, and we have been
reading past them for weeks.

## Fifth sweep, same day, after acting on it

The sweep above was written to be uncomfortable and then acted on. What moved:

| # | Was | Now |
|---|---|---|
| 4 | nothing | `crates/nix-ninja/benches/generate.rs`, divan, no new dependency. Numbers recorded in the commit: graph load linear at 563 µs/1k and 7.80 ms/10k edges, include scan ~30 µs per TU |
| 41 | nothing | Answered by measurement rather than opinion. mmap beats the read 1.7x (10.9 µs against 18.9 µs) and the read is 0.26% of the graph load it feeds, so it buys ~0.1% and costs a SIGBUS fallback for page-multiple files |
| 7 | nothing | `bench/e2e.sh` runs one example end to end and records wall clock plus the driver's own counters. Validated on `example-hello` |
| 14 | nothing | The hardcoded `"x86_64-linux"` in `build.rs` is gone, host-derived with `NIX_NINJA_SYSTEM` overriding. The rest of #14 is hardware, not code |
| 6 | nothing | Prepared with evidence: snix's daemon implements three store operations against the thirteen this driver needs, none of them a build |
| 16 | nothing | Prepared: the assumed blocker is wrong and the real one is named |

Three things the work turned up that were not on anybody's list:

- **The driver never printed its own totals.** The summary is gated on
  `n_tasks.is_multiple_of(500)`, so a build under 500 tasks printed no
  accounting at all - every example here - and a longer one reported its last
  tick rather than its result. Fixed with `report_progress_final`.
  **This invalidates TOTALS, not slopes, and the first draft of this entry
  overstated it as "every number".** ArtNix made the distinction and it is
  right: a reading at task 1000 and another at task 4000 are both real, so the
  0.084 s/task and 74 KiB/task slopes between them stand as measured. What
  the modulus breaks is any figure presented as a run's result - xfce's
  372 s `dyn` is a LOWER BOUND with an unmeasured tail, not a total. The two
  claims have different consequences for what has to be re-taken, which is
  why the difference is worth the sentence.
- **The summary reports whole seconds**, so the first end-to-end run returned
  a row of zeros that look like measurements and are rounding.
  `NIX_NINJA_STATS_JSON=1` now emits the same counters in milliseconds,
  additively, because the seconds line already has readers.
- **The boost collision blocks every example**, not the six VM checks alone,
  because `mkMesonPackage` takes `nix` as a build input. Worked around by
  giving `inputs.nix` its own pre-collision nixpkgs, with a `DEFER` naming the
  condition for removing it.

What is still NOT developed, so this sweep does not repeat the flattery the
last one had to correct: #16 is prepared and unimplemented, #6 is answered and
unimplementable against snix as it stands, #14 is unverified on aarch64
hardware, and #7 has a harness whose only subject so far is the smallest
example in the tree.
