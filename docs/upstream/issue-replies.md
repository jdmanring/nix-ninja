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
> case they are useful: qtwebengine 6.11.1, at least 16,077 tasks. That is the
> highest task index our driver log reaches rather than a total, since we have
> not yet driven the graph to completion.
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
> not need. This is a direct-dependency promotion rather than a new entry in
> your tree: `rustc-hash` is already in `Cargo.lock` at 9a07e67, pulled in by
> n2 itself, so it is compiled in your build today.
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
> One thing in your diff worth calling out explicitly in the description,
> regardless of what happens to the phony half: your target loop replaces the
> existing `let _ = scheduler.want_file(fid)` with `?`. That discarded
> `Result` matters more than it looks, because `want_file` is what detects a
> dependency cycle. So the one error it exists to raise was
> being thrown away, and a cyclic build proceeded and failed later somewhere
> less obvious. So that line is a behavior change rather than a refactor: a
> graph that previously built will now fail early, which is correct and will
> surprise someone. We hit the same thing porting your target loop and kept
> your fix.
>
> On the phony mechanism we went a different way, for reasons that only show up
> under sandboxing; written up in #5 rather than here, since it is a design
> question about the feature rather than about this PR. No comment from us on
> the CMake example half, which is the part of this PR that closes #20 - we
> have not exercised it.

---

## Into #52, alternate linkers (`CC_LD` / `CXX_LD`, mold as the example)

The reporter has the diagnosis and a workable fix in the issue body. Three
things are added here that he does not have, all read from nixpkgs
`fd14620` on 2026-08-21 rather than from memory, and each falsifiable from a
nixpkgs checkout in one grep.

> Three notes from a fork driving nix-ninja at qtwebengine scale, since we
> patch the same `shell_words::split` + `which` path for a different reason.
>
> **1. The provenance is wider than `CC_LD`/`CXX_LD`, and the wider route is
> the one that lands in `build.ninja` directly.** In nixpkgs a package can
> write the flag itself, with no environment variable anywhere. `fex`
> (`pkgs/by-name/fe/fex/package.nix`) has `cmake` and `ninja` in
> `nativeBuildInputs` and writes `set(CMAKE_EXE_LINKER_FLAGS_INIT
> "-fuse-ld=lld")` into a toolchain file, so every link rule in its generated
> `build.ninja` carries `-fuse-ld=lld` from the package definition. The
> `useMoldLinker` and `useGoldLinker` stdenv adapters
> (`pkgs/stdenv/adapters.nix`) do the same through `NIX_CFLAGS_LINK`. A fix
> keyed on the two environment variables would be correct for the dev-shell
> case and silent on these. Scanning the evaluated command, which is what
> your last paragraph proposes, catches both - worth stating explicitly,
> because the title says `CC_LD` and an implementer may follow the title.
>
> **2. The flag is present in the WORKING case too, so a scan that resolves
> unconditionally can pull in the wrong linker.** `useMoldLinker` overrides
> `stdenv.cc.bintools` to symlink `ld.mold` into the bintools wrapper's own
> `bin`, and separately appends `-fuse-ld=mold`. The flag there is not a
> problem, because the tool is already inside a store path the toolchain
> carries; the reported failure is the same flag with the tool reachable only
> through ambient PATH. Since `which` reads the driver's ambient PATH, a scan
> that sees `-fuse-ld=mold` and resolves `mold` can bind a DIFFERENT mold
> store path than the one that stdenv baked in. That links successfully with
> the wrong tool, where the bug you reported at least fails loudly. Whatever
> the fix does about the ambient case, it should leave the already-reachable
> case alone.
>
> **3. Scope it to the documented spellings and say where the edge is.**
> `-fuse-ld=<name>` and `--ld-path=<path>` are documented gcc/clang flags and
> a small fixed set. `-B<dir>`, `LD=`, and `COMPILER_PATH` reach the same
> outcome and are not bounded that way. Handling the first two and reporting
> the rest as an unhandled case is worth more than trying to be complete: an
> unhandled spelling fails with a build error naming a missing tool, which is
> the safe direction, and it is the direction this already fails in.
>
> One collision to expect: GN quotes interpreter paths in generated commands
> and `which(1)` does not accept quotes, so we strip quotes off the resolved
> token before resolution. That is the same few lines a `-fuse-ld=` scan
> would edit.

