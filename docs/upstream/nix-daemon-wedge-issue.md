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

We wrote a standalone reproducer and it does not reproduce. Seven distinct
concurrency levels between 2 and 32, across two request shapes - N clients
issuing one `build_paths` each (2 through 24), and one client issuing N
concurrently (4 through 32) - against a freshly restarted daemon on an idle
24-thread host with swap drained: every round healthy, every build completed,
no child ever read zero ticks.

So we are explicitly NOT claiming a concurrency threshold. Concurrency alone
does not appear to be the trigger. The reproducer and its result table are at
`scripts/daemon-stress-bisect.py` and `docs/daemon-wedge.md` in our fork, and
the negative result is written up there as carefully as the positive one,
including three defects in the instrument itself that we had to fix before any
round meant anything - one of which produced a convincing "healthy" over zero
observed children, and one of which produced a convincing WEDGE on a round
where both builds completed.

Our leading untested hypothesis is memory pressure, because it is the only
candidate that explains both datasets rather than merely differing between
them: the real build's daemon workers were measured at roughly 2 GiB RSS each,
seven of them, taking the machine down to 2.5 GiB available, before we bounded
what we were sending them; and the wedge coincided with the machine in swap,
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

Round 1: 2026-08-20, adversarial review against the tree. NOT a sign-off - it
returned ten blocking findings across this directory, four of them in this
file. Applied here:

- the memory figure said "14 GiB apiece" where the measurement is seven workers
  at ~2 GiB each, machine down to 2.5 GiB available. A 7x inflation in the
  number the whole memory hypothesis rests on, taken from a commit SUBJECT
  where the in-code comment carried the real reading;
- "six concurrency levels from 2 to 32, in both request shapes" - neither shape
  spans that range. Now states the coverage of each shape separately;
- "four defects, each produced a convincing healthy" - there are three, one
  produced a false healthy, one a false WEDGE, and the fourth item was a design
  choice rather than a defect. The issue and its linked evidence disagreed
  about the instrument's own history, which invites exactly the reading a
  negative result cannot survive;
- the ordering argument in `README.md` leaned on async `nix store add` raising
  concurrency into the wedge, which is a different RPC plus a mechanism this
  report exists to disclaim. Replaced with the orphan behavior.

STILL OPEN before filing, and each needs a measurement rather than an edit:

- "no kernel flock waiter" - say how that was determined, or drop it;
- "verified per kill" - name how many kills, or soften it;
- the 20 GiB orphan figure is a sum over seventeen processes from a single `ps`
  reading and should say so;
- search their tracker for duplicates. Not done: it is part of filing.

Status: NOT SENDABLE. Needs a second audit round after the open items above.
