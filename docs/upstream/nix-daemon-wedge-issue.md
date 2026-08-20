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

nix-daemon 2.35.2: worker children stop responding while holding their locks,
and survive as init-reparented root orphans

The previous title asserted "under sustained concurrent `build_paths`", which
the body then withdraws - we never reproduced it at any concurrency. A title
stating a condition the body retracts is triaged against our own published
table showing healthy at every rung, and closed. Title what was observed.

## Body

### What happens

During a large build driven by a single client issuing many concurrent
`build_paths` requests, `nix-daemon` worker children stop making progress and
never reply. The client blocks forever.

The stuck children show, consistently:

- zero CPU ticks and zero voluntary/involuntary context switches over a 20 s
  sampling window, while alive;
- their build locks still held;
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
   releases its locks. That is the recovery we implemented and it has not yet
   failed for us, though we did not record a per-kill count, so read it as
   consistent behaviour rather than as a tally.

### The part that outlives the build

On 2026-08-20, after a run that hit this, seventeen wedged children survived as
init-reparented orphans owned by root. A single `ps` reading, summed across
the seventeen, put them at roughly 20 GiB of RSS with the largest at 7.5 GiB;
that is one sample rather than a peak or a mean, and RSS double-counts shared
pages, so treat it as an order of magnitude. `SIGTERM` did not clear them. Restarting the
supervised daemon did not clear them, because that kills the listener and not
its orphaned children. Only `kill -9` on the process name did.

This is the part we would most like a maintainer's eye on: whatever the
mechanism, a wedged worker is not confined to the build that produced it and
does not clean up after itself.

### What we could NOT establish

We wrote a standalone reproducer and it has not reproduced the wedge at any
level we have run it at.

In the shape this report describes - one client issuing N concurrent
`build_paths` - that is N = 2 (negative control), 4, 8, 12, 16, 20, 24 and 32,
each against a freshly restarted daemon on an idle 24-thread host with 30.5 GiB
RAM, roughly 25 GiB of it available throughout (sampled from `/proc/meminfo`
before, during and after the top rung; 4.64 GiB of swap was in use and did not
move). We also ran the contrasting shape, N clients issuing one
request each, at N = 2, 8, 12, 16, 20 and 24. Every round completed and no
process went dead.

Two caveats we would rather state than have found:

- **We have no positive control.** The reproducer has never once produced a
  wedge, so we cannot demonstrate that it WOULD detect one. What we can show is
  that it now resolves individual workers: each one-client round samples N+1
  processes, one daemon child plus its N builders, and the verdict is computed
  per process rather than over the round.
- **Earlier versions of it could not have seen this bug even if it had
  happened**, twice over, and both times in the direction that reports healthy.
  It judged each daemon child by the CPU its whole subtree accumulated, and
  under one client there is a single child forking every builder, so one dead
  worker among N live siblings was masked by theirs - and the failure we are
  reporting is partial, some workers stuck while the build moved, which is
  precisely that case. Then, having fixed the unit, it still subtracted one
  subtree total from another, so a stuck parent whose builder EXITED between
  samples had a shrinking total and a negative delta and was never flagged -
  which is what our orphans actually were. Both fixed, deltas are now computed
  per process, and the ladder was re-run from scratch after each. We volunteer
  this because it is the kind of thing that should make you discount a negative
  result, and you would be right to.

So we are explicitly NOT claiming a concurrency threshold, and not claiming the
daemon is fine at these levels either. Concurrency alone does not appear to be
the trigger. The reproducer and the full table are at
`scripts/daemon-stress-bisect.py` and `docs/daemon-wedge.md` in our fork, where
the negative result is written up with every instrument defect found on the way
to it, of which there were more than we would like.

Our leading untested hypothesis is memory pressure, because it is the only
candidate that explains both datasets rather than merely differing between
them.

Two readings, in order. Before we bounded what we were sending them, seven
daemon workers measured at roughly 2 GiB RSS each, with the machine down to
2.5 GiB available. We bounded it, and the wedge described above happened
AFTER that: seventeen orphans, one of them at 7.5 GiB. So the pressure did not
go away with the fix, and the post-fix reading is the larger one. Meanwhile the reproducer cannot reach that state: its builders hold a shell
loop counter, and 32 of them concurrently moved `MemAvailable` by about a
quarter of a gibibyte on a host with 25 GiB free. So our negative result is a
concurrency result, and it does not test this hypothesis at all.

A reclaim-driven wedge would fire at any concurrency once memory is tight, and
at none while it is not, which is the shape of both datasets. We have not
tested it yet.

If a maintainer can say "that region takes lock X and can block on Y", that
would very likely be faster than us continuing to bisect from the outside.

### What we should have measured and did not

Stated plainly because it is the first thing a maintainer will ask for, and we
had the specimens in hand:

Seventeen wedged processes were alive on the machine and the only reading taken
off them was `ps` RSS. No `/proc/<pid>/stack`, no `wchan`, no `syscall`, no
`status` signal masks, no `gdb -p` or `eu-stack` backtrace, no `strace`. Each of
those is one command against a pid, needs no reproducer and no positive control,
and any one of them would likely have been worth more than the entire bisect
that followed. The debugger WAS attached during the incident - to the client,
which was the half already understood.

