# DRAFT replies to open nix-ninja issues and PRs

Not posted. Sessions stage; James posts. Issue numbers and states are as read
on 2026-08-20, when `origin/main` was `9a07e67`.

Each draft below is written to be useful on its own to the person who opened
the thread, rather than as an advertisement for this fork. Where the answer is
"we do not have this", it says so.

---

## Into #7, "Add benchmarks for end-to-end compilation of NixOS/Nix for perf work"

The body asks why nix-ninja comes out about the same as ninja. #4
(derivation-generation benchmarks) gets the same reply; post to one and link
from the other.

> We have profiler numbers from a much larger graph than the examples here, in
> case they are useful: qtwebengine 6.11.1, about 15,800 tasks.
>
> Two hotspots dominated the driver, both found with `perf` against the live
> driver rather than by reading, and both in the same place - the include scan.
>
> Method, since it decides how much these are worth: both are `perf` samples
> against the driver while it was running, not a controlled A/B. The first was
> taken at task 10,500 of one run, the second after fixing the first, in the
> same run.
>
> **1. The virtual-path lookup was a pairwise scan.** That sample put 25% of
> driver CPU in `Components::next_back`. `canonicalize_cached` in
> `deps-infer` compares its virtual-paths map PAIRWISE with a full `Path`
> comparison per entry, and that map grows with every materialized output, so
> each include lookup is O(V) in outputs so far. The map's own keyed `get` is
> the same comparison at O(1). The two are equivalent because `PathBuf` hashes
> over the same components its `Eq` compares, which is what makes a differently
> spelled probe (`a/./b`) still find an entry keyed `a/b`; we pin that with a
> test rather than relying on it.
>
> **2. The residual was SipHash over long path keys.** The second sample put
> about a third of driver CPU in hashing. We switched these maps to `FxHash`
> (`rustc-hash`): the keys are long paths, and adversarial input is not a
> concern here, so the default hasher is paying for a property this code does
> not need. That does add one dependency, which we would understand you
> wanting to weigh against the gain.
>
> We are deliberately not quoting an end-to-end number for these two. We have
> one from our own tree, but our tree carries other performance work in the
> same area, so we cannot honestly attribute a whole-run delta to just these
> two commits without re-running them in isolation. If a before/after on a
> clean base would help you evaluate it, say so and we will do that run.
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
> question about the feature rather than about this PR. No comment from us on
> the CMake example half, which is the part of this PR that closes #20 - we
> have not exercised it.

---

## Into #52, alternate linkers (`CC_LD` / `CXX_LD`, mold as the example)

The reporter already has the diagnosis and the fix in the issue body. The only
thing we can add is the collision, so that is all this says.

> Heads up on a collision, since we patch the same code path for a different
> reason: GN quotes paths in generated commands and `which(1)` does not accept
> quotes, so the resolved interpreter token needs its quotes stripped before
> resolution. That is the same `shell_words::split` plus `which` path where the
> `-fuse-ld=` handling you describe would go, so whoever takes this will be
> editing the lines that fix needs.

---

## Audit

Round 1: 2026-08-20, adversarial review against the tree. NOT a sign-off.
Findings applied to this file:

- **the 44 s / 5 s net-effect figure does not exist anywhere in the tree** and
  was deleted. `git log --all` has no such reading. Worse, the large measured
  wins in this area come from commits that are NOT in the proposed perf PR, so
  a maintainer who merged it and benchmarked would have failed to reproduce our
  headline number - falsified by the reviewer's own measurement, which is the
  worst way to lose a PR;
- **"matching what the driver's other hot maps already use" was false against
  UPSTREAM's tree.** There is no FxHash anywhere in their crates; the other hot
  maps are ours. One grep and the reader stops reading. Reworded, and the added
  `rustc-hash` dependency is now disclosed rather than left in the diff;
- the 25% and one-third figures are `perf` samples from a single run, not a
  controlled A/B, and now say so inline. Both come from bare one-line commits
  with no recorded method, which is a weakness in our own record, not just in
  the prose;
- #7 was quoted under a title it does not have. Real title restored;
- the #52 reply handed the reporter back his own diagnosis and his own proposed
  fix, which is the one condescending passage in the set. Cut to the single
  thing we actually add, the merge collision;
- PR 43's CMake half now gets an acknowledgment instead of silence, and PR 56
  is credited to amaanq rather than to obsidiansystems, which is only the head
  repo.

Verified clean in the same pass: the claim that PR 43 already changes
`want_file` to `?` is TRUE against their current diff; every design claim in
the #5 reply exists in `task.rs`; #43's age and comment count are accurate.

Status: NOT SENDABLE until a second round. The perf reply in particular should
not go out until we decide whether to do the isolated before/after run it now
offers.
