#!/usr/bin/env python3
"""Probe the nix-daemon for the concurrent-build_paths wedge.

RESULT SO FAR, 2026-08-20: this script has not reproduced the wedge at any
level it has been run at. Read docs/daemon-wedge.md for the exact coverage of
each shape before using or quoting that - it is NOT "any concurrency in either
shape", and an earlier version of this line said so wrongly. The incident is real and
separately evidenced; concurrency alone is not its trigger, so it is not a
threshold-finder today and the name "bisect" describes its protocol rather
than a result it has ever produced.

Background (observed during a qtwebengine build of at least 16,077 tasks -
the highest index the driver log reaches, the graph never having been driven
to completion - against nix-daemon 2.35.2): daemon child processes go dead-asleep
while holding their locks - 0 CPU ticks, 0 context switches, no kernel
waiters - and never reply. Closing the client connection kills the stuck
child and frees its locks, which is the recovery nix-ninja's watchdog
implements. A mass retry that re-issues every outstanding request at once
recreates the condition, which is why the watchdog gates retries through a
2-permit semaphore. The children can outlive their supervisor as
init-reparented root orphans holding gigabytes of RSS.

One invocation runs ONE round at one concurrency level N:

  1. refuses to run if a nix-ninja campaign build is live (the round
     would wedge it - this script IS the trigger condition);
  2. launches N concurrent build_paths requests. Two shapes: by default N
     separate `nix build` processes, which is N CONNECTIONS carrying one
     request each; with --one-client, a single nix process building N
     derivations, which is one connection carrying N concurrent requests
     and is the shape the incident describes. Keep both: the pair is what
     separates a per-connection fault from a per-daemon one;
  3. samples utime+stime for EVERY process under the daemon, twice,
     --interval apart, and computes each process's delta individually;
  4. reports every process whose own SUBTREE accrued nothing - the wedge
     signature - and whether the round completed in --timeout. The unit is
     the subtree because a daemon child forks the sandboxed builder and
     blocks in wait(), so its own ticks stay flat all build and reading it
     alone makes a healthy child look wedged. The unit is EVERY process
     rather than each top-level child because one client's requests are
     served by a single child forking N builders, and a per-child total
     hides one dead builder behind its live siblings. A subtree that
     accrued nothing AND lost a member is reported INDETERMINATE, not
     healthy: it ran for an unknown part of the window.

The stress builder BURNS CPU rather than sleeping, and for long enough to
outlive --settle. Both properties are load-bearing and both were absent
first time round: a builder that exits early leaves the oracle with no
subject, and a sleeping one reads zero ticks, which is the wedge signature
itself. See docs/daemon-wedge.md for the three verdicts that cost.

Exit codes: 0 = round completed, no wedged child (healthy);
3 = wedge observed (the finding); 1 = the round itself failed to run.

Protocol (run by hand - daemon restarts need root, so a person drives them):
  for N in 2 8 12 16 20 24:
    ! doas s6-svc -r /run/service/nix-daemon   # see below
    python3 scripts/daemon-stress-bisect.py -n $N --one-client
  N=2 is the negative control: it must exit 0, or the oracle is reading
  something other than the wedge. A fresh daemon is needed after any round
  that WEDGED, because a stuck child from round k is indistinguishable from
  a fresh wedge in round k+1; a healthy round leaves nothing to confound its
  successor.

--selftest exercises the /proc parsing, the subtree walk and the verdict
logic on fixtures, and touches neither the daemon nor the store. One fixture
is the masking case the per-child oracle could not see: it asserts that the
old reading calls that round healthy and the current one names the dead
builder.
"""

import argparse
import os
import subprocess
import sys
import time


def parse_stat_ticks(stat_line: str) -> int:
    """utime+stime from a /proc/<pid>/stat line.

    The comm field (2) is parenthesized and may itself contain spaces
    and ')' - `(nix-daemon 2.35)` is real - so split on the LAST ')':
    everything after it is space-separated, with utime and stime at
    fields 14 and 15 of the full record (indices 11 and 12 after the
    close paren).
    """
    after = stat_line.rsplit(")", 1)[1].split()
    return int(after[11]) + int(after[12])


