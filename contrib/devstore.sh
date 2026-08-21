#!/usr/bin/env bash
# Iterate on nix-ninja against a throwaway store, unprivileged, without
# touching the system daemon.
#
# CONTRIBUTING.md asks for this: "If there's a good UX way of iterating on
# nix-ninja in a tmp store and without modifying your main nix, please
# contribute!" The documented alternative is to make a patched nix YOUR system
# daemon, which is a large thing to ask of a contributor and impossible on a
# shared machine.
#
#   contrib/devstore.sh selftest        # prove the store works
#   contrib/devstore.sh run -- <cmd>    # run a command against it
#   contrib/devstore.sh stop
#
# The daemon comes from this repository's own `nix` flake input, so it is the
# version this project already pins rather than one this script chose. It is
# built or substituted like any other input the first time, which on a cold
# machine can mean building nix; after that it is a store lookup. Override
# with NIX_NINJA_DEVSTORE_DAEMON=/path/to/nix-daemon.
#
# `sandbox = false` below is deliberate and is the one setting worth arguing
# about. The store lives under a user-owned directory with no build users
# group, so builds run as the invoking user and the host filesystem is
# visible to them. That is fine for iterating on nix-ninja and is NOT fine
# for producing anything anyone will consume: an unsandboxed build can link
# whatever the host happens to have. Do not push results from this store.
#
# TWO INGREDIENTS ARE REQUIRED AND BOTH FAIL BY BLAMING THE CLIENT. This is
# the part worth reading; each cost an afternoon.
#
#   1. `trusted-users` must name the connecting user in the DAEMON's conf.
#      Without it the daemon silently discards client settings: it warns about
#      restricted settings and then reports `experimental Nix feature
#      'ca-derivations' is disabled`, which reads as the CLIENT lacking the
#      feature. Passing --extra-experimental-features to the client does not
#      help, because the client already had it.
#
#   2. `--extra-experimental-features` must be on the DAEMON'S OWN ARGV. The
#      daemon does not take these from the `experimental-features` line of the
#      conf it was pointed at. The error text is identical to (1), so the two
#      are indistinguishable from the message and fixing one leaves the other.
#
# Two things concluded before those were found, recorded so nobody re-concludes
# them: that an unprivileged daemon cannot serve CA derivations, and that a
# `local?root=` store does not support ca-derivations. Both false. The obstacle
# that IS real: a daemon left on the default store cannot open the root-owned
# /nix/var/nix/db/big-lock, which is why the store has to move.
set -u

REPO=$(cd "$(dirname "$0")/.." && pwd)
DEVSTORE=${NIX_NINJA_DEVSTORE:-${XDG_CACHE_HOME:-$HOME/.cache}/nix-ninja-devstore}
FE="nix-command flakes ca-derivations dynamic-derivations recursive-nix"

resolve_daemon() {
  if [ -n "${NIX_NINJA_DEVSTORE_DAEMON:-}" ]; then
    printf '%s\n' "$NIX_NINJA_DEVSTORE_DAEMON"
    return 0
  fi
  # --impure because getFlake on a local path needs it; this is a dev tool and
  # the input is locked by flake.lock either way.
  local out p
  out=$(nix build --no-link --print-out-paths --impure \
        --extra-experimental-features "nix-command flakes" \
        --expr "(builtins.getFlake \"$REPO\").inputs.nix.packages.\${builtins.currentSystem}.default" \
        2>/dev/null) || return 1
  # A multi-output derivation prints EVERY output, one per line, and nix's own
  # package puts `-man` first. Taking the first line resolved to the man output
  # and reported "not executable" against a path that was never going to be -
  # so select by what is being looked for rather than by position.
  while IFS= read -r p; do
    [ -x "$p/bin/nix-daemon" ] && { printf '%s/bin/nix-daemon\n' "$p"; return 0; }
  done <<EOF
$out
EOF
  return 1
}

start() {
  DAEMON=$(resolve_daemon) || {
    echo "devstore: COULD-NOT-RUN - could not resolve a nix-daemon." >&2
    echo "devstore:   set NIX_NINJA_DEVSTORE_DAEMON to one." >&2
    exit 2
  }
  if [ ! -x "$DAEMON" ]; then
    echo "devstore: COULD-NOT-RUN - not executable: $DAEMON" >&2
    exit 2
  fi
  mkdir -p "$DEVSTORE/conf" "$DEVSTORE/sock" "$DEVSTORE/store"
  cat > "$DEVSTORE/conf/nix.conf" <<EOF
store = local?root=$DEVSTORE/store
build-users-group =
sandbox = false
trusted-users = $(id -un)
experimental-features = $FE
EOF
  if [ -S "$DEVSTORE/sock/socket" ] && [ -f "$DEVSTORE/pid" ] \
     && kill -0 "$(cat "$DEVSTORE/pid")" 2>/dev/null; then
    return 0
  fi
  NIX_CONF_DIR="$DEVSTORE/conf" NIX_DAEMON_SOCKET_PATH="$DEVSTORE/sock/socket" \
    "$DAEMON" --extra-experimental-features "$FE" > "$DEVSTORE/daemon.log" 2>&1 &
  echo "$!" > "$DEVSTORE/pid"
  for _ in 1 2 3 4 5 6 7 8; do
    [ -S "$DEVSTORE/sock/socket" ] && return 0
    sleep 1
  done
  echo "devstore: COULD-NOT-RUN - the daemon never opened its socket." >&2
  tail -5 "$DEVSTORE/daemon.log" >&2
  exit 2
}

