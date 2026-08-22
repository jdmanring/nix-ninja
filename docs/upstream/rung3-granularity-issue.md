# DRAFT issue: a one-file edit rebuilds every TU derivation on a CMake project

Not filed. Sessions stage; James files.

This is the strongest single finding this fork has produced for upstream,
because it is not a feature request or a scale report. It is the project's
central claim - one derivation per translation unit, so one edit rebuilds one
object file - not holding on a CMake project.

## The measurement

qtsvg 6.11.1, built through `mkMesonPackage` with the CMake arguments below,
on nix 2.36.0pre with `builder-rpc-v0` available.

    translation-unit derivations:                     27
    TU derivations that changed after a ONE-FILE
    comment edit to src/svg/qsvganimate.cpp:          27

For contrast, the same harness against pixman (meson, C) changed 1 of 35.

Repeated with `-DCMAKE_DISABLE_PRECOMPILE_HEADERS=ON`: still 27 of 27, so the
precompiled header is not the cause.

## The population differs in a way that is not explained

Measured side by side in one session, same driver, same harness:

    pixman   35 TU derivations   35 resolved (.drv)    0 generators (.drv.drv)
    qtsvg    27 TU derivations    3 resolved (.drv)   24 generators (.drv.drv)

A resolved pixman derivation carries 59 store inputs, 7 of them named `.c` or
`.h` files, and no whole-tree input, so per-file resolution demonstrably works
for that build. Nearly all of qtsvg's stayed dynamic generators instead.

Whether that is the failure, a symptom of it, or an artifact of when the count
was taken is unmeasured, and it is the first thing worth asking.

## A mechanism this draft OFFERED and then withdrew, recorded so it is not re-offered

An earlier version of this draft said each per-TU derivation depends on the
entire source tree, evidenced by a qtsvg derivation naming
`/nix/store/<hash>-src` and no individual source file, with ten store inputs.

That reading was taken from a `ninja-build.drv.drv` - a GENERATOR - where a
whole-tree input is normal, because the generator has not yet resolved which
files each unit needs. It was then compared against a pixman `ninja-build.drv`,
which is the resolved output. The two differ by four characters.

The count is unaffected and the cause is withdrawn.

## What this draft does NOT claim

It does not say why. Whether the result is CMake-specific, C++-specific, a
consequence of the custom-command path, or a regression is unmeasured, and a
cause offered without measurement is the thing this staging directory keeps
catching in itself - twice in this document alone, the precompiled header and
then the whole-tree input.

The side-by-side gap an earlier version flagged is CLOSED: pixman and qtsvg
were run in the same session against the same driver, and the resolved/generator
split came out of that pairing.

What remains open before filing: whether 27 of 27 reproduces off this machine,
and whether a CMake project smaller than a Qt module shows the same shape.

## The recipe, because it is a second finding and #20 asks for it

Driving any CMake project through `mkMesonPackage` needs arguments that reading
the nixpkgs setup hooks does not produce. The first three are derivable from
the hooks; the last two are not, and were found by running:

    dontUseMesonConfigure = true;          # meson's hook claims the configure
                                           # slot first, so cmakeFlags are
                                           # never consumed otherwise
    nativeBuildInputs = [ cmake ninja ];   # CMake validates CMAKE_MAKE_PROGRAM
                                           # at CONFIGURE time and refuses
                                           # -GNinja without a ninja to name
    cmakeFlags = [ "-GNinja" ];            # the automatic path is unreachable:
                                           # ninja's hook sets buildPhase only
                                           # when unset, cmake's adds -GNinja
                                           # only when buildPhase is
                                           # ninjaBuildPhase, and
                                           # mkMesonPackage always sets it
    dontWrapQtApps = true;                 # Qt module only: qtbase in
                                           # buildInputs makes the Qt hook
                                           # refuse without a wrapping
                                           # decision, failing in qtPreHook
    buildInputs = [ qt6.qtbase ];          # Qt module only

The `ninja` entry looks like it contradicts the point of the tool and does not.
It writes `build.ninja` and never executes it: `mkMesonPackage` sets
`buildPhase` on the overriding side of its merge, so the build phase is still
`nix-ninja <target>`. Confirmed by the provenance of a failure rather than
deduced - the failing run printed `Failed to build task derivation`, which only
`crates/nix-ninja/src/task.rs` emits.

## Relationship to their open work

- **#20, "Add example for CMake project"** (`help wanted`, open since
  2025-04-01) asks for exactly this recipe. **PR #43** is an open, conflicting
  attempt at it, untouched since 2026-02-27. Anything offered into #20 either
  builds on that PR or says why not.
- **#16, `mkMesonPackage` configure caching.** The recipe above is a cost of
  not splitting the configure phase, from a consumer. It grew from three
  arguments to five the first time somebody drove a non-meson project, which is
  a better argument for their design than agreement.
- The granularity finding is not any open issue. It would be a new one.

## Prerequisite fix, already pushed to the fork

`crates/nix-ninja/src/task.rs` asked PATH for `cd`. A CMake custom command
opens `cd <subdir> && <real command>`; the binary pick skipped `:` and `&&` but
not `cd`, which is a shell builtin, so the task failed with `Failed to find cd:
cannot find binary path` - reading as a missing tool rather than as a command
shape the resolver does not handle. `cd_depth` in the same file already
recognises that prefix for path rewriting; only the binary pick was never
taught it. Stepped over as a pair, since skipping `cd` alone resolves its bare
directory argument next.

That fix is a clean, small, self-contained PR on its own and does not depend on
any of the above. It is the one piece here that is ready.

## Audit

Round 1 (2026-08-21), drafted and attacked in the same pass:

- the first draft asserted the precompiled header as the cause. Killed by
  running with PCH disabled: still 27 of 27. Rewritten to record the dead
  hypothesis, because a report that names a wrong cause is worse than one that
  names none;
- the first draft said "pixman gets 1 of 35" without noting the two runs were
  not side by side. CLOSED rather than flagged: pixman was re-run in the same
  session against the same driver and measured 1 of 35 with a 35-derivation
  baseline, and the resolved/generator split above came out of that run;
- the first draft named a cause - whole-tree inputs on every TU derivation -
  read off a GENERATOR and compared against a RESOLVED derivation. Withdrawn
  above. A report naming a wrong cause is worse than one naming none, and this
  one would have sent a maintainer to the wrong half of their own code;
- the first draft led with the recipe. The granularity finding leads now,
  because the recipe is useful and the granularity result is the one that
  bears on whether the project's central claim holds.

Not attacked by an independent reader; rule 10 requires that before filing.
Specifically unaudited: whether 27 of 27 reproduces on a machine that is not
this one, and whether a smaller CMake project shows the same shape - both
worth having before this is filed as a defect rather than as a report.
