# Their roadmap, and what this fork already has against it

`README.md` and `pr-plan.md` are indexed by OUR pull requests. That is the
right index for sending, and it is the wrong one for answering "what of the
maintainer's own list is already done", which nobody could read off either
file. This is that index, against `docs/todo.md` as it stands at `9a07e67`.

Read it before proposing new work. Two items below are largely covered by
code already written, and neither was visible as covered.

| their todo item | issue | state here |
|---|---|---|
| Phony target support | #5 | **substantially built**, on a different mechanism from PR 43's |
| Depfile support | #17 | **step one built** 2026-08-21: the depfile is a declared CA output. Steps two and three open |
| `mkMesonPackage` configure caching | #16 | not built; we have a measured argument FOR it |
| Writing derivation caching for local mode | - | **partly built**: `resolve_cache.rs` |
| `nix store add` async | #18 | not built |
| Benchmarks for generating derivations | #4 | profiler samples, no benchmark |
| Benchmarks for end-to-end compilation | #7 | profiler samples, no benchmark |
| CMake example (`help wanted`) | #20 | **the recipe is now known and RUN**, 2026-08-21 |

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
2. **#18 async `nix store add`.** The maintainer's own note calls it "probably
   biggest perf bottleneck". Our driver timers now attribute the realise RPC
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
file is not the issue list. An item can be labelled `help wanted` for sixteen
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
issue. Whatever goes into #20 either builds on that PR or says clearly why it
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
