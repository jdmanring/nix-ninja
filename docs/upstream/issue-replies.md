# DRAFT replies to open nix-ninja issues and PRs

Not posted. Sessions stage; James posts. Issue numbers and states are as read
on 2026-08-20, when `origin/main` was `9a07e67`.

Each draft below is written to be useful on its own to the person who opened
the thread, rather than as an advertisement for this fork. Where the answer is
"we do not have this", it says so.

---

## Into #7, "nix-ninja takes ~6m, same as ninja"

And #4 (derivation-generation benchmarks) is the same reply; post to one and
link from the other.

> We have profiler numbers from a much larger graph than the examples here, in
> case they are useful: qtwebengine 6.11.1, about 15,800 tasks.
>
> Two hotspots dominated the driver, both found with `perf` against the live
> driver rather than by reading, and both in the same place - the include scan.
>
> **1. The virtual-path lookup was a pairwise scan.** Sampling at task 10,500
> put 25% of driver CPU in `Components::next_back`. `canonicalize_cached` in
> `deps-infer` compares its virtual-paths map PAIRWISE with a full `Path`
> comparison per entry, and that map grows with every materialized output, so
> each include lookup is O(V) in outputs so far. The map's own keyed `get` is
> the same comparison at O(1). The two are equivalent because `PathBuf` hashes
> over the same components its `Eq` compares, which is what makes a differently
> spelled probe (`a/./b`) still find an entry keyed `a/b`; we pin that with a
> test rather than relying on it.
>
> **2. The residual was SipHash over long path keys.** A second sample after
> fixing 1 put about a third of driver CPU in hashing. The include-scan maps
> now use `FxHash`, matching what the driver's other hot maps already use.
> Path keys here are long and adversarial input is not a concern, so the
> default hasher is paying for a property this code does not need.
>
> Net effect, measured between two rounds of the same build: the worklist
> bucket went from decelerating by 44 s per 500-task window to holding flat at
> 5 s.
>
> The reason this does not show up on the examples in this repo is that both
> costs are superlinear in graph size - at a few hundred tasks the scan is free
> and the hashing is noise. If the question in this issue is "where does the
> time go", the answer at small N may genuinely be "nowhere interesting", and
> the shape only appears past a few thousand tasks.
>
> Both fixes are small and we would be glad to send them as a PR against
> `deps-infer` if that is welcome.

---

## Into #17, "optional depfiles"

Framed as an alternative, not as a solution, because it answers half the issue
by a different route.

> Reporting a different mechanism that covers the second half of this issue, in
> case it is useful before depfiles land.
>
> The half we mean is "on a second run, avoid redoing dependency inference".
> Rather than emitting depfiles as CA outputs, we persist the driver's resolve
> MEMOS across restarts: the inference result is keyed by what the upload
> CONTAINS, and the cache file carries a version header so entries recorded
> under different upload semantics cannot replay.
>
> This does not give you what depfiles give you - it is internal to the driver,
> nothing else can consume it, and it says nothing about incremental
> correctness against a changed source tree. It is strictly the "don't redo the
> scan" half.
>
> One trap worth passing on regardless of which mechanism you pick, because it
> cost us a whole round's cached work silently. Our first version, on a header
> mismatch, had `init` IGNORE the file while `flush` only wrote a header into an
> ABSENT file. So a whole round's entries appended under the stale header and
> were then discarded unread. A mismatch has to REMOVE the file, not ignore it.
> The failure presents as a cold start, which is a legal state that nothing
> reports, so there is no error to read - and any cache with a version header
> inherits this unless the mismatch path deletes.

---

## Into #5, "phony targets"

> Implemented in our fork, and PR #43 also implements it, differently. Since
> both exist, the trade-off may be worth having on the record here.
>
> PR #43 maps a phony's outputs into a per-file `Vec<DerivedFile>`. We instead
> record the phony's inputs as aliases and expand them transitively at INPUT
> ASSEMBLY, so a dependent inherits the phony's inputs as its own derivation
> inputs and the ordering falls out of the nix model rather than being
> maintained separately.
>
> The reason we went that way is two cases that the output-mapping shape does
> not reach on its own:
>
> - the expansion marks inherited inputs as arriving via a phony, which is also
>   where the order-only cut for gcc-style header inputs has to apply;
> - a stamp file certifies that its edge's inputs EXIST but carries no content.
>   On a shared filesystem, depending on the stamp is enough. In a sandbox the
>   certified files have to be materialized too, so a stamp dependency also
>   pulls in the stamp edge's own inputs. We hit this with perfetto's table
>   generator stamping python files that a later script imports from the repo
>   root.
>
> The cost of our shape is at the CLI boundary: a phony records no derived file
> of its own, so naming one as a target needs an explicit expansion step, which
> we added.
>
> We are not claiming ours should win - #43 was here first and does the thing
> the issue asks for. We are flagging that whichever lands should be checked
> against the order-only and stamp cases, because a sandboxed build surfaces
> both and a shared-filesystem build surfaces neither.

---

## Into PR #43

> Thanks for this - we independently needed multiple targets and ended up
> adopting your approach for it.
>
> Specifically: `build()` returning a `Vec`, the explicit "expected exactly one"
> refusal on the paths that can only mean one derivation, dedup by build path,
> and sorting so the result is a function of the target SET rather than of
> argument order. We had a single-target TODO in the same place and your shape
> is the right one; credited in our commit.
>
> One thing we changed while porting it, which may be worth folding in here
> regardless of what happens to the phony half of this PR: the existing
> `let _ = scheduler.want_file(fid)` discards a `Result`, and `want_file` is
> what detects a dependency cycle. So the one error it exists to raise was
> being thrown away, and a cyclic build proceeded and failed later somewhere
> less obvious. Your diff already changes that line to `?` as a side effect of
> the loop; we think it is worth calling out in the description, because it is
> a behavior change rather than a refactor - a graph that previously built
> will now fail early, which is correct but will surprise someone.
>
> On the phony mechanism we went a different way, for reasons that only show up
> under sandboxing; written up in #5 rather than here, since it is a design
> question about the feature rather than about this PR.

---

## Into #52, `-fuse-ld=mold`

An honest negative. Worth posting because the adjacency is real.

> No fix from us, but a pointer that may save whoever takes this some time.
>
> We patch the same code path for a different reason: GN quotes paths in
> generated commands and `which(1)` does not accept quotes, so the resolved
> interpreter token needs its quotes stripped before resolution. That is in the
> `shell_words::split` plus `which` path, which is where a `-fuse-ld=` argument
> would also have to be recognized and its linker resolved to a store path.
>
> So the two fixes land in the same function. Whoever takes one will be
> editing the lines the other needs.

---

## Audit

Status: NOT YET AUDITED. Exposed claims a maintainer could falsify:

- the 25% and one-third figures, and the 44 s / 5 s pair. State the sampling
  method inline (it is `perf` on the live driver at task 10,500) and do not let
  "measured between two rounds" stand in for a controlled A/B - it was not one.
- "PR #43 already changes that line to `?`" - verify against their current
  diff before posting, since a force-push would make it false.
- the #5 claim that output-mapping "does not reach" the two cases. That is our
  reading of their diff, not something we ran. Soften to what we verified.
