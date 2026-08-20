# The nix-daemon wedge: what is observed, and what is not reproduced

Status as of 2026-08-20: the incident is real and recorded. The standalone
reproducer does **not** reproduce it, at any concurrency tested. Those are two
separate facts and this document keeps them separate, because the upstream
report is only as strong as the weaker of the two.

## What was observed, during the build

Measured during a qtwebengine 6.11.1 build of at least 16,077 tasks driven by nix-ninja
against nix-daemon 2.35.2:

- daemon child processes stop making progress while holding their locks: zero
  CPU ticks, zero context switches, no kernel flock waiter, and no reply ever
  sent;
- dropping the client connection kills the stuck child and frees its locks.
  That is the recovery `nix-builder-rpc-client`'s watchdog implements;
- a mass retry that re-issues every outstanding request at once recreates the
  condition, which is why the watchdog gates retries through a two-permit
  semaphore;
- the stuck children can outlive their supervisor. On 2026-08-20 seventeen of
  them survived as init-reparented root orphans holding roughly 20 GiB of RSS
  between them, one at 7.5 GiB. `SIGTERM` did not clear them, and neither did
  `s6-svc -kr`, which kills only the supervised parent. `pkill -9 -x
  nix-daemon` did.

That last point is why the incident is worth reporting whatever the mechanism
turns out to be: the failure is not confined to the build that triggers it, and
it does not clean up after itself.

## What the reproducer measured

`scripts/daemon-stress-bisect.py` drives N concurrent builds against a freshly
restarted daemon and reads CPU ticks from `/proc` to separate a working process
from a dead-asleep one. Re-run in full on 2026-08-20 after the oracle was
fixed (see below); every verdict recorded before that fix is void and is not
reproduced here.

Host: 24-thread Ryzen 9 7900X3D, 30.5 GiB RAM, nix-daemon 2.35.2, no other
build running, swap drained to zero.

**One client issuing N concurrent `build_paths`** - the shape the incident
describes:

| N | processes sampled | fully-dead subtrees | verdict |
|---|---|---|---|
| 2 (control) | 3 | 0 | healthy |
| 4  | 5  | 0 | healthy |
| 8  | 9  | 0 | healthy |
| 12 | 13 | 0 | healthy |
| 16 | 17 | 0 | healthy |
| 20 | 21 | 0 | healthy |
| 24 | 25 | 0 | healthy |
| 32 | 33 | 0 | healthy |

**N clients issuing one `build_paths` each** - kept as the contrast, since the
pair is what would separate a per-connection fault from a per-daemon one:

| N | daemon children | processes sampled | fully-dead subtrees | verdict |
|---|---|---|---|---|
| 2 (control) | 2  | 4  | 0 | healthy |
| 8  | 8  | 16 | 0 | healthy |
| 12 | 12 | 24 | 0 | healthy |
| 16 | 16 | 32 | 0 | healthy |
| 20 | 20 | 40 | 0 | healthy |
| 24 | 24 | 48 | 0 | healthy |

Sampled is 2N here, one child and one builder each, against N+1 in one-client
mode. That difference is the whole reason the two modes exist, and it is also
why the superseded per-child oracle would still have caught a wedge in THIS
shape while missing it in the other: with one builder per child, a child's
subtree total and its builder's are the same reading.

Read the coverage from the rows rather than from a count: this document has
twice carried a total that contradicted its own table. Neither shape spans the
whole range on its own, and each has its own N=2 negative control.

**The "processes sampled" column is the load-bearing one.** It is N+1 in every
one-client row: one daemon child plus its N builders. That is the evidence the
oracle can see individual workers at all, which it could not before the fix
below, and it is the column that would expose the next instrument defect the
way it exposed the first three. A verdict without its population is not a
reading.

## Four defects in the instrument, and one choice that avoided a fifth

Counted carefully, because the count itself has been wrong in this document
three times, and an instrument whose own history is told inconsistently invites
the one reading a negative result cannot survive: that it was never trusted.

They did NOT all fail the same way. Two produced a false HEALTHY, one produced
a false WEDGE, one measured the wrong request shape entirely. Item 2 is not a
defect at all; it is the choice that kept the fix to item 1 from creating a
false wedge by construction, and it is listed because getting it wrong was the
obvious move.

1. **The workload could not hold a daemon child open.** The stress derivation
   wrote a file and exited inside the settle window, so the tick oracle never
   had a subject and only build-completion was doing any work. The first three
   rounds returned "healthy" while observing ZERO daemon children. The builder
   now burns CPU for `--spin` iterations. The default of 15,000,000 was
   calibrated on this host and nowhere else: 3,000,000 iterations timed at
   4.1 s, so 15,000,000 is about 20 s, comfortably past `--settle` plus
   `--interval`. It is a shell loop, so it will time differently on other
   hardware; the property that matters is that a builder outlive the sampling
   window, and the `processes sampled` column is what shows whether it did.

2. **Not a defect: it burns rather than sleeps, deliberately.** The obvious
   repair for item 1 is a sleeping builder, and a sleeping child reads zero
   ticks, which is the wedge signature itself, so that repair would have
   reported every healthy round as a wedge.

