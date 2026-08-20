# The nix-daemon wedge: what is observed, and what is not reproduced

Status as of 2026-08-20: the incident is real and recorded. The standalone
reproducer does **not** reproduce it, at any concurrency tested. Those are two
separate facts and this document keeps them separate, because the upstream
report is only as strong as the weaker of the two.

## What was observed, during the build

Measured during a ~15,800 task qtwebengine 6.11.1 build driven by nix-ninja
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

`scripts/daemon-stress-bisect.py` drives N concurrent trivial builds and reads
each daemon child's CPU ticks to separate a working child from a dead-asleep
one. The bisect ladder was run on 2026-08-20 against a freshly restarted
nix-daemon 2.35.2, on a 24-thread Ryzen 9 7900X3D with 30.5 GiB of RAM, no
other build running, swap drained to zero:

| concurrency | multi-client mode | one-client mode |
|---|---|---|
| 2  | healthy (control) | not run |
| 4  | -       | healthy |
| 8  | healthy | healthy |
| 12 | healthy | healthy |
| 16 | healthy | healthy |
| 20 | healthy | healthy |
| 24 | healthy | healthy |
| 32 | -       | healthy |

Every rung: all builds completed, and no child read zero ticks across the
sampling interval. **No wedge was reproduced.** Read the coverage exactly as
the table gives it: seven distinct levels between 2 and 32, and NEITHER shape
spans the whole range on its own - multi-client ran 2 to 24, one-client ran 4
to 32. The negative control is multi-client N=2; one-client N=2 was never run.
N=20 appears in both because it is the concurrency the incident was attributed
to.

## Three defects in the instrument, and one choice that avoided a fourth

Counted carefully, because the count itself was wrong in three places until an
audit caught it, and an instrument whose own history is told inconsistently
invites the one reading a negative result cannot survive: that it was never
trusted.

Three defects, and they did NOT all fail the same way. One produced a false
HEALTHY, one produced a false WEDGE, one measured the wrong request shape
entirely. Item 2 below is not a defect at all - it is the choice that kept the
fix to item 1 from creating a second false wedge by construction, and it is
listed because getting it wrong was the obvious move.

The first three rounds returned "healthy" while observing **zero** daemon
children, and the zero was the finding rather than the background.

1. **The workload could not hold a daemon child open.** The stress derivation
   wrote a file and exited inside the settle window, so the tick oracle never
   had a subject and only build-completion was doing any work. The builder now
   burns CPU for `--spin` iterations, about twenty seconds by default.

2. **Not a defect: it burns rather than sleeps, deliberately.** The obvious
   repair for item 1 is a sleeping builder, and a sleeping child reads zero
   ticks, which is the wedge signature itself - so that repair would have
   reported every healthy round as a wedge. Recorded because the wrong version
   is the one a reader would reach for.

3. **The tick oracle read the wrong process, and this one produced a false
   WEDGE rather than a false healthy.** With children finally visible, N=2
   reported both as zero-tick while both builds completed. A daemon child
   does not run the build: it forks the sandboxed builder and blocks in
   `wait()`, so its own utime and stime stay flat for the entire build, and a
   healthy child is indistinguishable from a wedged one at that level.
   `read_ticks` now sums each child's whole subtree.

4. **The default mode drives the wrong shape.** Launching N separate `nix
   build` processes opens N daemon *connections* carrying one `build_paths`
   each. The incident describes ~20 concurrent `build_paths` from ONE client,
   and prescribes dropping the client connection as the recovery, which only
   means anything when many requests share a connection. `--one-client` runs a
   single nix process building N derivations with `--max-jobs N`; measured, it
   yields one daemon child that forks all N builders, which is the process
   shape that was watched going dead-asleep. The multi-client mode is kept as
   the negative control: the pair is what would separate a per-connection fault
   from a per-daemon one.

## What the negative result licenses, and what it does not

It does not say the daemon is fine. An incident was observed with primary
evidence, twice, and the recovery is understood well enough to be implemented.

It says **concurrency alone is not the trigger**, and therefore that
"nix-daemon wedges at ~20 concurrent build_paths" is not a claim we can put
upstream. Something present in the campaign and absent from the reproducer is
load-bearing. The candidates, none yet tested:

- **Memory pressure.** The strongest one. `a2c003e` measured seven daemon
  workers at roughly 2 GiB RSS each, squeezing the machine to 2.5 GiB
  available, before it bounded the blanket build-dir injection, and the wedge
  coincided with the machine in swap. (That commit's SUBJECT says "14 GiB",
  which is the seven-worker TOTAL; its in-code comment carries the per-worker
  reading, and the drafts staged from the subject alone said "apiece" until an
  audit caught it. Cite the comment, not the subject.) The
  reproducer's builders hold a shell loop counter. A wedge driven by RSS
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
python3 scripts/daemon-stress-bisect.py --selftest      # 6 assertions, touches nothing
for N in 2 8 12 16 20 24; do
  # a fresh daemon per round: a wedged child from round k is
  # indistinguishable from a fresh wedge in round k+1
  doas s6-svc -r /run/service/nix-daemon
  python3 scripts/daemon-stress-bisect.py -n $N --one-client
done
```

A fresh daemon is only strictly required after a round that wedged; a healthy
round leaves no stuck child to confound its successor. Exit codes: 0 healthy,
3 wedge observed, 1 the round could not run. The script refuses to run while a
nix-ninja campaign build is live, because it is the trigger condition and would
take the campaign down with it.
