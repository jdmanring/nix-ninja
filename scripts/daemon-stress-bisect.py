#!/usr/bin/env python3
"""Reproduce the nix-daemon concurrent-build_paths wedge, and bisect its threshold.

Background (measured against nix-daemon 2.35.2 during a 15,800-task
qtwebengine build driven by nix-ninja): at ~20 concurrent build_paths
requests from one client, daemon child processes go dead-asleep while
holding their locks - 0 CPU ticks, 0 context switches, no kernel flock
waiters - and never reply. Closing the client connection kills the stuck
child and frees its locks, which is the recovery nix-ninja's watchdog
implements. The trigger is load: a mass retry that re-issues all ~20
requests at once deterministically recreates the wedge, which is why the
watchdog gates retries through a 2-permit semaphore.

This script is the standalone reproducer for an upstream report. One
invocation runs ONE round at one concurrency level N:

  1. refuses to run if a nix-ninja campaign build is live (the round
     would wedge it - this script IS the trigger condition);
  2. launches N concurrent `nix build` requests, each a unique trivial
     derivation (a nonce env var defeats caching, so every request is a
     real build_paths that must build);
  3. samples every nix-daemon child's utime+stime from /proc/<pid>/stat
     twice, --interval seconds apart, once the builds have had time to
     start;
  4. reports children whose tick delta is zero while still alive - the
     wedge signature - and whether the N builds completed in --timeout.

Exit codes: 0 = all builds completed, no wedged child (healthy);
3 = wedge observed (the finding); 1 = the round itself failed to run.

Bisect protocol (run by hand - daemon restarts need root, so a person
drives them):
  for N in 2 8 12 16 20 24:
    ! doas s6-svc -r /run/service/nix-daemon   # fresh daemon per round
    python3 scripts/daemon-stress-bisect.py -n $N
  N=2 is the negative control: it must exit 0, or the oracle is reading
  something other than the wedge. A fresh daemon per round matters
  because a wedged child from round k is indistinguishable from a fresh
  wedge in round k+1.

--selftest exercises the /proc parsing and verdict logic on fixtures and
touches neither the daemon nor the store.
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


def read_ticks(pids: list[int]) -> dict[int, int]:
    ticks = {}
    for pid in pids:
        try:
            with open(f"/proc/{pid}/stat") as f:
                ticks[pid] = parse_stat_ticks(f.read())
        except OSError:
            pass  # exited between listing and reading: not wedged
    return ticks


def wedged_pids(t0: dict[int, int], t1: dict[int, int]) -> list[int]:
    """Alive at both samples, zero tick delta: the dead-asleep signature."""
    return [pid for pid, v in t1.items() if pid in t0 and v - t0[pid] == 0]


def build_argv(nix: str) -> list[str]:
    # A unique impure env var makes every derivation distinct, so each
    # request is a genuine build, not a cache hit answering instantly.
    expr = (
        'derivation { name = "stress"; system = builtins.currentSystem; '
        'builder = "/bin/sh"; args = ["-c" "echo $NONCE > $out"]; '
        "NONCE = builtins.getEnv \"NONCE\"; }"
    )
    return [nix, "build", "--impure", "--no-link", "--expr", expr]


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
    procs = []
    for i in range(args.n):
        env = dict(os.environ, NONCE=f"{nonce_base}-{i}")
        procs.append(
            subprocess.Popen(
                build_argv(args.nix),
                env=env,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        )
    print(f"launched {args.n} concurrent build requests (nonce {nonce_base})")

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
    done = args.n - len(pending)
    print(f"builds completed: {done}/{args.n} within {args.timeout}s")

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
    print("selftest: 5 assertions passed")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    ap.add_argument("-n", type=int, default=20, help="concurrent build requests")
    ap.add_argument("--interval", type=int, default=10, help="tick-sample gap (s)")
    ap.add_argument("--settle", type=int, default=5, help="wait before sampling (s)")
    ap.add_argument("--timeout", type=int, default=120, help="build completion budget (s)")
    ap.add_argument("--daemon-pid", type=int, help="override daemon pid autodetect")
    ap.add_argument("--nix", default="nix", help="nix binary to launch builds with")
    ap.add_argument("--selftest", action="store_true")
    args = ap.parse_args()
    if args.selftest:
        return selftest()
    return run_round(args)


if __name__ == "__main__":
    sys.exit(main())
