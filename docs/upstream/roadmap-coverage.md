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
| Depfile support | #17 | not built. The general solution, and the one that would retire most of PR 1 |
| `mkMesonPackage` configure caching | #16 | not built; we have a measured argument FOR it |
| Writing derivation caching for local mode | - | **partly built**: `resolve_cache.rs` |
| `nix store add` async | #18 | not built |
| Benchmarks for generating derivations | #4 | profiler samples, no benchmark |
| Benchmarks for end-to-end compilation | #7 | profiler samples, no benchmark |

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
builder needs `dontUseMesonConfigure = true` plus explicit `cmakeFlags`. That
is a three-argument workaround every non-meson consumer rediscovers, and
splitting the phase is what removes it. Evidence for their design, from a
consumer, which is worth more than agreement.

## The three genuinely open, in the order they are worth taking

1. **#17 depfiles.** The general solution. `pr-plan.md` already argues that PR
   1's inference rules are heuristics recovering what the build file failed to
   declare, and that depfiles are the real answer - so this is the item that
   makes most of our own largest PR unnecessary. Taking it is worth more to
   both trees than defending the heuristics.
2. **#18 async `nix store add`.** The maintainer's own note calls it "probably
   biggest perf bottleneck". Our driver timers now attribute the realise RPC
   separately, so we can measure whether that holds at our scale, which is a
   cheaper first contribution than the change itself.
3. **#16 configure caching.** Above.

None of the three is started, and none is an easy win. Recording them as a
ranked list rather than a backlog is the point: this file exists so the next
session does not re-derive the ranking or, worse, start item 1 by writing more
inference rules.

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

Not attacked by an independent reader. Status: internal working document, not
for sending. If any row is quoted outward it must be re-read against their
`docs/todo.md` first, which moves without telling us.