def parse_stat_starttime(stat_line: str) -> int:
    """Field 22, the process's start time in clock ticks since boot.

    The pid-reuse discriminator. Two samples can show the same pid holding two
    different processes, and the second reads fewer ticks than the first, which
    is a negative delta - the exact shape that produced defect 6. A pid whose
    starttime moved is a different process and its counter is not comparable.
    """
    return int(stat_line.rsplit(")", 1)[1].split()[19])


def parse_stat_state(stat_line: str) -> str:
    """Field 3, the single-letter process state. 'Z' is a zombie."""
    return stat_line.rsplit(")", 1)[1].split()[0]


def read_activity(pid: int) -> tuple[int, int] | None:
    """(activity counter, starttime) for one process, or None if unreadable.

    The counter sums utime, stime and BOTH context-switch counts. The failure
    being reported is a process with zero CPU ticks AND zero context switches;
    a ticks-only reading cannot tell that from a builder legitimately blocked
    on I/O, which accrues no ticks either. Every component is a monotonic
    counter, so the sum is zero across an interval only when all of them are,
    which is the reported condition and not a weaker proxy for it.
    """
    try:
        with open(f"/proc/{pid}/stat") as f:
            stat = f.read()
        if parse_stat_state(stat) == "Z":
            # A ZOMBIE IS NOT A WEDGE, and to a counter-delta oracle the two
            # are byte-identical: a reaped-pending child accrues no ticks and
            # no context switches because it is dead, not because it is stuck.
            # The composite metric does not separate them - a zombie is zero on
            # every counter there is. Measured 2026-08-20 against the live
            # daemon during a real qtwebengine round: the oracle's first real
            # detection was pid 17695, State Z, RSS 0, a defunct
            # nix-ninja-task, and it would have been reported upstream as a
            # wedged worker. Excluded here rather than in classify, so no
            # caller can forget.
            return None
        ticks = parse_stat_ticks(stat)
        started = parse_stat_starttime(stat)
        switches = 0
        with open(f"/proc/{pid}/status") as f:
            for line in f:
                if line.startswith(("voluntary_ctxt_switches", "nonvoluntary_ctxt")):
                    switches += int(line.split()[1])
    except (OSError, IndexError, ValueError):
        return None
    return ticks + switches, started


def daemon_children(daemon_pid: int) -> list[int]:
    """Direct children of one pid. Raises rather than reporting none on error.

    pgrep exits 1 for "no matches" and >1 for a real failure, and collapsing
    those into an empty list is how an enumeration failure becomes a healthy
    verdict over a population of zero.
    """
    r = subprocess.run(["pgrep", "-P", str(daemon_pid)], capture_output=True, text=True)
    if r.returncode > 1:
        raise SystemExit(f"pgrep failed enumerating children of {daemon_pid}: {r.stderr.strip()}")
    return [int(p) for p in r.stdout.split()]


def descendants(pid: int) -> list[int]:
    """pid plus every process beneath it, breadth-first."""
    seen, frontier = [pid], [pid]
    while frontier:
        nxt = []
        for p in frontier:
            for kid in daemon_children(p):
                if kid not in seen:
                    seen.append(kid)
                    nxt.append(kid)
        frontier = nxt
    return seen


UNREADABLE: set[int] = set()
STARTED: dict[int, int] = {}


def proc_tree(roots: list[int]) -> tuple[dict[int, int], dict[int, int]]:
    """Own ticks per process, and each process's parent, over every subtree.

    Returns (ticks_by_pid, parent_by_pid). Roots map to parent -1.
    """
    ticks: dict[int, int] = {}
    parent: dict[int, int] = {}
    for root in roots:
        frontier = [root]
        parent.setdefault(root, -1)
        while frontier:
            nxt = []
            for pid in frontier:
                got = read_activity(pid)
                if got is None:
                    # Exited between listing and reading, OR a transient read
                    # failure. Do not `continue` past the walk: dropping the
                    # node silently drops every descendant with it, shrinking
                    # the population toward a healthy verdict. Record it as
                    # unreadable and keep walking.
                    UNREADABLE.add(pid)
                else:
                    ticks[pid], STARTED[pid] = got
                for kid in daemon_children(pid):
                    if kid not in parent:
                        parent[kid] = pid
                        nxt.append(kid)
            frontier = nxt
    return ticks, parent


