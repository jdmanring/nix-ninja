#!/usr/bin/env python3
"""Probe the nix-daemon for the concurrent-build_paths wedge.

RESULT SO FAR, 2026-08-20: this script does NOT reproduce the wedge, at any
concurrency from 2 to 32, in either shape it can drive. Read
docs/daemon-wedge.md before using it or quoting it. The incident is real and
separately evidenced; concurrency alone is not its trigger, so it is not a
threshold-finder today and the name "bisect" describes its protocol rather
than a result it has ever produced.

Background (observed during a ~15,800 task qtwebengine build driven by
nix-ninja against nix-daemon 2.35.2): daemon child processes go dead-asleep
while holding their locks - 0 CPU ticks, 0 context switches, no kernel flock
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
  3. samples each daemon child's SUBTREE utime+stime from /proc/<pid>/stat
     twice, --interval apart. The subtree is the unit because a daemon
     child forks the sandboxed builder and blocks in wait(), so its own
     ticks stay flat all build and a healthy child would read as wedged;
  4. reports children whose tick delta is zero while still alive - the
     wedge signature - and whether the round completed in --timeout.

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
logic on fixtures, and touches neither the daemon nor the store.
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


def daemon_children(daemon_pid: int) -> list[int]:
    out = subprocess.run(
        ["pgrep", "-P", str(daemon_pid)], capture_output=True, text=True
    ).stdout
    return [int(p) for p in out.split()]


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


def read_ticks(pids: list[int]) -> dict[int, int]:
    """Ticks per daemon child, summed over its whole SUBTREE.

    A daemon child does not run the build: it forks the sandboxed builder and
    blocks in wait(), so its OWN utime+stime stays flat for the entire build.
    Reading the child alone therefore reports a perfectly healthy round as a
    wedge - measured 2026-08-20, N=2 with a CPU-burning builder returned
    "zero-tick: 2" while both builds completed. The load lives in the
    descendants, so the subtree is the unit that distinguishes working from
    dead-asleep.
    """
    ticks = {}
    for pid in pids:
        total, alive = 0, False
        for member in descendants(pid):
            try:
                with open(f"/proc/{member}/stat") as f:
                    total += parse_stat_ticks(f.read())
                alive = True
            except OSError:
                pass  # exited between listing and reading
        if alive:
            ticks[pid] = total
    return ticks


def wedged_pids(t0: dict[int, int], t1: dict[int, int]) -> list[int]:
    """Alive at both samples, zero tick delta: the dead-asleep signature."""
    return [pid for pid, v in t1.items() if pid in t0 and v - t0[pid] == 0]


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
    return (
        subprocess.run(["pgrep", "-x", "nix-ninja"], capture_output=True).returncode
        == 0
    )


def find_daemon_pid() -> int:
    out = subprocess.run(
        ["pgrep", "-o", "-x", "nix-daemon"], capture_output=True, text=True
    ).stdout.strip()
    if not out:
        raise SystemExit("no nix-daemon process found")
    return int(out)


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
    t0 = read_ticks(kids)
    time.sleep(args.interval)
    t1 = read_ticks(list(t0))
    wedged = wedged_pids(t0, t1)
    print(
        f"daemon children observed: {len(t0)}; "
        f"zero-tick over {args.interval}s: {len(wedged)} {wedged or ''}"
    )

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
    print("selftest: 6 assertions passed")
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