There is also a confound in the comparison, and it is not a minor one: the real
build ran with dynamic derivations and `ca-derivations` throughout, and the
reproducer ran with neither. Those are different code paths. So the negative
result is not a bisect of the incident; it is a separate experiment that shares
a symptom description. We would not blame anyone for stopping reading there.

If this recurs, the first action is a stack from one specimen, before anything
else.

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

**Everything below this line is OUR record and is not part of the filed text.**
Two rounds of hostile review said the same thing about it: a large block about
defects in our own instrument, inside an issue body, reads to a maintainer as
"these measurements are unreliable" and discounts the primary observation along
with the rest. One sentence carries what a reader needs, and it is already in
the body: the reproducer has no positive control for the incident's shape, so
treat its negative as no evidence.

---

## Audit

Three adversarial rounds, 2026-08-20, all against the tree rather than the
prose. None signed off. Rounds 2 and 3 each found a LIVE defect in the
reproducer rather than in a sentence, and round 5 found four more, and both were false-healthy holes in the
shape this report describes; both are fixed and both re-runs are in
`../daemon-wedge.md`.

Applied to this file across the three rounds: the memory figure (stated
"apiece" where the measurement is per-worker across seven, a 7x error, and
citing the pre-fix reading when the post-fix one is later and larger); the
coverage claim (three different wrong totals in three rounds, now enumerated
per shape and never counted); the instrument-defect count (now stated once, in
`daemon-wedge.md`, and linked from here); the task count, unsourced in four
places; and the two caveats a reader deserves without having to derive them -
that there is no positive control, and that earlier oracles were blind to this
exact bug.

Round 4 (2026-08-20) closed the four measurable items round 3 left open. Three
of the four were closed by DELETING a claim rather than by supporting one,
which is the honest ratio for a set of sentences nobody had measured:

- **"no kernel `flock` waiter" is gone.** A search for its origin found the
  phrase in three of our own files and in no record anywhere - no `/proc/locks`
  capture, no log. It had propagated purely by restatement. It was the most
  specific-sounding line in the symptom list, which is exactly why it would
  have been the first thing a maintainer tried to use;
- **"verified per kill" is softened** to consistent behaviour, for the same
  reason: no count was recorded, so the tally was an invention;
- **the orphan RSS figures now carry their method** - one `ps` sample, summed,
  RSS double-counting shared pages;
- **the host memory state is now measured rather than asserted.** The claim was
  "swap drained". It was false: 4.64 GiB of swap was in use throughout and did
  not move. `/proc/meminfo` was sampled before, during and after a re-run of the
  top rung, and the figures replace the assertion. That re-run also produced the
  number that makes the memory hypothesis legible - 32 concurrent builders cost
  about a quarter of a gibibyte - and therefore sharpens the caveat rather than
  the claim.

Round 5 (2026-08-20) was four independent hostile reviews and none signed off.
It found four more live defects in the instrument, listed in
`../daemon-wedge.md`, one of them the reason this report's negative result was
weaker than it read: an init-reparented wedged child left the sampled
population entirely, which is this incident's own shape. That round also built
the first positive control, so the "we have no positive control" caveat above
is now narrower than it was and is stated as it stands.

The tracker search is DONE, and it was doable from here after all - the forge
CLI reads issues without filing anything. Saying otherwise was a hedge. Related
open issues on NixOS/nix, checked 2026-08-20:

- **#11979**, "Concurrent instances of the same store hang on
  `LocalStore::createTempRootsFile()`" - an flock deadlock under concurrent
  store access. Reported without a daemon, so not the same configuration, but
  it is the nearest named mechanism and it is in the lock path. Whether ours is
  the same is exactly what a `/proc/<pid>/stack` would have answered;
- **#16222**, "nix-daemon leaks GC temproots files per connection" - a
  regression introduced in 2.34.0, fixed by PR 16223 merged 2026-07-27. The
  mechanism is that the per-connection worker in `daemonLoop()` exits via
  `exit(0)`, so `~LocalStore()` never runs. That is a daemon worker not
  cleaning up after itself, in the version range we are on, and our own machine
  shows 77 stale temproots files with no daemon children alive. Not the wedge,
  but it is the same worker-lifecycle code and it should be ruled in or out
  before filing;
- **#7297**, "Hang on large set of recursive-nix builds" - three years open,
  same family of symptom at scale;
- **#16005**, an `!awake.empty()` assertion in `Worker::run()` with `buildPaths`
  in the trace, which is the loop that would go quiet.

STILL OPEN, and these now block rather than the search:

- no stack, wchan or syscall reading from any wedged process. Everything else
  here is secondary to that;
- the `ca-derivations` / dynamic-derivations confound between the incident and
  the reproducer;
- whether the orphan half should be filed separately. Two reviewers said yes
  independently: it stands alone with no hypothesis and no reproducer, it is
  actionable whatever causes the wedge, and it is the half a maintainer would
  triage rather than close.

Status: NOT SENDABLE. Not on the tracker search, which is done, but on the
missing specimen readings and the confound above.