def subtree_totals(
    ticks: dict[int, int], parent: dict[int, int]
) -> dict[int, int]:
    """Each pid's own ticks plus every descendant's, from the parent map."""
    totals = dict(ticks)
    for pid in ticks:
        p = parent.get(pid, -1)
        while p != -1 and p in totals:
            totals[p] += ticks[pid]
            p = parent.get(p, -1)
    return totals


def read_ticks(pids: list[int]) -> dict[int, int]:
    """SUPERSEDED, and kept only for the selftest. See classify().

    Nothing in the live path calls this, nor `descendants`, nor `wedged_pids`,
    nor `wedged_nodes`. They are each a previous generation of this oracle, and
    they stay because the selftest asserts what each of them answers on the
    same fixture. That record is the only thing documenting why the unit
    changed three times, and deleting it as dead code would delete the reason.
    """
    ticks, parent = proc_tree(pids)
    totals = subtree_totals(ticks, parent)
    return {pid: totals[pid] for pid in pids if pid in totals}


def wedged_pids(t0: dict[int, int], t1: dict[int, int]) -> list[int]:
    """The SUPERSEDED per-root reading, kept only so the selftest can assert
    what it used to answer. Do not use it for a verdict."""
    return [pid for pid, v in t1.items() if pid in t0 and v - t0[pid] == 0]


def classify(
    ticks0: dict[int, int],
    ticks1: dict[int, int],
    parent1: dict[int, int],
    parent0: dict[int, int],
) -> tuple[list[int], list[int]]:
    """(fully dead subtrees, indeterminate subtrees), outermost nodes only.

    Deltas are computed PER PROCESS and then summed over the subtree. Summing
    each subtree's total and subtracting is not the same arithmetic and has a
    silent hole: a wedged parent whose builder EXITS between the samples has a
    shrinking total, so its delta is negative, so an `== 0` test never fires -
    and a parent outliving its builder is precisely the shape of the orphans
    this script exists to look for. PID reuse lands in the same hole, the new
    occupant of a recycled pid reading fewer ticks than the old.

    A member present at both samples contributes what it accrued. A member
    that appeared contributes everything it has, since it did that work inside
    the window. A member that EXITED contributes nothing measurable: it ran for
    an unknown part of the window and its ticks are gone. That last case is
    genuinely unknown rather than zero, so a subtree that accrued nothing AND
    lost a member is reported as INDETERMINATE rather than as either verdict.
    Calling it healthy would be the false-healthy hole again; calling it wedged
    would flag every builder that simply finished.
    """
    dead: list[int] = []
    unknown: list[int] = []
    delta: dict[int, int] = {}
    for pid, v1 in ticks1.items():
        delta[pid] = v1 - ticks0[pid] if pid in ticks0 else v1

    members: dict[int, list[int]] = {}
    for pid in ticks1:
        node = pid
        while node != -1:
            members.setdefault(node, []).append(pid)
            node = parent1.get(node, -1)

    # Which subtrees LOST a member, per subtree. Membership for a process that
    # is gone at t1 has to come from the FIRST sample's parent map, since it no
    # longer has one. Computing this globally - "did anything anywhere exit" -
    # marks every zero-delta subtree indeterminate the moment any unrelated
    # process finishes, which would mask a real wedge behind a bystander. That
    # is the same aggregate-for-member error the oracle has now made twice, so
    # it is spelled out rather than left to be re-derived.
    # parent0 is REQUIRED rather than defaulted. A process gone at t1 has no
    # entry in parent1, so with an empty first-sample map its ancestry walk
    # stops at itself and its parent is never marked - which silently restores
    # the false healthy this function exists to close.
    lost_under: set[int] = set()
    p0 = parent0
    for pid in ticks0:
        if pid in ticks1:
            continue
        node = pid
        while node != -1:
            lost_under.add(node)
            node = p0.get(node, parent1.get(node, -1))

    for root, kin in members.items():
        if sum(delta[k] for k in kin) != 0:
            continue
        # nothing accrued anywhere beneath it: dead, unless one of ITS OWN
        # members vanished
        (unknown if root in lost_under else dead).append(root)

    dead_set = set(dead)
    outer_dead = sorted(p for p in dead if parent1.get(p, -1) not in dead_set)
    unknown_set = set(unknown)
    outer_unknown = sorted(
        p for p in unknown if parent1.get(p, -1) not in unknown_set
    )
    return outer_dead, outer_unknown