Not implemented in our fork, and that is a deliberate no rather than a
backlog entry. Nothing we build uses an alternate linker - the one
`-fuse-ld=` our own tree meets is nixpkgs' `useLdGold` on ghc, which we
strip - so implementing it here would be a rule with no consumer, on a fork
whose reviewability problem is its size.

---

## Audit

Three adversarial rounds, 2026-08-20. None signed off. Applied to this file:

- **the 44 s / 5 s net-effect figure does not exist anywhere in the tree** and
  was deleted. Worse, the large measured wins in that area come from commits
  NOT in the proposed perf PR, so a maintainer who merged it and benchmarked
  would have failed to reproduce our headline number. The reply now offers an
  isolated before/after run rather than quoting one we cannot attribute;
- **"matching what the driver's other hot maps already use" was false against
  UPSTREAM's tree.** No crate under their `crates/` uses FxHash. One grep and
  the reader stops. Reworded. Round 4 then corrected the correction: the
  reply called this an added dependency, and `rustc-hash` is already in their
  `Cargo.lock` as a dependency of n2, so it is a direct-dep promotion and not
  a new tree entry. Saying otherwise overstates the cost of our own patch,
  which a maintainer checks in one grep;
- the 25% and one-third figures are `perf` samples from a single run, not a
  controlled A/B, and now say so inline. Both trace to bare one-line commits
  with no recorded method, which is a weakness in our own record;
- #7 was quoted under a title it does not have;
- the #52 reply handed the reporter back his own diagnosis and his own
  proposed fix, the one condescending passage in the set. Cut to the single
  thing we add;
- credit: PR 56 to amaanq rather than to obsidiansystems, which is only the
  head repo; PR 43's CMake half acknowledged rather than passed over; and the
  `want_file` fix attributed to their diff, where an earlier version of our own
  commit message had claimed it in passing.

Verified clean across the rounds: that PR 43 already changes `want_file` to
`?`; every design claim in the #5 reply against `task.rs`; #43's age and
comment count.

Round 5 (2026-08-20), read from the maintainer's chair, sorted these by whether
they help the person in the thread:

- **#7/#4 and #52 are sendable today.** #7 answers the maintainer's own open
  question with a stated method and refuses to quote a number it cannot
  attribute; #52 is one fact the reporter does not have. Both were called out
  as the right shape;
- **the PR 43 reply needs its opening cut.** "We independently needed multiple
  targets and ended up adopting your approach" is our status, not the
  contributor's business. Lead with the `want_file` finding, which is the part
  that helps them;
- **#17 should be trimmed to the header-mismatch trap.** The first two thirds
  describe a mechanism that lives only in our fork and that nothing else can
  consume, posted into an issue about a feature we chose not to build. As
  written it reads as "here is what we did instead of your issue";
- **the #5 reply should be cut to its last two sentences, or not posted.** Read
  as its author, it gives their design one sentence and ours four paragraphs,
  in a thread where their PR has sat since February with no maintainer comment.
  "We are not claiming ours should win" does not undo that; it is the tell. The
  only part they can act on is the closing point that whichever lands should be
  checked against the order-only and stamp cases. Send that.
- **the dependency claim was corrected**, see above. `rustc-hash` is already in
  their lock via n2.

Round 6 (2026-08-21) re-attacked the #52 reply, which round 5 had called
sendable, and found the reply true but thin: it offered one collision and no
evidence. Rebuilt on three readings of nixpkgs `fd14620`, each checkable by
the maintainer in one grep. Two of the three came from attacking our own
first draft rather than the issue:

- the first version cited ghc's `useLdGold` as the counter-example to the
  `CC_LD` framing. Ghc builds with hadrian, not ninja, so the instance was two
  steps off the reported mechanism and rejectable in one sentence. Replaced
  with `fex`, which has `cmake` and `ninja` in `nativeBuildInputs` and writes
  `-fuse-ld=lld` into a toolchain file - in-graph, no environment variable;
- the first version was going to recommend `useMoldLinker` as the correct nix
  answer. Reading `pkgs/stdenv/adapters.nix` killed that: the adapter appends
  `-fuse-ld=mold` itself. What makes it work is the bintools symlink, not the
  absence of the flag - which turns the recommendation into the reply's
  strongest point, that the flag is present in the working case too and an
  unconditional scan can bind the wrong mold;
- the claim that the flag set is "closed" was cut. `-fuse-ld=` and
  `--ld-path=` are documented and small; `-B`, `LD=` and `COMPILER_PATH` are
  not, and a reader names that edge if the draft does not.

