# Recorded end-to-end runs

Upstream #7 asks for end-to-end compilation benchmarks "for perf work
upstream". `bench/e2e.sh` produces a record; this is where records live, so a
number in a document can be traced to a run instead of quoted from memory.

**A record is only comparable with another record built the same way.** The
harness does not clean the store, and `target_prebuilt` alone does not say how
cold a run was - read `derivations_built` beside it. A failed run is written to
`<name>.json.failed` and never to a record here.

No records are committed here. A record is machine-specific and comparable
only with another taken the same way, so the directory ships empty and each
reader fills it from their own runs.

## The harness reads more than a stock driver prints

`e2e.sh` parses two things: a `nix-ninja: resolved ...` line and a
`nix-ninja-stats {...}` JSON line carrying per-phase counters. **A driver
without that instrumentation emits neither.** The harness then degrades to
wall clock plus a count of derivations built, with every per-phase field
absent - absent rather than zero, because "the phase took no time" and "the
line was not there" are different facts.

The instrumentation is a separate change and is not in this one.

## Reading a record

Two fields say how much work happened and BOTH are needed. `target_prebuilt`
alone is not enough: two runs of `example-hello` both reported it false while
taking 312 s and 18.6 s, because the first built the whole closure and the
second reused it. False does not mean cold. Read `derivations_built` beside
it, and treat `null` there as "the count could not be taken", never as zero.

A failed build is written to `<name>.json.failed` and never into a record, so
a fast failure cannot be averaged into a set of fast successes.