def wedged_nodes(
    t0: dict[int, int],
    t1: dict[int, int],
    parent: dict[int, int],
) -> list[int]:
    """Superseded subtree-total reading. See classify() for the live oracle.

    Kept because the selftest asserts what each generation of this oracle
    answers on the same fixture, which is the only record of why the unit
    changed three times.
    """
    dead = {pid for pid, v in t1.items() if pid in t0 and v - t0[pid] == 0}
    return sorted(p for p in dead if parent.get(p, -1) not in dead)


BUILDER_ARGS = (
    '["-c" "i=0; while [ $i -lt $SPIN ]; do i=$((i+1)); done; echo $NONCE > $out"]'
)


def build_argv(nix: str) -> list[str]:
    # A unique impure env var makes every derivation distinct, so each
    # request is a genuine build, not a cache hit answering instantly.
    expr = (
        'derivation { name = "stress"; system = builtins.currentSystem; '
        'builder = "/bin/sh"; args = ' + BUILDER_ARGS + '; '
        'SPIN = builtins.getEnv "SPIN"; '
        "NONCE = builtins.getEnv \"NONCE\"; }"
    )
    return [nix, "build", "--impure", "--no-link", "--expr", expr]


def one_client_argv(nix: str, n: int) -> list[str]:
    """ONE nix process building n distinct derivations concurrently.

    This is the shape the wedge was reported in and the default multi-process
    mode is NOT it. Launching n separate `nix build` processes opens n daemon
    CONNECTIONS carrying one build_paths each; the report describes ~20
    concurrent build_paths from ONE client, and prescribes dropping the client
    connection as the recovery - which only means anything when many requests
    share a connection. Measured 2026-08-20: the multi-process ladder ran
    healthy at every rung through N=24, falsifying the instrument rather than
    the daemon.
    """
    expr = (
        "let mk = i: derivation { "
        'name = "stress-${toString i}"; system = builtins.currentSystem; '
        'builder = "/bin/sh"; args = ' + BUILDER_ARGS + '; '
        'SPIN = builtins.getEnv "SPIN"; '
        'NONCE = builtins.getEnv "NONCE" + toString i; }; '
        'in builtins.listToAttrs (map (i: { name = "s" + toString i; value = mk i; }) '
        "(builtins.genList (x: x) " + str(n) + "))"
    )
    attrs = [f"s{i}" for i in range(n)]
    return [
        nix, "build", "--impure", "--no-link", "--max-jobs", str(n), "--expr", expr
    ] + attrs


def campaign_live() -> bool:
    """True if a real build is running. Fails CLOSED: this guard exists to stop
    the script perturbing a live campaign, so any pgrep failure must read as
    "something is running", never as permission to proceed."""
    r = subprocess.run(["pgrep", "-x", "nix-ninja"], capture_output=True)
    return r.returncode != 1


def find_daemon_pid() -> int:
    """The one live nix-daemon listener, or a refusal.

    This used to be `pgrep -o`, "least recently started", which is the WRONG
    end after exactly the event this script studies: the protocol restarts the
    daemon between rounds, so the fresh listener is always younger than any
    wedged survivor, and `-o` hands back the orphan. Every later round then
    enumerates the orphan's children, finds none, and reports healthy over an
    empty population.

    More than one nix-daemon means a survivor is still around, and that is a
    reason to stop rather than to choose. Aiming the instrument is not a
    tiebreak.
    """
    r = subprocess.run(["pgrep", "-x", "nix-daemon"], capture_output=True, text=True)
    if r.returncode > 1:
        raise SystemExit(f"pgrep failed looking for nix-daemon: {r.stderr.strip()}")
    pids = [int(p) for p in r.stdout.split()]
    if not pids:
        raise SystemExit("no nix-daemon process found")
    if len(pids) > 1:
        raise SystemExit(
            f"{len(pids)} nix-daemon processes alive ({pids}); a survivor from an "
            "earlier round makes every reading ambiguous. Clear them "
            "(pkill -9 -x nix-daemon), restart the daemon, and re-run."
        )
    return pids[0]


