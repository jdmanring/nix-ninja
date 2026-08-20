# The nix-daemon wedge: what is observed, and what is not reproduced

Status as of 2026-08-20: the incident is real and recorded. The standalone
reproducer does **not** reproduce it, at any concurrency tested. Those are two
separate facts and this document keeps them separate, because the upstream
report is only as strong as the weaker of the two.

## What was observed, during the build

Measured during a qtwebengine 6.11.1 build of at least 16,077 tasks driven by nix-ninja
against nix-daemon 2.35.2:

- daemon child processes stop making progress while holding their locks: zero
  CPU ticks, zero context switches, and no reply ever
  sent;
- dropping the client connection kills the stuck child and frees its locks.
  That is the recovery `nix-builder-rpc-client`'s watchdog implements;
- a mass retry that re-issues every outstanding request at once recreates the
  condition, which is why the watchdog gates retries through a two-permit
  semaphore;
- the stuck children can outlive their supervisor. On 2026-08-20 seventeen of
  them survived as init-reparented root orphans holding roughly 20 GiB of RSS
  between them, one at 7.5 GiB. That is one `ps` reading summed across the
  seventeen, not a sampled peak. `SIGTERM` did not clear them, and neither did
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

The table below comes from the instrument as it now ships. It was produced
twice: once after the fix to defect 6, and again after the fix to defect 7,
and the two runs agree on every cell. That agreement is worth exactly what it
costs and no more - defect 7 was a false-HEALTHY hole, and fixing one cannot
turn a healthy rung into a wedge, only an unseen wedge into a reported one. So
identical numbers say the earlier table was not hiding a wedge; they say
nothing about whether the current oracle can see one, which is the caveat this
document keeps returning to.

Host: 24-thread Ryzen 9 7900X3D, 30.5 GiB RAM, nix-daemon 2.35.2, no other
build running.

Neither ladder log records a memory reading, so the ladder's own rows carry no
memory evidence. That gap was closed separately rather than asserted away: the
top rung of the reported shape was re-run on 2026-08-20 with `/proc/meminfo`
sampled before, during and after.

    [before] MemAvailable 25.12 GiB; swap used 4.64 GiB of 39.09 GiB
    [during] MemAvailable 24.87 GiB; swap used 4.64 GiB of 39.09 GiB
    [after]  MemAvailable 25.06 GiB; swap used 4.64 GiB of 39.09 GiB
    daemon children: 1; processes sampled: 33; fully-dead subtrees over 8s: 0

Two things follow, and the second is the one that matters. The host had roughly
25 GiB available throughout, so these rounds are not evidence about a daemon
under memory pressure. And 32 concurrent builders moved MemAvailable by about a
quarter of a gibibyte, because each holds a shell loop counter - so the ladder
never approached the condition the real build was in when it wedged, where
single workers were measured in gibibytes. The ladder is a concurrency
instrument and it is not a memory instrument, which is the honest reason its
negative result does not reach the leading hypothesis below.

Recorded because an earlier draft of the upstream issue said "swap drained" of
this host. It was not: 4.64 GiB of swap was in use for the whole run and did
not move. Nobody measured it until the claim was challenged.

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

## Six defects in the instrument, and one choice that avoided another

**This section is the canonical count. Every other document links here rather
than restating it**, because the count has now been wrong in three separate
files across three audit rounds, and an instrument whose own history is told
inconsistently invites the one reading a negative result cannot survive: that
it was never trusted.

They did NOT all fail the same way. Three produced a false HEALTHY, one
produced a false WEDGE, one measured the wrong request shape entirely. Item 2
is not a defect at all; it is the choice that kept the fix to item 1 from
creating a false wedge by construction, and it is listed because getting it
wrong was the obvious move.

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

6. **Subtracting one subtree TOTAL from another put the false healthy back, in
   the incident's own shape.** A wedged parent whose builder exits between the
   samples has a shrinking total, so its delta is negative and an `== 0` test
   never fires - and a parent outliving its builder is exactly what the
   2026-08-20 orphans were. PID reuse lands in the same hole. The repair is not
   `<= 0`, which trades it for a false wedge on every completing build; the
   arithmetic was wrong. Deltas are computed PER PROCESS and then summed.
   A subtree that accrued nothing and also LOST a member is now reported
   INDETERMINATE with its own exit code, since it ran for an unknown part of
   the window and forcing it into either verdict is how a false all-clear gets
   published.

7. **The lost-member test was global.** `classify` asked "did anything
   anywhere exit" and applied the answer to every zero-delta subtree, so one
   unrelated builder finishing would mark a genuinely dead subtree
   INDETERMINATE instead of WEDGED - a real finding masked by a bystander.
   That is the same aggregate-answering-for-a-member error as 3 and 4, made
   INSIDE the function written to fix them, with the subject fresh. Membership
   for a process gone at the second sample has to come from the FIRST sample's
   parent map, which is now a required argument rather than an optional one:
   with an empty map the ancestry walk stops at the exited process itself and
   its parent is never marked, restoring the false healthy. The optional
   parameter was the trap, and the fixture that should have caught it was
   passing while testing nothing.
   Found by re-reading the fix rather than by an audit, and it never reached a
   published table: the branch is reachable only when a zero-delta subtree
   exists, and no round has ever produced one.

The pattern across 1, 3, 4, 6 and 7 is one pattern: **the UNIT or the ARITHMETIC
of the reading was wrong five times, in both directions**, and every wrong one
returned a confident verdict rather than an error. Three were visible only
because the script prints the population it sampled. Two needed an outside
reader, which is the argument for having one: an instrument cannot report the
blind spot it has. The last was caught by re-reading a fix immediately after
writing it, which is the cheapest of the three methods and the one most easily
skipped, since a fix feels finished the moment its test goes green.

## What this oracle can still get wrong

Stated because three of its defects were found by someone attacking it and
none by the instrument itself, and because the first thing a maintainer will
do with a future WEDGE verdict is look for a benign explanation. These are the
benign explanations.

- **A finished but unreaped builder is flagged.** Any process that legitimately
  does no work across the sampling window reads identically to a stuck one.
  Harmless while every round comes back healthy; it is the leading alternative
  explanation the day one does not.
- **The child list is captured once, before the first sample.** A daemon child
  forked after the settle window is invisible to both samples, and only round
  completion would catch a fault in it.
- **A subtree that both accrued nothing and lost a member is INDETERMINATE**,
  not healthy and not wedged. It ran for an unknown part of the window. The
  round reports it separately and exits nonzero rather than picking a verdict.
- **One window, one reading.** A process stuck for less than `--interval`
  is indistinguishable from a fast one, and a process that wedges after the
  second sample is only caught by the completion check.
- **There is no positive control.** This script has never produced a wedge, so
  it has never demonstrated that it CAN. The fixtures in `--selftest` are the
  only evidence the verdict logic fires at all, and a fixture is not a daemon.

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

**Read the "processes sampled" number on every round.** It must be N+1 in
one-client mode (one daemon child plus N builders) and 2N in multi-client mode
(N children, one builder each). Anything less and the oracle is not seeing the
builders, so the verdict means nothing whatever it says. An earlier version of
this line said "N-ish" for the multi-client case, which passes on exactly the
reading that would mean the builders are invisible: the guard defeated itself.