Status: NOT SENDABLE as a set. The perf reply still owes a decision on the
isolated before/after run it offers, and the #5 and #17 replies need cutting
before anyone posts them. #52 and #7/#4 remain the two that are ready.


---

## Into #31, "example-hello fails to build in x86_64 Linux container"

Added 2026-08-21. **This issue had no presence anywhere in this directory
before today**, and neither did #6 or #14 - see the audit note at the foot of
`roadmap-coverage.md` for why: the coverage index was built against
`docs/todo.md`, and the todo file is not the issue list.

It is worth a reply because the thread STOPPED on an unanswered failure rather
than on a resolution. The title names an error the reporter got past. The last
comment, smpdt on 2025-06-23 and unanswered for fourteen months, is a different
one:

```
error: Cannot build '/nix/store/...-ninja-build-hello.p-main.cpp.o.drv'.
       Reason: builder failed with exit code 1.
       > NIX_BUILD_TOP /tmp/nix-build-...
       > Error: Permission denied (os error 13)
```

with the reporter's own guess: "I wonder if Docker's sandboxing doesn't play
nice with recursive nix".

### The reply

> Worth retesting rather than diagnosing from here, because the mechanism this
> failure is about has been replaced since the thread stopped.
>
> When you hit this, a task reached the daemon through recursive-nix from
> inside its own build. #49, "nix-ninja: route inside-derivation calls through
> builder-rpc-v0", merged 2026-07-23 - thirteen months after this comment - and
> changed that path. Your guess that Docker's sandboxing and recursive-nix were
> the pair in conflict is pointing at the mechanism that is no longer the one
> in use, which is a reason to re-run rather than a reason to close.
>
> One thing worth checking BEFORE the rerun, because it fails much later and
> with a message that does not name the cause: whether the daemon you end up
> with advertises `builder-rpc-v0`. Every dynamic task requires it by name, a
> daemon without it refuses the derivation, and the refusal does not say
> "your daemon is too old". The whole check is a derivation that requires the
> feature - if it is denied, the refusal prints the daemon's entire available
> feature set, which is the list you want to read.
>
> Note this is a property of the DAEMON binary rather than of your client or
> your `experimental-features` line: the name is not grantable through
> `system-features`, so the only lever is which nix the daemon is. In a
> container that is whichever nix the image starts, which is the same class of
> problem the original report in this thread turned out to be.

### What this reply does NOT claim

It does not say #31 is fixed. Nobody here has run nix-ninja in a Docker
container, and the reporter's environment is not reproduced. The claim is
narrower and is checkable by them: the mechanism their hypothesis names was
replaced by a merged PR, so the evidence in the thread no longer describes the
code. Calling it fixed outright would be a guess about somebody else's
environment presented as a finding, which is the thing this directory keeps
catching in itself.

## The two remaining uncovered issues, and neither is ours

**#6, "Investigate if it's possible to use snix as an alternative backend"**
(`help wanted`). Not ours. `CONTRIBUTING.md` already states the blocker -
snix's `nix-compat` supports Nix 2.3, which has neither content-addressed nor
dynamic derivations - and nothing this fork did bears on it. Recorded as
deliberately unaddressed rather than left invisible.

**#14, "aarch64 support"**. Not ours, for a reason that is a fact about the
hardware rather than a judgment: everything this fork has measured ran on one
x86_64 workstation. Offering anything into an architecture issue from a tree
that has never been built on that architecture would be exactly the
untested-claim shape the rest of this directory exists to avoid.

## #31 (example-hello fails in container: "Building dynamic derivations in one shot is not yet implemented")

Draft reply, staged 2026-08-23, not sent:

That error is nix's, not nix-ninja's: the daemon serving the build has to
implement building a dynamic derivation end to end, and the nixos/nix
container image ships a nix that does not. The configuration this fork
has driven ~1,200-package builds through:

- daemon: nix 2.36.0pre (the `builder-rpc-v0` system feature is in 2.36.0pre
  and not in 2.35.x; it is not grantable via `system-features` - both
  versions silently drop the name - so the daemon binary is the only lever)
- `experimental-features = nix-command flakes ca-derivations
  dynamic-derivations recursive-nix` in the DAEMON's nix.conf (the
  client's file does not reach the daemon; `--extra-experimental-features`
  on the invocation covers the client side)

A four-second probe that answers whether a given daemon qualifies, without
building anything real: require a nonexistent system feature and read the
`Available features:` list in the refusal.
