# DRAFT issue for NixOS/nix

Not filed. Audit status: see the foot of this file.

Target: `NixOS/nix`, not this repository. The observation is about
`nix-daemon` and reproduces nothing in nix-ninja's own code; nix-ninja is
where it was noticed and where the workaround lives.

Before filing, re-check that the version below is still the one being run, and
search their tracker for `build_paths` / daemon hang duplicates. This has not
been done: it needs their issue tracker, and searching it is part of filing
rather than part of staging.

---

## Title

nix-daemon 2.35.2: worker children stop responding under sustained concurrent
`build_paths`, holding their locks, with no error and no log line

## Body

### What happens

During a large build driven by a single client issuing many concurrent
`build_paths` requests, `nix-daemon` worker children stop making progress and
never reply. The client blocks forever.

The stuck children show, consistently:

- zero CPU ticks and zero voluntary/involuntary context switches over a 20 s
  sampling window, while alive;
- their build locks still held;
- no kernel `flock` waiter recorded against them;
- nothing written to the daemon log.

The client sees only that `build_paths` never returns. Every worker thread in
the client parks in that call.

### Why this is not "the build is slow"

Three independent readings, all pointing the same way:

1. `/proc/<pid>/stat` deltas over 20 s: the children are not merely idle
   waiting on I/O, they accrue no time at all.
2. Attaching a debugger to the client (which had to be launched as the
   debugger's own child, since yama blocked attach) put all 20 runner threads
   in `build_paths`.
3. Closing the client connection immediately kills the stuck child and
   releases its locks. Verified per kill. That is the recovery we implemented,
   and it works every time.

### The part that outlives the build

On 2026-08-20, after a run that hit this, seventeen wedged children survived as
init-reparented orphans owned by root, holding roughly 20 GiB of RSS between
them, the largest at 7.5 GiB. `SIGTERM` did not clear them. Restarting the
supervised daemon did not clear them, because that kills the listener and not
its orphaned children. Only `kill -9` on the process name did.

This is the part we would most like a maintainer's eye on: whatever the
mechanism, a wedged worker is not confined to the build that produced it and
does not clean up after itself.

### What we could NOT establish

We wrote a standalone reproducer and it does not reproduce. Six concurrency
levels from 2 to 32, in both request shapes (N clients issuing one
`build_paths` each, and one client issuing N concurrently), against a freshly
restarted daemon on an idle 24-thread host with swap drained: every round
healthy, every build completed, no child ever read zero ticks.

So we are explicitly NOT claiming a concurrency threshold. Concurrency alone
does not appear to be the trigger. The reproducer and its result table are at
`scripts/daemon-stress-bisect.py` and `docs/daemon-wedge.md` in our fork, and
the negative result is written up there as carefully as the positive one,
including four defects in the instrument itself that each produced a
convincing "healthy" before it measured anything real.

Our leading untested hypothesis is memory pressure, because it is the only
candidate that explains both datasets rather than merely differing between
them: the real build's workers were measured at 14 GiB apiece before we bounded
what we were sending them, and the wedge coincided with the machine in swap,
while the reproducer's builders hold a shell loop counter. A reclaim-driven
wedge would fire at any concurrency once memory is tight, and at none while it
is not, which is the shape of both datasets. We have not tested it yet.

If a maintainer can say "that region takes lock X and can block on Y", that
would very likely be faster than us continuing to bisect from the outside.

### Environment

- `nix-daemon` 2.35.2, multi-user, store owned `root:nixbld`
- x86_64-linux, 24 threads, 30.5 GiB RAM
- client: a Rust program issuing concurrent `build_paths` over the daemon
  protocol, up to 20 in flight
- dynamic derivations and `ca-derivations` in use throughout the real build,
  and in neither case in the reproducer

### What we are doing about it meanwhile

Bounding the wait and dropping the connection on timeout, then retrying on a
fresh one. Two things learned doing that, in case they are useful signal:

- the mass reap of killed children made fresh `connect` calls time out, so the
  recovery has to tolerate its own side effects;
- retrying every outstanding request at once recreates the condition
  deterministically, so retries are now gated through a two-permit semaphore
  while first-attempt traffic is uncapped. Fresh children born at the retry
  instant were observed dead-asleep within seconds.

That second one is the closest thing we have to a controlled trigger, and it
still needs the real workload to show up.

---

## Audit

Status: NOT YET AUDITED. Do not file until an adversarial reader has attacked
every claim above and signed off. The claims most exposed to a hostile read:

- "no kernel flock waiter" - state how that was determined, or drop it;
- the 20 GiB orphan figure - it is a sum across seventeen processes from one
  `ps` reading, and should say so;
- "verified per kill" - name how many kills, or soften it;
- the environment section says the client issues up to 20 concurrent
  `build_paths`, which is where the retired "~20 concurrent" figure came from.
  Make sure it reads as the client's configured limit and not as a measured
  threshold, because that is exactly the conflation this draft exists to avoid.