stop() {
  [ -f "$DEVSTORE/pid" ] && kill "$(cat "$DEVSTORE/pid")" 2>/dev/null
  rm -f "$DEVSTORE/pid" "$DEVSTORE/sock/socket"
  echo "devstore: stopped"
}

case "${1:-}" in
  start) start; echo "devstore: up at $DEVSTORE/sock/socket" ;;
  stop)  stop ;;
  run)
    shift
    [ "${1:-}" = "--" ] && shift
    [ $# -gt 0 ] || { echo "devstore: run needs a command" >&2; exit 2; }
    start
    NIX_REMOTE="unix://$DEVSTORE/sock/socket" exec "$@"
    ;;
  selftest)
    start
    cat > "$DEVSTORE/ca.nix" <<'EOF'
derivation { name = "ca-probe"; system = builtins.currentSystem;
  builder = "/bin/sh"; args = [ "-c" "echo hi > $out" ];
  __contentAddressed = true; outputHashMode = "recursive";
  outputHashAlgo = "sha256"; }
EOF
    cat > "$DEVSTORE/rpc.nix" <<'EOF'
derivation { name = "rpc-probe"; system = builtins.currentSystem;
  builder = "/bin/sh"; args = [ "-c" "true" ];
  requiredSystemFeatures = [ "builder-rpc-v0" ];
  __contentAddressed = true; outputHashMode = "recursive";
  outputHashAlgo = "sha256"; }
EOF

    echo "devstore: probe 1 - a CA derivation must BUILD"
    if ! NIX_REMOTE="unix://$DEVSTORE/sock/socket" nix build --impure \
         --no-link --extra-experimental-features "$FE" \
         -f "$DEVSTORE/ca.nix" > "$DEVSTORE/ca.out" 2> "$DEVSTORE/ca.err"; then
      echo "devstore: FAIL - the store cannot build a CA derivation." >&2
      tail -4 "$DEVSTORE/ca.err" >&2; exit 1
    fi
    echo "devstore:   ok"

    # PROBE 2 CANNOT BE JUDGED BY EXIT STATUS. builder-rpc-v0 deliberately
    # leaves $out unset and an ordinary builder never calls SubmitOutput, so a
    # daemon that HAS the feature produces a FAILING build. nix-ninja requires
    # this feature for every dynamic task (crates/nix-ninja/src/task.rs), so a
    # daemon without it fails much later with a confusing message.
    echo "devstore: probe 2 - builder-rpc-v0 must be ADVERTISED"
    NIX_REMOTE="unix://$DEVSTORE/sock/socket" nix build --impure \
      --no-link --extra-experimental-features "$FE" \
      -f "$DEVSTORE/rpc.nix" > "$DEVSTORE/rpc.out" 2> "$DEVSTORE/rpc.err"
    if grep -q 'missing system features' "$DEVSTORE/rpc.err"; then
      echo "devstore: FAIL - this daemon does not advertise builder-rpc-v0." >&2
      grep 'Available features' "$DEVSTORE/rpc.err" >&2; exit 1
    fi
    # THE POSITIVE SIGNATURE, because the branch above is an absence test and
    # an absence test passes on every other way the run can die - daemon gone,
    # socket refused, expression malformed, probe file missing, disk full.
    # `failed to submit output path` proves the scheduler accepted the
    # derivation, the builder ran, and the daemon wanted a SubmitOutput call.
    if ! grep -q 'failed to submit output path' "$DEVSTORE/rpc.err"; then
      echo "devstore: COULD-NOT-RUN - neither signature appeared; this run" >&2
      echo "devstore:   measured nothing about builder-rpc-v0." >&2
      tail -5 "$DEVSTORE/rpc.err" >&2; exit 2
    fi
    echo "devstore:   ok (the build fails with \$out unset; that IS the feature)"
    echo "devstore: PASS - $DEVSTORE is usable for nix-ninja iteration"
    ;;
  *) sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;;
esac
