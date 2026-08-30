# Recorded end-to-end runs

Upstream #7 asks for end-to-end compilation benchmarks "for perf work
upstream". `bench/e2e.sh` produces a record; this is where records live, so a
number in a document can be traced to a run instead of quoted from memory.

**A record is only comparable with another record built the same way.** The
harness does not clean the store, and `target_prebuilt` alone does not say how
cold a run was - read `derivations_built` beside it. A failed run is written to
`<name>.json.failed` and never to a record here.

| file | what it is |
|---|---|
| `2026-08-30-example-nix.json` | nix 2.36.0pre built per translation unit: 345 tasks, 446 derivations, 371 s wall. The first real subject this harness has had |
| `2026-08-30-example-dynamic-deps.json` | the 4-task example, kept as the small-graph comparison |

## What the nix run says, beyond wall clock

Read the counters rather than the total; the total is one machine on one day.

- **345 tasks, 246 plain `add_drv_to_store` calls, 99 from inside a sandbox.**
  The sandbox calls are the dynamic half - a task re-reading its own drv and
  submitting the updated one - and they cost 1.28 s against the plain calls'
  0.72 s. Per call that is 13 ms versus 2.9 ms.
- **NAR uploads: 1,157 sent of 35,569 offered.** The stamp map absorbs 96.7% of
  them. That is the #18 work paying for itself on a real graph, and it is the
  number to put beside the maintainer's own note ranking `nix store add` the
  biggest bottleneck.
- **Include scans: 1,174 parsed of 86,792 requested.** 98.6% hit the memo. The
  scan is not where the driver's time goes on this package.
- **Resolve: 897 ms for 345 tasks**, against `dyn` at 4.04 s. Derivation
  generation is not the cost here; the dynamic round trips are.
- **79 MiB RSS**, 23 MiB live against 78 MiB retained. The retention gap is
  known and unexplained - see `docs/todo.md`. It wants a profiler, not a guess.

**What this run does NOT establish.** It is one package, one machine, warm
store, single run - no variance, no cold-store figure, and no comparison
against plain ninja, which is the comparison #7's perf work actually wants.
Anyone quoting a speedup from this file is quoting something that was not
measured.