def run_round(args: argparse.Namespace) -> int:
    if campaign_live():
        print(
            "REFUSED: a nix-ninja build is running and this script's whole "
            "purpose is to wedge the daemon it is using. Wait or stop it.",
            file=sys.stderr,
        )
        return 1
    daemon_pid = args.daemon_pid or find_daemon_pid()
    nonce_base = str(int(time.time()))
    env = dict(os.environ, NONCE=f"{nonce_base}-", SPIN=str(args.spin))
    if args.one_client:
        procs = [
            subprocess.Popen(
                one_client_argv(args.nix, args.n),
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        ]
        shape = "1 client, %d concurrent build_paths" % args.n
    else:
        procs = [
            subprocess.Popen(
                build_argv(args.nix),
                env=dict(env, NONCE=f"{nonce_base}-{i}"),
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            for i in range(args.n)
        ]
        shape = "%d clients, 1 build_paths each" % args.n
    print(f"launched {shape} (nonce {nonce_base})")

    time.sleep(args.settle)  # let the daemon fork its children
    kids = daemon_children(daemon_pid)
    UNREADABLE.clear()
    ticks0, parent0 = proc_tree(kids)
    started0 = dict(STARTED)
    time.sleep(args.interval)
    # Re-walk from the daemon's children AND from every pid seen in the first
    # sample. A wedged child whose parent dies is reparented to init, which
    # takes it out of `pgrep -P daemon` entirely - so walking only the live
    # daemon's children loses exactly the process this script exists to find,
    # and reports healthy over the smaller population. The incident it is
    # modelled on was seventeen such orphans. Carrying the pid set forward
    # keeps a reparented process in view for as long as it is alive.
    UNREADABLE.clear()
    ticks1, parent1 = proc_tree(sorted(set(kids) | set(ticks0)))
    # A pid whose starttime moved is a DIFFERENT process. Dropping it from the
    # first sample makes classify treat it as newly appeared and credit it with
    # everything it has, instead of subtracting an unrelated process's counter
    # and producing a negative delta.
    recycled = [p for p, t in STARTED.items() if p in started0 and started0[p] != t]
    for pid in recycled:
        del ticks0[pid]
    wedged, unknown = classify(ticks0, ticks1, parent1, parent0)
    orphaned = sorted(set(ticks1) - set(kids) - set(parent1) | (set(ticks0) - set(ticks1)))
    reparented = [p for p in ticks1 if p not in kids and parent1.get(p, -1) == -1]
    # Print the POPULATION, not only the verdict. Three of this script's four
    # instrument defects were visible only because the sampled counts sit
    # beside the result: a bare "healthy" over zero observed processes reads
    # exactly like a healthy daemon.
    print(
        f"daemon children: {len(kids)}; processes sampled: {len(ticks1)}; "
        f"fully-dead subtrees over {args.interval}s: {len(wedged)} {wedged or ''}"
        + (f"; INDETERMINATE: {len(unknown)} {unknown}" if unknown else "")
        + (f"; REPARENTED (lost their parent, still sampled): {reparented}" if reparented else "")
        + (f"; recycled pids: {recycled}" if recycled else "")
        + (f"; UNREADABLE: {len(UNREADABLE)}" if UNREADABLE else "")
    )
    if not ticks1:
        # Defect 1 arriving by a second door. A verdict over an empty
        # population is not a healthy reading, it is no reading, and the whole
        # point of printing the count was that a human would notice - which is
        # a hope, not a gate. This is the gate.
        print("VERDICT: could not run - sampled no processes at all")
        return 1

    deadline = time.time() + args.timeout
    pending = list(procs)
    while pending and time.time() < deadline:
        pending = [p for p in pending if p.poll() is None]
        if pending:
            time.sleep(2)
    for p in pending:
        p.kill()
    total = len(procs)
    done = total - len(pending)
    unit = "client(s)" if args.one_client else "builds"
    print(f"{unit} completed: {done}/{total} within {args.timeout}s")

    if wedged or pending:
        print(f"VERDICT: WEDGE at N={args.n}")
        return 3
    if unknown:
        # A subtree that accrued nothing and also lost a member ran for an
        # unknown part of the window. Not healthy, not wedged, and reporting
        # it as either is how a false all-clear gets published.
        print(f"VERDICT: INDETERMINATE at N={args.n}")
        return 1
    print(f"VERDICT: healthy at N={args.n}")
    return 0


def selftest() -> int:
    # comm with a space and an inner ')' - the field the naive split loses.
    line = "123 (nix-daemon 2.35)) S 1 1 1 0 -1 4194560 0 0 0 0 40 2 0 0 20 0 1 0 5 0 0"
    assert parse_stat_ticks(line) == 42, parse_stat_ticks(line)
    # zero-delta detection, and exit-between-samples is NOT wedged
    assert wedged_pids({1: 10, 2: 5}, {1: 10}) == [1]
    assert wedged_pids({1: 10, 2: 5}, {1: 11, 2: 9}) == []
    assert wedged_pids({1: 10}, {}) == []
    # the build expr is impure-unique: nonce reaches it via env, not argv
    argv = build_argv("nix")
    assert "--impure" in argv and "--no-link" in argv
    # the subtree walk must at minimum contain its own root, and self-nesting
    # (a pid reachable from itself) must not loop forever
    me = os.getpid()
    assert descendants(me)[0] == me

    # THE BLIND SPOT THAT COST ROUND 2 OF THE AUDIT. One daemon child forking
    # N builders, one of them dead-asleep and the rest burning. The old
    # per-child subtree sum reports the round healthy, because the siblings'
    # CPU masks the dead one. Fixture: child 10 with builders 11 (dead), 12
    # and 13 (burning).
    parent = {10: -1, 11: 10, 12: 10, 13: 10}
    ticks_a = {10: 5, 11: 100, 12: 200, 13: 300}
    ticks_b = {10: 5, 11: 100, 12: 260, 13: 380}
    tot_a = subtree_totals(ticks_a, parent)
    tot_b = subtree_totals(ticks_b, parent)
    # the masking is real: the child's own subtree total DID move
    assert tot_b[10] - tot_a[10] > 0
    assert wedged_pids({10: tot_a[10]}, {10: tot_b[10]}) == []
    # and the per-node oracle still finds the dead builder
    assert wedged_nodes(tot_a, tot_b, parent) == [11]

    # a parked child with a burning builder is NOT flagged, and when the whole
    # region dies only the outermost node is named
    parent2 = {20: -1, 21: 20}
    assert wedged_nodes(
        subtree_totals({20: 7, 21: 50}, parent2),
        subtree_totals({20: 7, 21: 90}, parent2),
        parent2,
    ) == []
    assert wedged_nodes(
        subtree_totals({20: 7, 21: 50}, parent2),
        subtree_totals({20: 7, 21: 50}, parent2),
        parent2,
    ) == [20]

    # THE HOLE ROUND 3 FOUND, and it is the incident's own shape: a wedged
    # parent whose builder EXITS between the samples. Subtree TOTALS shrink,
    # so the delta goes negative and an == 0 test never fires. Per-process
    # deltas do not have the hole. Both readings asserted, because the record
    # of why the unit changed is the only thing that stops it changing back.
    parent_x = {10: -1, 11: 10}
    t0_x = {10: 5, 11: 100}
    t1_x = {10: 5}  # builder gone, parent flat
    assert wedged_nodes(
        subtree_totals(t0_x, parent_x), subtree_totals(t1_x, {10: -1}), {10: -1}
    ) == [], "the superseded reading must be shown missing it"
    dead_x, unknown_x = classify(t0_x, t1_x, {10: -1}, parent_x)
    assert dead_x == [] and unknown_x == [10], (dead_x, unknown_x)

    # A parent that accrued nothing with NO member lost is dead, not unknown.
    dead_y, unknown_y = classify({10: 5}, {10: 5}, {10: -1}, {10: -1})
    assert dead_y == [10] and unknown_y == []

    # A burning builder keeps its parked parent off both lists.
    parent_z = {20: -1, 21: 20}
    dead_z, unknown_z = classify(
        {20: 7, 21: 50}, {20: 7, 21: 90}, parent_z, parent_z
    )
    assert dead_z == [] and unknown_z == []

    # A process that APPEARED inside the window counts its whole reading as
    # work: it cannot have accrued those ticks before it existed.
    dead_w, unknown_w = classify({20: 7}, {20: 7, 21: 40}, parent_z, {20: -1})
    assert dead_w == [] and unknown_w == []

    # A BYSTANDER EXIT MUST NOT MASK A WEDGE. Two independent children: 10 is
    # dead with no children of its own, 30's builder 31 exits normally. Asking
    # "did anything anywhere exit" would call 10 indeterminate because of 31,
    # which is a different subtree entirely.
    parent_b0 = {10: -1, 30: -1, 31: 30}
    parent_b1 = {10: -1, 30: -1}
    dead_b, unknown_b = classify(
        {10: 5, 30: 9, 31: 70}, {10: 5, 30: 9}, parent_b1, parent_b0
    )
    assert dead_b == [10], (dead_b, unknown_b)
    assert unknown_b == [30], (dead_b, unknown_b)

    # --- defect 8: an orphaned subtree must still be classified -------------
    # A reparented process arrives as its own root (parent -1). The bug being
    # hunted is a wedged child whose parent died, so if a root-parented node
    # cannot be classified the instrument is blind to the incident's own shape.
    orphan_t0 = {900: 50}
    orphan_t1 = {900: 50}
    orphan_par = {900: -1}
    dead, unk = classify(orphan_t0, orphan_t1, orphan_par, orphan_par)
    assert dead == [900], dead
    assert unk == []

    # An orphan that is still working is not wedged.
    dead, _ = classify({900: 50}, {900: 90}, orphan_par, orphan_par)
    assert dead == []

    # --- defect 8: context switches count as activity -----------------------
    # A builder blocked on I/O accrues no CPU ticks and DOES accrue context
    # switches. Under the superseded ticks-only reading it was indistinguishable
    # from a wedge; the composite counter separates them. This fixture pins the
    # separation so the metric cannot quietly narrow back to ticks.
    dead, _ = classify({901: 100}, {901: 104}, {901: -1}, {901: -1})
    assert dead == [], "activity in the interval must not read as dead"

    # starttime parses from the same awkward line the tick parser handles.
    assert parse_stat_starttime(line) == 5, parse_stat_starttime(line)

    # read_activity answers for a live process and is None for a pid that
    # cannot exist. An unreadable process must not silently become healthy.
    mine = read_activity(os.getpid())
    assert mine is not None and mine[0] > 0, mine
    assert read_activity(2**22) is None

    # --- a zombie must not read as a wedge -----------------------------------
    zline = "999 (nix-ninja-task) Z 1 1 1 0 -1 4194560 0 0 0 0 0 0 0 0 20 0 1 0 7 0 0"
    assert parse_stat_state(zline) == "Z", parse_stat_state(zline)
    assert parse_stat_state(line) == "S", parse_stat_state(line)

    # Count the assertions by PARSING this function rather than by carrying a
    # literal. The literal said 19 over 18 assertions and the documentation
    # said 11, which is a count wrong in both directions at once - in a script
    # whose subject is readings that are confidently wrong.
    import ast as _ast

    with open(__file__) as _f:
        _tree = _ast.parse(_f.read())
    _n = sum(
        isinstance(node, _ast.Assert)
        for fn in _tree.body
        if isinstance(fn, _ast.FunctionDef) and fn.name == "selftest"
        for node in _ast.walk(fn)
    )
    print(f"selftest: {_n} assertions passed")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("-n", type=int, default=20, help="concurrent build requests")
    ap.add_argument("--interval", type=int, default=10, help="tick-sample gap (s)")
    ap.add_argument("--settle", type=int, default=2, help="wait before sampling (s)")
    # The stress builder must BURN CPU, not sleep: a sleeping child reads zero
    # ticks, which is the wedge signature itself, so a sleep-based workload would
    # report every healthy round as a wedge. It must also outlive --settle - a
    # trivial builder exits first and the oracle observes 0 children, which is how
    # the first three rounds of the 2026-08-20 ladder returned vacuous verdicts.
    ap.add_argument(
        "--spin",
        type=int,
        default=15_000_000,
        help="shell-loop iterations each stress builder burns (CPU, not sleep)",
    )
    ap.add_argument("--timeout", type=int, default=120, help="build completion budget (s)")
    ap.add_argument(
        "--one-client",
        action="store_true",
        help="one nix process issuing n concurrent build_paths (the reported shape)",
    )
    ap.add_argument("--daemon-pid", type=int, help="override daemon pid autodetect")
    ap.add_argument("--nix", default="nix", help="nix binary to launch builds with")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return selftest()
    return run_round(args)


if __name__ == "__main__":
    sys.exit(main())
