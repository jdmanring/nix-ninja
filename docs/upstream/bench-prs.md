# PR bodies for the two benchmark branches (their #4 and #7)

Copy-pasteable. Each block below is a complete PR title and body; nothing in
them needs editing except where a line says so. Both branches are one commit
off `upstream/main` and both are UNVERIFIED against upstream's own dependency
resolution - see the caveat at the end, which must be resolved before either
is opened.

---

## PR 1 of 2 - branch `pr/bench-e2e` - closes #7

**Title:** `Add an end-to-end compilation benchmark harness`

**Body:**

> #7 asks for benchmarks of end-to-end compilation for perf work. The driver
> already prints everything such work needs; what has not existed is something
> that RUNS a build and records those numbers in a form two people can compare.
>
> `bench/e2e.sh` runs one example end to end and emits a single JSON record:
> wall clock, the driver's own phase breakdown, and the store paths.
> `bench/records/` holds the runs, so a number in a discussion can be traced to
> one instead of quoted from memory.
>
> Three behaviours are deliberate, and each was bought by getting the reading
> wrong first:
>
> - a FAILED build is written to `<name>.json.failed`, never into the record
>   set. `wall_seconds` and `derivations_built` are the fields a comparison
>   reads, and a run that died in four seconds has both.
> - `target_prebuilt` is three-valued. Any `nix path-info` failure used to
>   record `false`, which is the direction that reads as "more work was done"
>   and the wrong way for an unknown to fail. It is also not sufficient alone:
>   two runs of `example-hello` both reported `false` while taking 312 s and
>   18.6 s, because the first built nix's whole closure. Read
>   `derivations_built` beside it.
> - `derivations_built` is `null`, not `0`, when no log is readable. "Nothing
>   was built" and "the count could not be taken" are different facts.
>
> The first real subject is nix itself: 345 tasks, 446 derivations, 371 s, with
> the driver's own counters recorded beside it. Two figures from that run may
> be worth your attention independently of this PR: NAR uploads deduplicate
> 1,157 sent of 35,569 offered (96.7%), and include scans hit the memo 98.6% of
> the time, which says the scan is not where the driver's time goes on that
> package.
>
> What this does NOT establish, stated so nobody quotes it as more: one
> package, one machine, warm store, single run. No variance, no cold-store
> figure, and no comparison against plain ninja, which is the comparison perf
> work actually wants.

---

## PR 2 of 2 - branch `pr/bench-generate` - closes #4, answers #41

**Title:** `Add benchmarks for derivation generation`

**Body:**

> #4 asks for benchmarks of nix-ninja generating derivations. This covers the
> part that runs without a daemon: the ninja graph load and the include scan,
> which are the two phases the driver's resolve breakdown attributes its serial
> cost to and the two that a change to this crate can move.
>
> Scope is stated in the file so no reader mistakes it. It does NOT cover the
> daemon round trip per task; that needs a store and a daemon and belongs with
> the end-to-end harness. A number from here is a claim about the driver's CPU,
> never about a build's wall clock.
>
> The fixture is generated rather than checked in: a corpus large enough to be
> worth timing is not worth storing in git, and a generator states the shape it
> claims to represent where a blob would not.
>
> One result worth attention: **the graph load is superlinear.** 1k edges load
> in ~563 us and 10k in ~7.80 ms, which is 13.9x for 10x the edges. That is
> measured on generated input rather than a real project's `build.ninja`, so it
> is a lead rather than a diagnosis.
>
> It also carries the comparison #41 asks for, since the issue poses the
> question and nothing had measured it: **reading the file wins.** 21.89 us
> against 25.65 us for mmap on a 400 KB input. Both sides touch every byte, so
> neither number is timing a lazy mapping that was never faulted in - that
> mistake was made first and it reversed the result.
>
> `divan` is added as a dev-dependency for the harness and `libc` for the mmap
> comparison. (EDIT THIS LINE if the maintainer prefers `criterion` or would
> rather the benches live outside the workspace.)

---

## The caveat that gates BOTH, and it is not optional

**Neither branch has been compiled against `upstream/main`.** This fork
resolves `n2` from a vendored `vendor-n2/` and upstream resolves it from a git
dependency, so every build done here is a different resolution than a build
there would be. `benches/generate.rs` calls `n2::load::read` and
`deps_infer::c_include_parser::retrieve_c_includes`, both of which must be
public in upstream's `n2` pin and in upstream's `deps-infer`.

A branch that does not compile is worse than no branch, because it looks ready.
Check out each branch, build it against upstream's own lock, and only then
open. `bench/e2e.sh` is a shell script and carries no such risk; the
benchmark PR does.