3. **The oracle read the wrong process, and this one produced a false WEDGE.**
   With children finally visible, N=2 reported both as zero-tick while both
   builds completed. A daemon child does not run the build: it forks the
   sandboxed builder and blocks in `wait()`, so its own ticks stay flat for the
   whole build and a healthy child is indistinguishable from a wedged one at
   that level.

4. **Reading each child's SUBTREE SUM fixed item 3 and made the reported
   failure invisible.** Under `--one-client` there is exactly ONE daemon child,
   forking every builder, so the round collapses to a single sum over N burning
   processes: one dead-asleep worker among live siblings is masked by their
   CPU, and the verdict can only trip if every builder stops at once. The
   incident is PARTIAL - seventeen children wedged while the rest of the build
   moved - so the shape that matches the incident was exactly the shape the
   oracle was blind in, and "no child read zero ticks" was a statement over a
   population of one. Found by an adversarial audit rather than by the
   instrument. The unit is now every process in the tree, judged by whether its
   own subtree is entirely dead: a parked child with a burning builder is not
   flagged, a dead builder among live siblings is. A selftest fixture pins it
   by asserting both readings.

5. **The default mode drives the wrong SHAPE.** Launching N separate `nix
   build` processes opens N daemon *connections* carrying one `build_paths`
   each, while the incident describes many requests on ONE connection and
   prescribes dropping that connection as the recovery. `--one-client` drives
   the reported shape; measured, it yields one daemon child forking all N
   builders.

The pattern across 1, 3 and 4 is one pattern: **the UNIT of the reading was
wrong three times, twice in opposite directions**, and each wrong unit returned
a confident verdict rather than an error. Three of them were visible only
because the script prints the population it sampled; the fourth needed an
outside reader, which is the argument for having one.

## What the negative result licenses, and what it does not

It does not say the daemon is fine. An incident was observed with primary
evidence, twice, and the recovery is understood well enough to be implemented.

It says **concurrency alone is not the trigger**, and therefore that
"nix-daemon wedges at ~20 concurrent build_paths" is not a claim we can put
upstream. Something present in the campaign and absent from the reproducer is
load-bearing. The candidates, none yet tested:

- **Memory pressure.** The strongest one, and the two readings must be kept in
  their right order. On 2026-08-19 `a2c003e` measured seven daemon workers at
  roughly 2 GiB RSS each, squeezing the machine to 2.5 GiB available, and then
  BOUNDED the blanket build-dir injection that caused it - so that figure
  describes the state before its own fix. The wedge on 2026-08-20 came AFTER
  that bound, and its orphans were larger still: one child at 7.5 GiB, about
  20 GiB across seventeen, with the machine in swap. So memory pressure did
  not go away when the injection was bounded, and the later, post-fix reading
  is the one that supports the hypothesis. An earlier draft cited only the
  pre-bound figure, which is both superseded and the weaker of the two.
  (`a2c003e`'s SUBJECT says "14 GiB", the seven-worker TOTAL; its in-code
  comment carries the per-worker reading. Cite the comment.)
  The reproducer's builders hold a shell loop counter. A wedge driven by RSS
  pressure or reclaim would reproduce at *any* N once memory is tight, and at
  *no* N while it is not - which is exactly the pattern of these two datasets.
- **Dynamic derivations and `ca-derivations`.** The campaign builds through
  them throughout; the reproducer uses neither.
- **Output-spec size.** The fork already carries a split for oversized output
  specs against harmonia's 8 KiB display buffer. The reproducer's specs are
  one output each.
- **Real closure I/O.** Substitution, store adds, and lock contention over
  shared paths. Every reproducer derivation is disjoint from every other.

The next experiment is the memory hypothesis, because it is the only candidate
that explains both datasets rather than merely differing between them, and
because it is cheap to drive: hold the concurrency at a rung already measured
healthy and raise the per-builder RSS until the machine is under reclaim.

## How to run it

```
python3 scripts/daemon-stress-bisect.py --selftest    # 11 assertions, touches nothing

# The reported shape. Run its control in the SAME mode as its ladder -
# borrowing the control from the other mode is how this shape went a whole
# round with no negative control of its own.
for N in 2 4 8 12 16 20 24 32; do
  doas s6-svc -r /run/service/nix-daemon
  python3 scripts/daemon-stress-bisect.py -n $N --one-client
done

# The contrast: N connections carrying one request each.
for N in 2 8 12 16 20 24; do
  doas s6-svc -r /run/service/nix-daemon
  python3 scripts/daemon-stress-bisect.py -n $N
done
```

N=2 is the negative control in both modes and must exit 0; if it does not, the
oracle is reading something other than the wedge.

A fresh daemon is needed after any round that WEDGED, because a stuck child
from round k is indistinguishable from a fresh wedge in round k+1. A healthy
round leaves nothing to confound its successor, so the restart can be skipped
between healthy rounds; it is in the loop above because the person running it
does not know in advance which rounds those are.

Exit codes: 0 healthy, 3 wedge observed, 1 the round could not run. The script
refuses to start while a nix-ninja campaign build is live, because it is the
trigger condition and the campaign is what it would trigger against.

**Read the "processes sampled" number on every round.** If it is not N+1 in
one-client mode, or N-ish in multi-client mode, the oracle is not seeing the
builders and the verdict means nothing, whatever it says.
