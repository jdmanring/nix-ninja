#!/usr/bin/env bash
# IS THE DAEMON WORKING OR STUCK DURING A WATCHDOG GAP? Unprivileged.
#
# The client cannot tell a wedge from a very slow reply, and the natural
# instrument - a stack sample of the worker - needs privileges. This asks a
# weaker question that separates the same two states, from /proc, as any
# user:
#
#   cumulative CPU RISING            the worker is doing work
#   CPU flat, vctx RISING            blocking and waking: waiting on
#                                    something that keeps answering
#   CPU flat, vctx FLAT              stuck on one wait, which is the wedge
#
# CUMULATIVE CPU, NOT %CPU. A worker asleep reads 0.0% and so does a worker
# that has been stuck for twenty minutes; `ps -o time=` is monotonic and
# tells them apart. wchan is printed where the kernel exposes it and is
# often `0`, which is not an error.
#
# Run it DURING a gap - the daemon log's reap lines bound one - and give it
# the gap's length if you can. Sampling costs two file reads per worker.
set -u
INTERVAL=${1:-10}
COUNT=${2:-6}

vctx() { awk '$1=="voluntary_ctxt_switches:"{print $2}' "/proc/$1/status" 2>/dev/null; }
# LIFETIME SATURATION, because a SHORT DELTA CAN CATCH A LULL INSIDE SUSTAINED
# SATURATION and this probe reported exactly that. Measured 2026-09-07: a
# worker read 6 s of CPU over 50 s of wall clock, 0.12x, which was taken as a
# gap; its lifetime was 3h15m of CPU over 2h14m elapsed, 1.45x across 13
# threads, saturated throughout. The run launched on that reading aborted
# DaemonStalled, the third such abort for the same instrument.
# The delta and the lifetime answer DIFFERENT questions. The delta says what
# the worker is doing in this window. The lifetime says whether this window is
# representative. When they disagree, the lifetime wins for a go/no-go, because
# the cost of being wrong is an abort that measures nothing.
# Divide by thread count before comparing to 1.0: a 13-thread worker at 1.45x
# is not 145% of a core, it is 11% of its own capacity and still the busiest
# thing serving you.
life() { ps -o etimes=,times=,nlwp= -p "$1" 2>/dev/null | awk '
  $1 > 0 { printf "%.2fx over %ds, %d thr", $2/$1, $1, $3 }' ; }
cpu()  { ps -o time= -p "$1" 2>/dev/null | tr -d ' '; }
st()   { awk '{print $3}' "/proc/$1/stat" 2>/dev/null; }

echo "sampling nix-daemon workers every ${INTERVAL}s, $COUNT times"
echo "(a worker unreadable here is another user's and is reported as such,"
echo " never as absent - an unreadable process is not an idle one)"
# THE CRASH COUNT IS READ BEFORE AND AFTER, because it is the population every
# CPU column here structurally misses: a worker forked, segfaulted and reaped
# inside one sample interval appears in no row, so a daemon losing every new
# connection reads exactly like an idle one. Four DaemonStalled aborts were
# attributed to load before this line existed, the last of them on a daemon
# reading 0.008x with no occupied build cgroup. Status is discarded and the
# count kept, since grep exits 1 on zero.
DLOG=${NN_DAEMON_LOG:-/var/log/nix-daemon/current}
crashes() { grep -c 'daemon worker .* crashed' "$DLOG" 2>/dev/null || true; }
CRASH_BEFORE=$(crashes); CRASH_BEFORE=${CRASH_BEFORE:-unreadable}

for i in $(seq 1 "$COUNT"); do
  echo "--- sample $i  $(date +%H:%M:%S)"
  pgrep -x nix-daemon | while IFS= read -r p; do
    s=$(st "$p")
    if [ -z "$s" ]; then
      printf '  pid %-7s UNREADABLE (not ours)\n' "$p"
      continue
    fi
    printf '  pid %-7s state %-2s cpu %-10s vctx %-10s lifetime %s\n' \
      "$p" "$s" "$(cpu "$p")" "$(vctx "$p")" "$(life "$p")"
  done
  [ "$i" = "$COUNT" ] || sleep "$INTERVAL"
done
echo
CRASH_AFTER=$(crashes); CRASH_AFTER=${CRASH_AFTER:-unreadable}
echo "worker crashes in $DLOG   before $CRASH_BEFORE   after $CRASH_AFTER"
# THE DELTA IS THE READING AND THE ABSOLUTE COUNT IS NOT. s6-log rotates this
# file by size, so the same daemon reported 29 and then 27 within an hour with
# no crash in between; only the before/after difference is a statement about
# this window.
if [ "$CRASH_BEFORE" != unreadable ] && [ "$CRASH_AFTER" != "$CRASH_BEFORE" ]; then
  echo "  WORKERS ARE CRASHING RIGHT NOW. This is not a busy daemon and no gap"
  echo "  will open. A connect SUCCEEDS and the worker dies before it answers,"
  echo "  which the driver reports as DaemonStalled with 0 connect failures."
  grep 'daemon worker .* crashed' "$DLOG" | tail -3
fi
echo
echo "THE CRASH COUNT OUTRANKS EVERY CPU COLUMN BELOW. A crash loop is invisible"
echo "to all of them: they sample surviving workers by pid, and a worker that"
echo "dies in 300 ms is never sampled. Four aborts were read as load before this"
echo "line existed, the last on a daemon idle by every other reading."
echo
echo "TWO CPU READINGS, AND NEITHER IS SUFFICIENT ALONE. The delta says what a"
echo "worker is doing in this window; the LIFETIME column is a cumulative average"
echo "and therefore a RATCHET, so it cannot fall while the worker lives and must"
echo "not be read as a veto. The column that moved when the round drained was the"
echo "THREAD COUNT, one thread per inner recursive-nix connection: 21 while the"
echo "round drove, 4 once it stopped. Read threads and occupied build cgroups"
echo "together, and take the delta over the longest span you have rather than"
echo "over one sample, since 0.12x over 50 seconds was a lull inside 1.45x."
echo
echo "IT ALSO DOES NOT IDENTIFY WHO WILL SERVE YOU. Workers are forked per"
echo "connection, so no process listed here is necessarily the one your next"
echo "client gets, and this machine has run two daemon lineages at different"
echo "nix builds at once. A quiet worker is not a free daemon."
echo
echo "READ IT AS DELTAS, never as levels: a worker with hours of CPU may be"
echo "idle now, and a worker with seconds may be the one burning the gap."
